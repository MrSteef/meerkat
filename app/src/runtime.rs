//! One capacity-driven coordinator and an independent SQLite-backed dispatcher.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use meerkat_core::{QueryExecutor, evaluate};
use meerkat_schedule::{WeeklySchedule, Window, is_due};
use meerkat_store::Store;
use meerkat_store::model::{Flow, RetryPolicy, RunCounts};
use meerkat_webhook::Sender;
use tokio::sync::Semaphore;
use tracing::{error, warn};

pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_delay_seconds: i64,
    pub max_delay_seconds: i64,
}
impl RetryConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.initial_delay_seconds > 0,
            "WEBHOOK_RETRY_INITIAL_SECONDS must be positive"
        );
        ensure!(
            self.max_delay_seconds >= self.initial_delay_seconds,
            "Maximum delay must be at least the initial delay"
        );
        Ok(())
    }
}

fn schedule_of(flow: &Flow) -> WeeklySchedule {
    WeeklySchedule {
        id: flow.schedule_id,
        weekday: u32::try_from(flow.weekday).unwrap_or(0),
        start_time: flow.start_time.clone(),
        end_time: flow.end_time.clone(),
        timezone: flow.timezone.clone(),
        interval_seconds: flow.poll_interval_seconds,
        completed_window: flow.completed_window.clone(),
        last_started_at: flow
            .last_started_at
            .as_deref()
            .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
            .map(|time| time.with_timezone(&Utc)),
    }
}

