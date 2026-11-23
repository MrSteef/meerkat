use chrono::{DateTime, Utc};
use meerkat_core::validate_read_only_sql;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::model::{Flow, FlowInput, RunCounts};
use crate::{Store, StoreError};

const FLOW_SELECT: &str = "SELECT f.id, f.name, f.enabled, f.sql_query, f.key_columns, f.trigger_on_first_run, f.revision,
 s.id AS schedule_id, s.weekday, s.start_time, s.end_time, s.timezone, s.poll_interval_seconds,
 s.stop_after_detection, s.last_started_at, s.completed_window,
 w.id AS webhook_id, w.url, w.headers, w.payload_template,
 st.baseline_initialized, st.previous_keys,
 (SELECT r.status FROM runs r WHERE r.flow_id=f.id ORDER BY r.id DESC LIMIT 1) AS last_run_status,
 (SELECT r.started_at FROM runs r WHERE r.flow_id=f.id ORDER BY r.id DESC LIMIT 1) AS last_run_started_at
 FROM flows f JOIN schedules s ON s.flow_id = f.id AND s.deleted_at IS NULL
 JOIN webhooks w ON w.flow_id = f.id AND w.deleted_at IS NULL
 JOIN flow_state st ON st.flow_id = f.id WHERE f.deleted_at IS NULL";

