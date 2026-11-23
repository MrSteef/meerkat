//! Atomic request-start and request-completion state transitions.
use crate::model::{RetryPolicy, StartedAttempt};
use crate::{Store, StoreError};
use chrono::{Duration, Utc};
use sqlx::Row;

impl Store {
    /// Select one eligible message from this session, mark it as sending,
    /// and record the in-progress attempt *before* network I/O.
    ///
    /// # Errors
    /// Returns SQLite errors.
    pub async fn start_attempt(
        &self,
        session_id: &str,
    ) -> Result<Option<StartedAttempt>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT id FROM webhook_messages WHERE session_id=? AND status='pending' AND next_attempt_at<=? ORDER BY next_attempt_at, created_at LIMIT 1")
            .bind(session_id).bind(Utc::now().to_rfc3339()).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: String = row.try_get("id")?;
        let changed = sqlx::query(
            "UPDATE webhook_messages SET status='sending' WHERE id=? AND status='pending'",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        let attempt = sqlx::query(
            "INSERT INTO webhook_attempts(message_id,started_at,outcome) VALUES(?,?,'in_progress')",
        )
        .bind(&id)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let message = self
            .message(&id)
            .await?
            .ok_or_else(|| StoreError::Validation("message vanished after claim".to_owned()))?;
        Ok(Some(StartedAttempt {
            attempt_id: attempt.last_insert_rowid(),
            message,
        }))
    }