fn select_due(flows: Vec<Flow>, now: DateTime<Utc>) -> Option<(Flow, Window)> {
    let mut candidates: Vec<_> = flows
        .into_iter()
        .filter(|f| f.enabled != 0)
        .filter_map(|f| is_due(&schedule_of(&f), now).map(|window| (f, window)))
        .collect();
    // Oldest last start first, so continuously-due flows do not starve others.
    candidates.sort_by(|(left, _), (right, _)| {
        left.last_started_at
            .cmp(&right.last_started_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.into_iter().next()
}

/// Orchestrate one Oracle run at a time. No queued stale polls.
pub async fn coordinator(
    store: Store,
    executor: Arc<dyn QueryExecutor>,
    capacity: Arc<Semaphore>,
    session_id: String,
) {
    loop {
        if let Ok(_permit) = Arc::clone(&capacity).try_acquire_owned() {
            match store.list_flows().await {
                Ok(flows) => {
                    if let Some((flow, window)) = select_due(flows, Utc::now()) {
                        execute_one(&store, executor.as_ref(), &flow, &window, &session_id).await;
                        continue;
                    }
                }
                Err(error) => error!(%error, "Failed to load due flows"),
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn execute_one(
    store: &Store,
    executor: &dyn QueryExecutor,
    flow: &Flow,
    window: &Window,
    session_id: &str,
) {
    let run_id = match store.start_run(flow, Utc::now()).await {
        Ok(id) => id,
        Err(error) => {
            warn!(flow_id=flow.id, %error, "Cannot start flow");
            return;
        }
    };
    let result = executor.execute(&flow.sql_query).await;
    match result {
        Err(error) => {
            warn!(run_id, %error, "Oracle query failed");
            if let Err(store_error) = store.fail_run(run_id, "failed", &error.message).await {
                error!(%store_error, "Could not record failed query");
            }
        }
        Ok(rows) => {
            let parsed = (|| -> Result<_, String> {
                let keys: Vec<String> =
                    serde_json::from_str(&flow.key_columns).map_err(|e| e.to_string())?;
                let previous: Vec<String> =
                    serde_json::from_str(&flow.previous_keys).map_err(|e| e.to_string())?;
                evaluate(
                    &rows,
                    &keys,
                    &previous,
                    flow.baseline_initialized != 0,
                    flow.trigger_on_first_run != 0,
                )
                .map_err(|e| e.to_string())
            })();
            match parsed {
                Err(error) => {
                    warn!(run_id, %error, "Evaluation failed; baseline preserved");
                    if let Err(store_error) = store.fail_run(run_id, "failed", &error).await {
                        error!(%store_error, "Could not record evaluation error");
                    }
                }
                Ok(detection) => {
                    match store
                        .commit_run(
                            flow,
                            run_id,
                            &window.id,
                            &detection.current_keys,
                            RunCounts {
                                row_count: rows.rows.len(),
                                new_key_count: detection.new_key_count,
                            },
                            session_id,
                        )
                        .await
                    {
                        Ok(()) => (),
                        Err(meerkat_store::StoreError::StaleRevision) => {
                            warn!(run_id, "Configuration changed; results discarded");
                        }
                        Err(error) => {
                            error!(run_id, %error, "Evaluation transaction failed");
                            if let Err(second) = store
                                .fail_run(run_id, "failed", &format!("SQLite commit: {error}"))
                                .await
                            {
                                error!(%second, "Also could not record commit error");
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Continuously process only this application's messages. Never consult
/// originating flow status or query execution state.
pub async fn dispatch(store: Store, sender: Sender, retry: RetryConfig, session_id: String) {
    loop {
        match store.start_attempt(&session_id).await {
            Ok(Some(started)) => {
                let outcome = sender
                    .send(
                        &started.message.id,
                        &started.message.url,
                        &started.message.headers,
                        &started.message.payload,
                    )
                    .await;
                if let Err(error) = store
                    .finish_attempt(
                        started.attempt_id,
                        &started.message.id,
                        outcome.status,
                        outcome.error.as_deref(),
                        RetryPolicy {
                            max_retries: retry.max_retries,
                            initial_delay_seconds: retry.initial_delay_seconds,
                            max_delay_seconds: retry.max_delay_seconds,
                        },
                    )
                    .await
                {
                    error!(%error, "Could not persist webhook attempt outcome");
                }
            }
            Ok(None) => tokio::time::sleep(Duration::from_secs(1)).await,
            Err(error) => {
                error!(%error, "Outbox query failed");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use meerkat_core::{QueryFailure, QueryResult, QueryRow};
    use meerkat_store::model::{FlowInput, HistoryFilter};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct FakeExecutor {
        results: Mutex<VecDeque<QueryResult>>,
    }

    #[async_trait]
    impl QueryExecutor for FakeExecutor {
        async fn execute(&self, _sql: &str) -> Result<QueryResult, QueryFailure> {
            self.results
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| QueryFailure {
                    message: "No fake query result available".to_owned(),
                })
        }
    }
    fn query_result(shift: Option<&str>) -> QueryResult {
        let rows = shift.map_or_else(Vec::new, |id| {
            let mut row = QueryRow::new();
            row.insert("SHIFT_ID".to_owned(), json!(id));
            vec![row]
        });
        QueryResult {
            columns: vec!["SHIFT_ID".to_owned()],
            rows,
        }
    }
    fn config() -> FlowInput {
        FlowInput {
            name: "Saturday readiness".to_owned(),
            enabled: true,
            sql_query: "SELECT SHIFT_ID FROM waves".to_owned(),
            key_columns: vec!["SHIFT_ID".to_owned()],
            trigger_on_first_run: true,
            weekday: 6,
            start_time: "13:00".to_owned(),
            end_time: "19:00".to_owned(),
            timezone: "Europe/Amsterdam".to_owned(),
            poll_interval_seconds: 60,
            stop_after_detection: true,
            url: "http://127.0.0.1:8080/hook".to_owned(),
            headers: "{}".to_owned(),
            payload_template: "{\"text\":\"All waves are active\"}".to_owned(),
        }
    }
    #[tokio::test]
    async fn query_results_create_one_message_and_complete_window() {
        let outcome: Result<bool, Box<dyn std::error::Error>> = async {
            let store = Store::open(":memory:").await?;
            let id = store.create_flow(&config()).await?;
            let fake = FakeExecutor {
                results: Mutex::new(VecDeque::from([
                    query_result(None),
                    query_result(Some("2026-11-21-A")),
                ])),
            };
            let window = Window {
                id: "window-test".to_owned(),
                start: Utc::now(),
                end: Utc::now(),
            };
            let first = store.flow(id).await?.ok_or("Missing first flow")?;
            execute_one(&store, &fake, &first, &window, "test-session").await;
            let next = store.flow(id).await?.ok_or("Missing second flow")?;
            execute_one(&store, &fake, &next, &window, "test-session").await;
            let updated = store.flow(id).await?.ok_or("Missing updated flow")?;
            let runs = store.runs(&HistoryFilter::default()).await?;
            let messages = store.messages(&HistoryFilter::default()).await?;
            let valid = updated.completed_window.as_deref() == Some("window-test")
                && updated.baseline_initialized == 1
                && runs.len() == 2
                && runs.first().and_then(|r| r.new_key_count) == Some(1)
                && messages.len() == 1
                && messages.first().map(|m| m.status.as_str()) == Some("pending");
            let _owned: Arc<dyn QueryExecutor> = Arc::new(fake);
            Ok(valid)
        }
        .await;
        assert!(outcome.as_ref().is_ok_and(|value| *value), "{outcome:?}");
    }
}
