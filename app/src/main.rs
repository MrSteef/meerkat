mod config;
mod runtime;

use std::fs::OpenOptions;
use std::sync::Arc;

use anyhow::{Context, Result};
use meerkat_core::QueryExecutor;
use meerkat_oracle::OracleExecutor;
use meerkat_store::Store;
use meerkat_web::WebState;
use meerkat_webhook::Sender;
use tokio::sync::Semaphore;
use tracing::info;
use uuid::Uuid;

fn main() -> Result<()> {
    // dotenvy updates process environment variables; do this before Tokio
    // starts worker threads that might read the environment concurrently.
    config::load_local_env()?;
    run()
}

#[tokio::main]
async fn run() -> Result<()> {
    let config = config::Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .map_err(|error| anyhow::anyhow!("Setting up tracing: {error}"))?;
    let sqlite_path = &config.sqlite_path;
    // Lifetime-long OS file lock: avoid a second process interrupting the
    // first process's active outbound requests.
    let lock_path = sqlite_path.with_extension("sqlite.app-lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Could not create lock {}", lock_path.display()))?;
    lock_file
        .try_lock()
        .context("Another Meerkat instance may be using this database")?;

    let store = Store::open(sqlite_path)
        .await
        .context("Opening SQLite store")?;
    store
        .recover_on_start()
        .await
        .context("Recovering previous session state")?;
    let executor: Arc<dyn QueryExecutor> = Arc::new(OracleExecutor::start(config.oracle)?);
    let capacity = Arc::new(Semaphore::new(1));
    let sender = Sender::new(config.webhook_timeout)?;
    let session_id = Uuid::new_v4().to_string();
    let bind = config.bind;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let web_state = WebState {
        store: store.clone(),
        executor: Arc::clone(&executor),
        query_capacity: Arc::clone(&capacity),
    };
    let server = axum::serve(listener, meerkat_web::router(web_state));
    info!(%bind, "Meerkat listening");

    let runner = tokio::spawn(runtime::coordinator(
        store.clone(),
        executor,
        capacity,
        session_id.clone(),
    ));
    let dispatcher = tokio::spawn(runtime::dispatch(store, sender, config.retry, session_id));
    tokio::select! {
        server_result = server => server_result.context("Web server stopped")?,
        signal = tokio::signal::ctrl_c() => { signal.context("Signal handler")?; info!("Shutdown requested"); },
    }
    runner.abort();
    dispatcher.abort();
    let _ = runner.await;
    let _ = dispatcher.await;
    // Keep lock_file alive until all tasks are stopped.
    drop(lock_file);
    Ok(())
}
