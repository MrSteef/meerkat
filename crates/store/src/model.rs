//! SQLite records and write-side configuration. Timestamp columns store RFC3339 UTC.
use sqlx::FromRow;

#[derive(Clone, Debug, FromRow)]
pub struct Flow {
    pub id: i64,
    pub name: String,
    pub enabled: i64,
    pub sql_query: String,
    pub key_columns: String,
    pub trigger_on_first_run: i64,
    pub revision: i64,
    pub schedule_id: i64,
    pub weekday: i64,
    pub start_time: String,
    pub end_time: String,
    pub timezone: String,
    pub poll_interval_seconds: i64,
    pub stop_after_detection: i64,
    pub last_started_at: Option<String>,
    pub completed_window: Option<String>,
    pub webhook_id: i64,
    pub url: String,
    pub headers: String,
    pub payload_template: String,
    pub baseline_initialized: i64,
    pub previous_keys: String,
    pub last_run_status: Option<String>,
    pub last_run_started_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FlowInput {
    pub name: String,
    pub enabled: bool,
    pub sql_query: String,
    pub key_columns: Vec<String>,
    pub trigger_on_first_run: bool,
    pub weekday: i64,
    pub start_time: String,
    pub end_time: String,
    pub timezone: String,
    pub poll_interval_seconds: i64,
    pub stop_after_detection: bool,
    pub url: String,
    pub headers: String,
    pub payload_template: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct Run {
    pub id: i64,
    pub flow_id: i64,
    pub flow_name: String,
    pub schedule_id: Option<i64>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub row_count: Option<i64>,
    pub new_key_count: Option<i64>,
    pub message_count: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Message {
    pub id: String,
    pub run_id: i64,
    pub flow_id: i64,
    pub flow_name: String,
    pub webhook_id: i64,
    pub session_id: String,
    pub created_at: String,
    pub url: String,
    pub headers: String,
    pub payload: String,
    pub status: String,
    pub next_attempt_at: Option<String>,
    pub attempt_count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct Attempt {
    pub id: i64,
    pub message_id: String,
    pub run_id: i64,
    pub flow_id: i64,
    pub flow_name: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: String,
    pub http_status: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct StartedAttempt {
    pub attempt_id: i64,
    pub message: Message,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    pub flow_id: Option<i64>,
    pub run_id: Option<i64>,
    pub message_id: Option<String>,
    pub status: Option<String>,
    pub page: i64,
}

/// Counts recorded for one successful query evaluation.
#[derive(Clone, Copy, Debug)]
pub struct RunCounts {
    pub row_count: usize,
    pub new_key_count: usize,
}

/// Application-wide retry limits applied when recording an attempt.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_delay_seconds: i64,
    pub max_delay_seconds: i64,
}
