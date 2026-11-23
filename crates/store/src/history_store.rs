//! Read-only history queries; all optional filter values are bound parameters.

use sqlx::{QueryBuilder, Sqlite};

use crate::model::{Attempt, HistoryFilter, Message, Run};
use crate::{Store, StoreError};

const RUNS_SELECT: &str =
    "SELECT r.*, f.name AS flow_name FROM runs r JOIN flows f ON f.id=r.flow_id";
const MESSAGES_SELECT: &str = "SELECT m.*, r.flow_id, f.name AS flow_name,
 (SELECT count(*) FROM webhook_attempts a WHERE a.message_id=m.id) AS attempt_count
 FROM webhook_messages m JOIN runs r ON r.id=m.run_id JOIN flows f ON f.id=r.flow_id";
const ATTEMPTS_SELECT: &str = "SELECT a.*, m.run_id, r.flow_id, f.name AS flow_name
 FROM webhook_attempts a JOIN webhook_messages m ON m.id=a.message_id
 JOIN runs r ON r.id=m.run_id JOIN flows f ON f.id=r.flow_id";

impl Store {
    /// # Errors
    /// Returns SQLite errors.
    pub async fn runs(&self, f: &HistoryFilter) -> Result<Vec<Run>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(RUNS_SELECT);
        query
            .push(" WHERE (")
            .push_bind(f.flow_id)
            .push(" IS NULL OR r.flow_id=")
            .push_bind(f.flow_id)
            .push(") ORDER BY r.id DESC LIMIT 50 OFFSET ")
            .push_bind(f.page.max(0).saturating_mul(50));
        Ok(query.build_query_as::<Run>().fetch_all(&self.pool).await?)
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn run(&self, id: i64) -> Result<Option<Run>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(RUNS_SELECT);
        query.push(" WHERE r.id=").push_bind(id);
        Ok(query
            .build_query_as::<Run>()
            .fetch_optional(&self.pool)
            .await?)
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn messages(&self, f: &HistoryFilter) -> Result<Vec<Message>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(MESSAGES_SELECT);
        query
            .push(" WHERE (")
            .push_bind(f.flow_id)
            .push(" IS NULL OR r.flow_id=")
            .push_bind(f.flow_id)
            .push(") AND (")
            .push_bind(f.run_id)
            .push(" IS NULL OR m.run_id=")
            .push_bind(f.run_id)
            .push(") AND (")
            .push_bind(f.status.clone())
            .push(" IS NULL OR m.status=")
            .push_bind(f.status.clone())
            .push(") ORDER BY m.created_at DESC, m.id DESC LIMIT 50 OFFSET ")
            .push_bind(f.page.max(0).saturating_mul(50));
        Ok(query
            .build_query_as::<Message>()
            .fetch_all(&self.pool)
            .await?)
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn message(&self, id: &str) -> Result<Option<Message>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(MESSAGES_SELECT);
        query.push(" WHERE m.id=").push_bind(id);
        Ok(query
            .build_query_as::<Message>()
            .fetch_optional(&self.pool)
            .await?)
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn attempts(&self, f: &HistoryFilter) -> Result<Vec<Attempt>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(ATTEMPTS_SELECT);
        query
            .push(" WHERE (")
            .push_bind(f.flow_id)
            .push(" IS NULL OR r.flow_id=")
            .push_bind(f.flow_id)
            .push(") AND (")
            .push_bind(f.message_id.clone())
            .push(" IS NULL OR a.message_id=")
            .push_bind(f.message_id.clone())
            .push(") ORDER BY a.id DESC LIMIT 50 OFFSET ")
            .push_bind(f.page.max(0).saturating_mul(50));
        Ok(query
            .build_query_as::<Attempt>()
            .fetch_all(&self.pool)
            .await?)
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn message_attempts(&self, message_id: &str) -> Result<Vec<Attempt>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(ATTEMPTS_SELECT);
        query
            .push(" WHERE a.message_id=")
            .push_bind(message_id)
            .push(" ORDER BY a.id DESC LIMIT 100");
        Ok(query
            .build_query_as::<Attempt>()
            .fetch_all(&self.pool)
            .await?)
    }
}
