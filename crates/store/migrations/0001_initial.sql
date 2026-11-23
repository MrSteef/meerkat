PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS flows (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    sql_query TEXT NOT NULL,
    key_columns TEXT NOT NULL CHECK (json_valid(key_columns)),
    trigger_on_first_run INTEGER NOT NULL DEFAULT 0 CHECK (trigger_on_first_run IN (0,1)),
    revision INTEGER NOT NULL DEFAULT 1,
    deleted_at TEXT
);
CREATE TABLE IF NOT EXISTS schedules (
    id INTEGER PRIMARY KEY,
    flow_id INTEGER NOT NULL REFERENCES flows(id),
    weekday INTEGER NOT NULL CHECK (weekday BETWEEN 1 AND 7),
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    timezone TEXT NOT NULL,
    poll_interval_seconds INTEGER NOT NULL CHECK (poll_interval_seconds >= 5),
    stop_after_detection INTEGER NOT NULL DEFAULT 1 CHECK (stop_after_detection IN (0,1)),
    last_started_at TEXT,
    completed_window TEXT,
    deleted_at TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS one_active_schedule ON schedules(flow_id) WHERE deleted_at IS NULL;
CREATE TABLE IF NOT EXISTS webhooks (
    id INTEGER PRIMARY KEY,
    flow_id INTEGER NOT NULL REFERENCES flows(id),
    url TEXT NOT NULL,
    headers TEXT NOT NULL CHECK (json_valid(headers)),
    payload_template TEXT NOT NULL CHECK (json_valid(payload_template)),
    deleted_at TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS one_active_webhook ON webhooks(flow_id) WHERE deleted_at IS NULL;
CREATE TABLE IF NOT EXISTS flow_state (
    flow_id INTEGER PRIMARY KEY REFERENCES flows(id),
    baseline_initialized INTEGER NOT NULL DEFAULT 0 CHECK (baseline_initialized IN (0,1)),
    previous_keys TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(previous_keys))
);
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    flow_id INTEGER NOT NULL REFERENCES flows(id),
    schedule_id INTEGER REFERENCES schedules(id),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'superseded', 'interrupted')),
    row_count INTEGER,
    new_key_count INTEGER,
    message_count INTEGER,
    error TEXT
);
CREATE INDEX IF NOT EXISTS runs_by_flow ON runs(flow_id, id DESC);
CREATE TABLE IF NOT EXISTS webhook_messages (
    id TEXT PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES runs(id),
    webhook_id INTEGER NOT NULL REFERENCES webhooks(id),
    session_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    url TEXT NOT NULL,
    headers TEXT NOT NULL CHECK (json_valid(headers)),
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    status TEXT NOT NULL CHECK (status IN ('pending', 'sending', 'delivered', 'exhausted', 'interrupted')),
    next_attempt_at TEXT
);
CREATE INDEX IF NOT EXISTS pending_messages ON webhook_messages(status, session_id, next_attempt_at);
CREATE INDEX IF NOT EXISTS messages_by_run ON webhook_messages(run_id);
CREATE TABLE IF NOT EXISTS webhook_attempts (
    id INTEGER PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES webhook_messages(id),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    outcome TEXT NOT NULL CHECK (outcome IN ('in_progress', 'succeeded', 'failed', 'interrupted')),
    http_status INTEGER,
    error TEXT
);
CREATE INDEX IF NOT EXISTS attempts_by_message ON webhook_attempts(message_id, id DESC);