impl Store {
    /// # Errors
    /// Returns SQLite or serialization errors.
    pub async fn list_flows(&self) -> Result<Vec<Flow>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(FLOW_SELECT);
        query.push(" ORDER BY f.id DESC");
        Ok(query.build_query_as::<Flow>().fetch_all(&self.pool).await?)
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn flow(&self, id: i64) -> Result<Option<Flow>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(FLOW_SELECT);
        query.push(" AND f.id = ").push_bind(id);
        Ok(query
            .build_query_as::<Flow>()
            .fetch_optional(&self.pool)
            .await?)
    }

    /// # Errors
    /// Validates the configuration before any write.
    pub async fn create_flow(&self, value: &FlowInput) -> Result<i64, StoreError> {
        validate_config(value)?;
        let keys = serde_json::to_string(&value.key_columns)?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("INSERT INTO flows(name,enabled,sql_query,key_columns,trigger_on_first_run) VALUES(?,?,?,?,?)")
            .bind(&value.name).bind(i64::from(value.enabled)).bind(&value.sql_query).bind(keys)
            .bind(i64::from(value.trigger_on_first_run)).execute(&mut *tx).await?;
        let id = row.last_insert_rowid();
        sqlx::query("INSERT INTO flow_state(flow_id) VALUES(?)")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        insert_schedule(&mut tx, id, value).await?;
        insert_webhook(&mut tx, id, value).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Update configuration in one transaction. The revision invalidates
    /// already running evaluations. Reset state only when requested.
    ///
    /// # Errors
    /// Returns validation and SQLite errors.
    pub async fn update_flow(
        &self,
        id: i64,
        value: &FlowInput,
        reset_baseline: bool,
    ) -> Result<(), StoreError> {
        validate_config(value)?;
        let keys = serde_json::to_string(&value.key_columns)?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("UPDATE flows SET name=?, enabled=?, sql_query=?, key_columns=?, trigger_on_first_run=?, revision=revision+1 WHERE id=? AND deleted_at IS NULL")
            .bind(&value.name).bind(i64::from(value.enabled)).bind(&value.sql_query).bind(keys)
            .bind(i64::from(value.trigger_on_first_run)).bind(id).execute(&mut *tx).await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Validation("Flow does not exist".to_owned()));
        }
        // Do not discard completed-window / polling progress on a name,
        // SQL, or webhook-only edit. Replace the schedule only if its
        // scheduling fields actually changed.
        let existing_schedule = sqlx::query("SELECT id FROM schedules WHERE flow_id=? AND deleted_at IS NULL AND weekday=? AND start_time=? AND end_time=? AND timezone=? AND poll_interval_seconds=? AND stop_after_detection=?")
            .bind(id).bind(value.weekday).bind(&value.start_time).bind(&value.end_time)
            .bind(&value.timezone).bind(value.poll_interval_seconds).bind(i64::from(value.stop_after_detection))
            .fetch_optional(&mut *tx).await?;
        if existing_schedule.is_none() {
            sqlx::query("UPDATE schedules SET deleted_at=? WHERE flow_id=? AND deleted_at IS NULL")
                .bind(Utc::now().to_rfc3339())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            insert_schedule(&mut tx, id, value).await?;
        }
        // Preserve the existing webhook configuration if its request is
        // unchanged; otherwise soft-delete it to retain historic FK targets.
        let same_webhook = sqlx::query("SELECT id FROM webhooks WHERE flow_id=? AND deleted_at IS NULL AND url=? AND headers=? AND payload_template=?")
            .bind(id).bind(&value.url).bind(&value.headers).bind(&value.payload_template)
            .fetch_optional(&mut *tx).await?;
        if same_webhook.is_none() {
            sqlx::query("UPDATE webhooks SET deleted_at=? WHERE flow_id=? AND deleted_at IS NULL")
                .bind(Utc::now().to_rfc3339())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            insert_webhook(&mut tx, id, value).await?;
        }
        if reset_baseline {
            sqlx::query(
                "UPDATE flow_state SET baseline_initialized=0, previous_keys='[]' WHERE flow_id=?",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn delete_flow(&self, id: i64) -> Result<(), StoreError> {
        // Outbox and historical rows are intentionally unaffected.
        let now = Utc::now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE flows SET deleted_at=?, enabled=0, revision=revision+1 WHERE id=? AND deleted_at IS NULL")
            .bind(&now).bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE schedules SET deleted_at=? WHERE flow_id=? AND deleted_at IS NULL")
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE webhooks SET deleted_at=? WHERE flow_id=? AND deleted_at IS NULL")
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Atomically claim an execution and record its start. The caller must
    /// also reserve the sole Oracle worker in memory.
    ///
    /// # Errors
    /// A stale configuration revision is rejected.
    pub async fn start_run(&self, flow: &Flow, when: DateTime<Utc>) -> Result<i64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let current = sqlx::query(
            "SELECT revision FROM flows WHERE id=? AND enabled=1 AND deleted_at IS NULL",
        )
        .bind(flow.id)
        .fetch_optional(&mut *tx)
        .await?;
        if current
            .as_ref()
            .and_then(|r| r.try_get::<i64, _>("revision").ok())
            != Some(flow.revision)
        {
            return Err(StoreError::StaleRevision);
        }
        sqlx::query("UPDATE schedules SET last_started_at=? WHERE id=? AND deleted_at IS NULL")
            .bind(when.to_rfc3339())
            .bind(flow.schedule_id)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            "INSERT INTO runs(flow_id,schedule_id,started_at,status) VALUES(?,?,?,'running')",
        )
        .bind(flow.id)
        .bind(flow.schedule_id)
        .bind(when.to_rfc3339())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.last_insert_rowid())
    }

    /// # Errors
    /// Returns SQLite errors.
    pub async fn fail_run(&self, run_id: i64, status: &str, error: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE runs SET status=?, finished_at=?, error=? WHERE id=? AND status='running'",
        )
        .bind(status)
        .bind(Utc::now().to_rfc3339())
        .bind(error)
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// One atomic commit for the entire evaluated result and its outbox
    /// message. Old revisions are marked superseded and cannot write state.
    ///
    /// # Errors
    /// Reports stale configuration or SQLite errors.
    pub async fn commit_run(
        &self,
        flow: &Flow,
        run_id: i64,
        window_id: &str,
        current_keys: &[String],
        counts: RunCounts,
        session_id: &str,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let revision = sqlx::query(
            "SELECT revision FROM flows WHERE id=? AND enabled=1 AND deleted_at IS NULL",
        )
        .bind(flow.id)
        .fetch_optional(&mut *tx)
        .await?
        .and_then(|row| row.try_get::<i64, _>("revision").ok());
        if revision != Some(flow.revision) {
            sqlx::query("UPDATE runs SET status='superseded', finished_at=?, error='Flow configuration changed during execution' WHERE id=?")
                .bind(Utc::now().to_rfc3339()).bind(run_id).execute(&mut *tx).await?;
            tx.commit().await?;
            return Err(StoreError::StaleRevision);
        }
        let keys = serde_json::to_string(current_keys)?;
        sqlx::query(
            "UPDATE flow_state SET baseline_initialized=1, previous_keys=? WHERE flow_id=?",
        )
        .bind(keys)
        .bind(flow.id)
        .execute(&mut *tx)
        .await?;
        let message_count = i64::from(counts.new_key_count > 0);
        sqlx::query("UPDATE runs SET status='succeeded', finished_at=?, row_count=?, new_key_count=?, message_count=? WHERE id=?")
            .bind(Utc::now().to_rfc3339()).bind(i64::try_from(counts.row_count).unwrap_or(i64::MAX))
            .bind(i64::try_from(counts.new_key_count).unwrap_or(i64::MAX)).bind(message_count).bind(run_id)
            .execute(&mut *tx).await?;
        if counts.new_key_count > 0 {
            sqlx::query("INSERT INTO webhook_messages(id,run_id,webhook_id,session_id,created_at,url,headers,payload,status,next_attempt_at) VALUES(?,?,?,?,?,?,?,?,'pending',?)")
                .bind(uuid::Uuid::new_v4().to_string()).bind(run_id).bind(flow.webhook_id)
                .bind(session_id).bind(Utc::now().to_rfc3339()).bind(&flow.url)
                .bind(&flow.headers).bind(&flow.payload_template).bind(Utc::now().to_rfc3339())
                .execute(&mut *tx).await?;
            if flow.stop_after_detection != 0 {
                sqlx::query(
                    "UPDATE schedules SET completed_window=? WHERE id=? AND deleted_at IS NULL",
                )
                .bind(window_id)
                .bind(flow.schedule_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

async fn insert_schedule(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    value: &FlowInput,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO schedules(flow_id,weekday,start_time,end_time,timezone,poll_interval_seconds,stop_after_detection) VALUES(?,?,?,?,?,?,?)")
        .bind(id).bind(value.weekday).bind(&value.start_time).bind(&value.end_time).bind(&value.timezone)
        .bind(value.poll_interval_seconds).bind(i64::from(value.stop_after_detection))
        .execute(&mut **tx).await?;
    Ok(())
}

async fn insert_webhook(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    value: &FlowInput,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO webhooks(flow_id,url,headers,payload_template) VALUES(?,?,?,?)")
        .bind(id)
        .bind(&value.url)
        .bind(&value.headers)
        .bind(&value.payload_template)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn validate_config(value: &FlowInput) -> Result<(), StoreError> {
    if value.name.trim().is_empty() {
        return Err(StoreError::Validation("A flow name is required".to_owned()));
    }
    validate_read_only_sql(&value.sql_query).map_err(StoreError::Validation)?;
    if value.key_columns.is_empty() || value.key_columns.iter().any(|key| key.trim().is_empty()) {
        return Err(StoreError::Validation(
            "Select at least one nonempty identity column".to_owned(),
        ));
    }
    if value
        .key_columns
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != value.key_columns.len()
    {
        return Err(StoreError::Validation(
            "Identity columns must be distinct".to_owned(),
        ));
    }
    let schedule = meerkat_schedule::WeeklySchedule {
        id: 0,
        weekday: u32::try_from(value.weekday).unwrap_or(0),
        start_time: value.start_time.clone(),
        end_time: value.end_time.clone(),
        timezone: value.timezone.clone(),
        interval_seconds: value.poll_interval_seconds,
        completed_window: None,
        last_started_at: None,
    };
    meerkat_schedule::validate(&schedule).map_err(StoreError::Validation)?;
    // Reuse dedicated outbound validation so the UI can't save conflicting headers.
    meerkat_webhook::validate_endpoint(&value.url, &value.headers, &value.payload_template)
        .map_err(StoreError::Validation)?;
    Ok(())
}
