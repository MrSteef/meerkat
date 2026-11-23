//! A single long-lived Oracle worker thread. Worker capacity can be increased
//! later without changing the `QueryExecutor` contract.
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDateTime, SecondsFormat};
use meerkat_core::{QueryExecutor, QueryFailure, QueryResult, QueryRow};
use oracle::Connection;
use oracle::sql_type::OracleType;
use serde_json::{Number, Value};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub struct OracleConfig {
    pub descriptor: String,
    pub username: String,
    pub password: String,
    pub timeout: Duration,
    pub max_rows: usize,
}

/// The password deliberately isn't exposed by Debug.
impl std::fmt::Debug for OracleConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OracleConfig")
            .field("descriptor", &self.descriptor)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("timeout", &self.timeout)
            .field("max_rows", &self.max_rows)
            .finish()
    }
}

struct Request {
    sql: String,
    reply: oneshot::Sender<Result<QueryResult, QueryFailure>>,
}

pub struct OracleExecutor {
    tx: mpsc::Sender<Request>,
}

impl OracleExecutor {
    /// Start a dedicated blocking thread that owns a single Oracle session.
    ///
    /// # Errors
    /// Fails if the operating system cannot spawn the worker.
    pub fn start(config: OracleConfig) -> std::io::Result<Self> {
        let (tx, mut rx) = mpsc::channel::<Request>(1);

        std::thread::Builder::new()
            .name("meerkat-oracle".to_owned())
            .spawn(move || {
                let mut connection: Option<Connection> = None;

                while let Some(request) = rx.blocking_recv() {
                    if connection.is_none() {
                        match Connection::connect(
                            &config.username,
                            &config.password,
                            &config.descriptor,
                        ) {
                            Ok(opened) => {
                                // Configure the session before using it.
                                if let Err(error) = opened.set_current_schema("LOCUS") {
                                    let _ = request.reply.send(Err(failure(format!(
                                        "Oracle session preparation failed: {error}"
                                    ))));
                                    continue;
                                }

                                connection = Some(opened);
                            }
                            Err(error) => {
                                let _ = request.reply.send(Err(failure(format!(
                                    "Oracle connection failed: {error}"
                                ))));
                                continue;
                            }
                        }
                    }

                    let result = match connection.as_ref() {
                        Some(active) => read_only_query(active, &request.sql, &config),
                        None => Err(failure("Oracle connection unavailable".to_owned())),
                    };

                    // A timed-out or broken connection is not reused.
                    if result.is_err() {
                        connection = None;
                    }

                    let _ = request.reply.send(result);
                }
            })?;

        Ok(Self { tx })
    }
}

#[async_trait]
impl QueryExecutor for OracleExecutor {
    async fn execute(&self, sql: &str) -> Result<QueryResult, QueryFailure> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Request {
                sql: sql.to_owned(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| failure("Oracle worker is unavailable".to_owned()))?;
        reply_rx
            .await
            .map_err(|_| failure("Oracle worker stopped unexpectedly".to_owned()))?
    }
}

const fn failure(message: String) -> QueryFailure {
    QueryFailure { message }
}

fn read_only_query(
    connection: &Connection,
    sql: &str,
    config: &OracleConfig,
) -> Result<QueryResult, QueryFailure> {
    connection
        .set_call_timeout(Some(config.timeout))
        .map_err(|e| failure(e.to_string()))?;

    fetch_rows(connection, sql, config.max_rows)
}

fn fetch_rows(
    connection: &Connection,
    sql: &str,
    max_rows: usize,
) -> Result<QueryResult, QueryFailure> {
    let mut stmt = connection
        .statement(sql)
        .fetch_array_size(100)
        .build()
        .map_err(|e| failure(e.to_string()))?;
    let result = stmt.query(&[]).map_err(|e| failure(e.to_string()))?;
    let columns = result
        .column_info()
        .iter()
        .map(|info| info.name().to_owned())
        .collect();
    let mut rows = Vec::new();
    for row in result {
        let row = row.map_err(|e| failure(e.to_string()))?;
        if rows.len() >= max_rows {
            return Err(failure(format!(
                "Query returned more than {max_rows} rows; baseline was not updated"
            )));
        }
        rows.push(convert_row(&row).map_err(|e| failure(e.to_string()))?);
    }
    Ok(QueryResult { columns, rows })
}

fn convert_row(row: &oracle::Row) -> oracle::Result<QueryRow> {
    let mut output = QueryRow::new();
    for (position, column) in row.column_info().iter().enumerate() {
        output.insert(
            column.name().to_owned(),
            convert_cell(row, position, column.oracle_type())?,
        );
    }
    Ok(output)
}

fn convert_cell(row: &oracle::Row, index: usize, kind: &OracleType) -> oracle::Result<Value> {
    Ok(match kind {
        OracleType::Number(_, _)
        | OracleType::Float(_)
        | OracleType::Int64
        | OracleType::UInt64 => row
            .get::<_, Option<String>>(index)?
            .map_or(Value::Null, |text| numeric_value(&text)),
        OracleType::BinaryFloat | OracleType::BinaryDouble => row
            .get::<_, Option<f64>>(index)?
            .and_then(Number::from_f64)
            .map_or(Value::Null, Value::Number),
        OracleType::Date | OracleType::Timestamp(_) => row
            .get::<_, Option<NaiveDateTime>>(index)?
            .map_or(Value::Null, |v| {
                Value::String(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string())
            }),
        OracleType::TimestampTZ(_) | OracleType::TimestampLTZ(_) => row
            .get::<_, Option<DateTime<FixedOffset>>>(index)?
            .map_or(Value::Null, |v| {
                Value::String(v.to_rfc3339_opts(SecondsFormat::AutoSi, true))
            }),
        OracleType::Boolean => row
            .get::<_, Option<bool>>(index)?
            .map_or(Value::Null, Value::Bool),
        _ => row
            .get::<_, Option<String>>(index)?
            .map_or(Value::Null, Value::String),
    })
}

fn numeric_value(text: &str) -> Value {
    let trimmed = text.trim();
    if let Ok(number) = trimmed.parse::<i64>() {
        return Value::Number(number.into());
    }
    if let Ok(number) = trimmed.parse::<u64>() {
        return Value::Number(number.into());
    }
    if let Ok(number) = trimmed.parse::<f64>()
        && let Some(json_number) = Number::from_f64(number)
    {
        return Value::Number(json_number);
    }
    Value::String(trimmed.to_owned())
}
