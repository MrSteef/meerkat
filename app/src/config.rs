//! Parse startup configuration once, regardless of where the environment variables originate.
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use meerkat_oracle::OracleConfig;

use crate::runtime::RetryConfig;

/// Load local settings without requiring a `.env` file in deployed environments.
///
/// # Errors
/// Returns an error if a discovered `.env` file cannot be read or parsed.
pub fn load_local_env() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(error) if error.not_found() => Ok(()),
        // A syntax error can contain the original line, including a password.
        Err(dotenvy::Error::LineParse(_, _)) => {
            Err(anyhow::anyhow!("Invalid syntax in .env (contents hidden)"))
        }
        Err(error) => Err(error).context("Loading .env file"),
    }
}

/// Validated, process-wide startup settings. Credentials are not exposed by `Debug`.
pub struct Config {
    pub sqlite_path: PathBuf,
    pub bind: SocketAddr,
    pub oracle: OracleConfig,
    pub retry: RetryConfig,
    pub webhook_timeout: Duration,
}

impl Config {
    /// Read configuration from the environment, including an already-loaded `.env`.
    ///
    /// # Errors
    /// Returns an error for missing credentials or invalid settings.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(&|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            // Avoid including credential values in startup error messages.
            Err(env::VarError::NotUnicode(_)) => Err(anyhow::anyhow!(
                "Environment variable {name} contains invalid Unicode"
            )),
        })
    }

    fn from_lookup(get: &impl Fn(&str) -> Result<Option<String>>) -> Result<Self> {
        let oracle = OracleConfig {
            descriptor: required(get, "ORACLE_CONNECTION_STRING")?,
            username: required(get, "ORACLE_USERNAME")?,
            password: required(get, "ORACLE_PASSWORD")?,
            timeout: Duration::from_secs(positive(get, "ORACLE_CALL_TIMEOUT_SECONDS", 45)?),
            max_rows: usize::try_from(positive(get, "ORACLE_MAX_ROWS", 2000)?)
                .context("ORACLE_MAX_ROWS too large")?,
        };
        let retry = RetryConfig {
            max_retries: u32::try_from(number(get, "WEBHOOK_MAX_RETRIES", 5)?)
                .context("Too many retries")?,
            initial_delay_seconds: i64::try_from(positive(
                get,
                "WEBHOOK_RETRY_INITIAL_SECONDS",
                30,
            )?)
            .context("Retry delay too large")?,
            max_delay_seconds: i64::try_from(positive(
                get,
                "WEBHOOK_RETRY_MAX_DELAY_SECONDS",
                1800,
            )?)
            .context("Retry delay too large")?,
        };
        retry.validate()?;

        let bind: SocketAddr = optional(get, "MEERKAT_BIND", "127.0.0.1:3000")?
            .parse()
            .context("Invalid MEERKAT_BIND socket address")?;
        // The V1 configuration UI has no authentication.
        ensure!(
            bind.ip().is_loopback(),
            "MEERKAT_BIND must be a loopback address in V1"
        );

        Ok(Self {
            sqlite_path: PathBuf::from(optional(get, "MEERKAT_SQLITE_PATH", "./meerkat.sqlite")?),
            bind,
            oracle,
            retry,
            webhook_timeout: Duration::from_secs(positive(get, "WEBHOOK_TIMEOUT_SECONDS", 15)?),
        })
    }
}

fn required(get: &impl Fn(&str) -> Result<Option<String>>, name: &str) -> Result<String> {
    get(name)?.with_context(|| format!("Missing required environment variable {name}"))
}

fn optional(
    get: &impl Fn(&str) -> Result<Option<String>>,
    name: &str,
    fallback: &str,
) -> Result<String> {
    Ok(get(name)?.unwrap_or_else(|| fallback.to_owned()))
}

fn positive(
    get: &impl Fn(&str) -> Result<Option<String>>,
    name: &str,
    fallback: u64,
) -> Result<u64> {
    let value = number(get, name, fallback)?;
    ensure!(value > 0, "{name} must be positive");
    Ok(value)
}

fn number(get: &impl Fn(&str) -> Result<Option<String>>, name: &str, fallback: u64) -> Result<u64> {
    get(name)?.map_or_else(|| Ok(fallback), |value| value
            .parse::<u64>()
            .with_context(|| format!("Invalid integer in {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_environment(name: &str) -> Option<String> {
        let value = match name {
            "ORACLE_CONNECTION_STRING" => Some("example:1521/service"),
            "ORACLE_USERNAME" => Some("reader"),
            "ORACLE_PASSWORD" => Some("secret"),
            _ => None,
        };
        value.map(str::to_owned)
    }

    #[test]
    fn defaults_keep_the_web_ui_local() -> Result<()> {
        let config = Config::from_lookup(&|name| Ok(minimal_environment(name)))?;
        ensure!(config.bind.ip().is_loopback());
        ensure!(config.bind.port() == 3000);
        ensure!(config.oracle.timeout == Duration::from_secs(45));
        ensure!(config.oracle.max_rows == 2000);
        ensure!(config.retry.max_retries == 5);
        ensure!(config.webhook_timeout == Duration::from_secs(15));
        Ok(())
    }

    #[test]
    fn rejects_nonlocal_bind_address() {
        let config = Config::from_lookup(&|name| {
            if name == "MEERKAT_BIND" {
                return Ok(Some("0.0.0.0:3000".to_owned()));
            }
            Ok(minimal_environment(name))
        });
        assert!(config.is_err());
    }

    #[test]
    fn rejects_invalid_retry_settings() {
        let config = Config::from_lookup(&|name| {
            if name == "WEBHOOK_RETRY_MAX_DELAY_SECONDS" {
                return Ok(Some("10".to_owned()));
            }
            Ok(minimal_environment(name))
        });
        assert!(config.is_err());
    }

    #[test]
    fn requires_oracle_credentials() {
        let config = Config::from_lookup(&|_| Ok(None));
        assert!(config.is_err());
    }
}