    /// Finalize a network attempt and its message as one transaction.
    /// A failed attempt may remain pending until its retry time.
    ///
    /// # Errors
    /// Returns SQLite errors.
    pub async fn finish_attempt(
        &self,
        id: i64,
        message_id: &str,
        response_status: Option<u16>,
        error: Option<&str>,
        retry: RetryPolicy,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let current =
            sqlx::query("SELECT outcome FROM webhook_attempts WHERE id=? AND message_id=?")
                .bind(id)
                .bind(message_id)
                .fetch_optional(&mut *tx)
                .await?;
        if current
            .as_ref()
            .and_then(|r| r.try_get::<String, _>("outcome").ok())
            .as_deref()
            != Some("in_progress")
        {
            return Err(StoreError::Validation("attempt not in progress".to_owned()));
        }
        let success = response_status.is_some_and(|code| (200..300).contains(&code));
        let outcome = if success { "succeeded" } else { "failed" };
        sqlx::query("UPDATE webhook_attempts SET outcome=?, finished_at=?, http_status=?, error=? WHERE id=?")
            .bind(outcome).bind(Utc::now().to_rfc3339()).bind(response_status.map(i64::from))
            .bind(error).bind(id).execute(&mut *tx).await?;
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webhook_attempts WHERE message_id=?")
                .bind(message_id)
                .fetch_one(&mut *tx)
                .await?;
        let (status, next) = if success {
            ("delivered", None)
        } else if count > i64::from(retry.max_retries) {
            ("exhausted", None)
        } else {
            let exponent = u32::try_from(count.saturating_sub(1))
                .unwrap_or(u32::MAX)
                .min(30);
            let factor = 2_i64.checked_pow(exponent).unwrap_or(i64::MAX);
            let delay = retry
                .initial_delay_seconds
                .saturating_mul(factor)
                .min(retry.max_delay_seconds);
            let due = Utc::now()
                .checked_add_signed(Duration::seconds(delay))
                .ok_or_else(|| StoreError::Validation("retry date overflow".to_owned()))?;
            ("pending", Some(due.to_rfc3339()))
        };
        sqlx::query("UPDATE webhook_messages SET status=?, next_attempt_at=? WHERE id=? AND status='sending'")
            .bind(status).bind(next).bind(message_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use crate::model::{FlowInput, HistoryFilter, RetryPolicy, RunCounts};

    fn input() -> FlowInput {
        FlowInput {
            name: "Shift readiness".to_owned(),
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
            payload_template: "{\"message\":\"Ready\"}".to_owned(),
        }
    }

    #[tokio::test]
    async fn startup_abandons_queued_requests() {
        let outcome: Result<bool, Box<dyn std::error::Error>> = async {
            let store = Store::open(":memory:").await?;
            let flow_id = store.create_flow(&input()).await?;
            let flow = store.flow(flow_id).await?.ok_or("Flow missing")?;
            let run_id = store.start_run(&flow, chrono::Utc::now()).await?;
            store
                .commit_run(
                    &flow,
                    run_id,
                    "test",
                    &["[1]".to_owned()],
                    RunCounts {
                        row_count: 1,
                        new_key_count: 1,
                    },
                    "session-1",
                )
                .await?;
            store.recover_on_start().await?;
            let cannot_send = store.start_attempt("session-2").await?.is_none();
            let messages = store.messages(&HistoryFilter::default()).await?;
            Ok(cannot_send && messages.first().map(|m| m.status.as_str()) == Some("interrupted"))
        }
        .await;
        assert!(outcome.as_ref().is_ok_and(|value| *value), "{outcome:?}");
    }

    #[tokio::test]
    async fn outbox_survives_flow_deletion_and_retry_records_attempts() {
        let outcome: Result<bool, Box<dyn std::error::Error>> = async {
            let store = Store::open(":memory:").await?;
            let id = store.create_flow(&input()).await?;
            let flow = store.flow(id).await?.ok_or("Flow missing")?;
            let run_id = store.start_run(&flow, chrono::Utc::now()).await?;
            store
                .commit_run(
                    &flow,
                    run_id,
                    "test-window",
                    &["[\"shift-1\"]".to_owned()],
                    RunCounts {
                        row_count: 1,
                        new_key_count: 1,
                    },
                    "session",
                )
                .await?;
            store.delete_flow(id).await?;
            let started = store
                .start_attempt("session")
                .await?
                .ok_or("Expected pending delivery")?;
            let right_payload = started.message.payload == "{\"message\":\"Ready\"}";
            store
                .finish_attempt(
                    started.attempt_id,
                    &started.message.id,
                    Some(503),
                    Some("HTTP 503"),
                    RetryPolicy {
                        max_retries: 2,
                        initial_delay_seconds: 1,
                        max_delay_seconds: 60,
                    },
                )
                .await?;
            let pending = store
                .message(&started.message.id)
                .await?
                .ok_or("Message not found")?;
            let pending_is_right = pending.status == "pending" && pending.attempt_count == 1;
            store.recover_on_start().await?;
            let abandoned = store
                .message(&started.message.id)
                .await?
                .ok_or("Message not found")?;
            let cannot_resend = store.start_attempt("new-session").await?.is_none();
            let attempts = store
                .attempts(&crate::model::HistoryFilter {
                    message_id: Some(started.message.id),
                    ..Default::default()
                })
                .await?;
            Ok(right_payload
                && pending_is_right
                && abandoned.status == "interrupted"
                && cannot_resend
                && attempts.len() == 1
                && attempts.first().map(|a| a.outcome.as_str()) == Some("failed"))
        }
        .await;
        assert!(outcome.as_ref().is_ok_and(|value| *value), "{outcome:?}");
    }

    #[tokio::test]
    async fn revision_fence_preserves_new_configuration_baseline() {
        let outcome: Result<bool, Box<dyn std::error::Error>> = async {
            let store = Store::open(":memory:").await?;
            let id = store.create_flow(&input()).await?;
            let old_flow = store.flow(id).await?.ok_or("Flow missing")?;
            let run_id = store.start_run(&old_flow, chrono::Utc::now()).await?;
            let mut edited = input();
            edited.sql_query = "SELECT SHIFT_ID FROM changed_query".to_owned();
            store.update_flow(id, &edited, true).await?;
            let commit = store
                .commit_run(
                    &old_flow,
                    run_id,
                    "test",
                    &["[1]".to_owned()],
                    RunCounts {
                        row_count: 1,
                        new_key_count: 1,
                    },
                    "session",
                )
                .await;
            let new_flow = store.flow(id).await?.ok_or("Flow missing")?;
            let empty = store.messages(&HistoryFilter::default()).await?.is_empty();
            Ok(matches!(commit, Err(crate::StoreError::StaleRevision))
                && new_flow.baseline_initialized == 0
                && new_flow.previous_keys == "[]"
                && empty)
        }
        .await;
        assert!(outcome.as_ref().is_ok_and(|value| *value), "{outcome:?}");
    }
}
