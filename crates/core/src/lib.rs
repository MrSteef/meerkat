//! Database-independent query results, query validation, and keyed rising edges.

mod detection;
mod sql_validation;

pub use detection::{Detection, DetectionError, evaluate};
pub use sql_validation::validate_read_only_sql;

use async_trait::async_trait;
use serde_json::{Map, Value};

/// A row is keyed by a result column name. Oracle-specific types are normalized
/// by the connector before they cross this boundary.
pub type QueryRow = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<QueryRow>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct QueryFailure {
    pub message: String,
}

#[async_trait]
pub trait QueryExecutor: Send + Sync {
    async fn execute(&self, sql: &str) -> Result<QueryResult, QueryFailure>;
}
