//! Typed, compile-time checked HTML views. All user-supplied values are HTML-escaped by Askama.
use askama::Template;
use meerkat_core::QueryResult;
use meerkat_store::model::{Attempt, Flow, Message, Run};

use super::{Filters, FlowForm};

#[derive(Clone, Copy)]
struct PageMeta {
    title: &'static str,
    nav: &'static str,
}

impl PageMeta {
    const fn new(title: &'static str, nav: &'static str) -> Self {
        Self { title, nav }
    }
}

fn render(template: &impl Template) -> String {
    template.render().unwrap_or_else(|error| {
        tracing::error!(%error, "HTML template rendering failed");
        "<!doctype html><html lang='en'><title>Meerkat error</title><body><h1>Unable to render page</h1><a href='/flows'>Back to flows</a></body></html>".to_owned()
    })
}

fn outcome_class(value: &str) -> &'static str {
    match value {
        "succeeded" | "delivered" => "success",
        "failed" | "exhausted" => "danger",
        "running" | "sending" | "pending" => "warning",
        _ => "neutral",
    }
}

const fn weekday_name(weekday: i64) -> &'static str {
    match weekday {
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        7 => "Sunday",
        _ => "Unknown day",
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    meta: PageMeta,
    message: &'a str,
}

pub fn error_page(message: &str) -> String {
    render(&ErrorPage {
        meta: PageMeta::new("Request failed", ""),
        message,
    })
}

#[derive(Template)]
#[template(path = "flows/list.html")]
struct FlowsPage<'a> {
    meta: PageMeta,
    flows: &'a [Flow],
}

pub fn flows(flows: &[Flow]) -> String {
    render(&FlowsPage {
        meta: PageMeta::new("Flows", "flows"),
        flows,
    })
}

struct WeekdayOption {
    name: &'static str,
    value: i64,
    selected: bool,
}

#[derive(Template)]
#[template(path = "flows/editor.html")]
struct EditorPage<'a> {
    meta: PageMeta,
    form: &'a FlowForm,
    action: String,
    is_edit: bool,
    weekdays: Vec<WeekdayOption>,
    feedback: Option<&'a QueryFeedback>,
}

pub fn editor(form: &FlowForm, feedback: Option<&QueryFeedback>) -> String {
    let weekdays = (1..=7)
        .map(|day| WeekdayOption {
            name: weekday_name(day),
            value: day,
            selected: form.weekday == day,
        })
        .collect();
    render(&EditorPage {
        meta: PageMeta::new("Configure flow", "flows"),
        form,
        action: form
            .flow_id
            .map_or_else(|| "/flows".to_owned(), |id| format!("/flows/{id}")),
        is_edit: form.flow_id.is_some(),
        weekdays,
        feedback,
    })
}

/// Data passed to the query-preview partial, whether embedded or returned over HTMX.
#[derive(Clone, Debug)]
pub struct QueryFeedback {
    pub error: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

impl QueryFeedback {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
        }
    }

    pub fn from_query(result: &QueryResult) -> Self {
        let rows = result
            .rows
            .iter()
            .take(20)
            .map(|row| {
                result
                    .columns
                    .iter()
                    .map(|column| {
                        row.get(column).map_or_else(
                            || "null".to_owned(),
                            |value| match value {
                                serde_json::Value::String(text) => text.clone(),
                                _ => value.to_string(),
                            },
                        )
                    })
                    .collect()
            })
            .collect();
        Self {
            error: None,
            columns: result.columns.clone(),
            rows,
            truncated: result.rows.len() > 20,
        }
    }
}

#[derive(Template)]
#[template(path = "partials/query_results.html")]
struct QueryResultPartial<'a> {
    report: &'a QueryFeedback,
}

pub fn query_feedback(report: &QueryFeedback) -> String {
    render(&QueryResultPartial { report })
}

#[derive(Template)]
#[template(path = "flows/detail.html")]
struct FlowPage<'a> {
    meta: PageMeta,
    flow: &'a Flow,
    weekday: &'static str,
    key_count: usize,
    runs: Vec<RunRow<'a>>,
    messages: Vec<MessageRow<'a>>,
}

pub fn flow_details(flow: &Flow, runs: &[Run], messages: &[Message]) -> String {
    let key_count = serde_json::from_str::<Vec<serde_json::Value>>(&flow.previous_keys)
        .map_or(0, |keys| keys.len());
    render(&FlowPage {
        meta: PageMeta::new("Flow details", "flows"),
        flow,
        weekday: weekday_name(flow.weekday),
        key_count,
        runs: run_rows(runs),
        messages: message_rows(messages),
    })
}

struct RunRow<'a> {
    run: &'a Run,
    row_count: String,
    new_key_count: String,
    message_count: i64,
    status_class: &'static str,
}

fn run_rows(runs: &[Run]) -> Vec<RunRow<'_>> {
    runs.iter()
        .map(|run| RunRow {
            run,
            row_count: run
                .row_count
                .map_or_else(|| "—".to_owned(), |n| n.to_string()),
            new_key_count: run
                .new_key_count
                .map_or_else(|| "—".to_owned(), |n| n.to_string()),
            message_count: run.message_count.unwrap_or(0),
            status_class: outcome_class(&run.status),
        })
        .collect()
}

struct MessageRow<'a> {
    message: &'a Message,
    status_class: &'static str,
}

fn message_rows(messages: &[Message]) -> Vec<MessageRow<'_>> {
    messages
        .iter()
        .map(|message| MessageRow {
            message,
            status_class: outcome_class(&message.status),
        })
        .collect()
}

struct AttemptRow<'a> {
    attempt: &'a Attempt,
    http_status: String,
    status_class: &'static str,
}

fn attempt_rows(attempts: &[Attempt]) -> Vec<AttemptRow<'_>> {
    attempts
        .iter()
        .map(|attempt| AttemptRow {
            attempt,
            http_status: attempt
                .http_status
                .map_or_else(|| "—".to_owned(), |n| n.to_string()),
            status_class: outcome_class(&attempt.outcome),
        })
        .collect()
}

struct Pagination {
    previous: Option<String>,
    next: String,
    current: i64,
}

fn pagination(path: &str, filter: &Filters) -> Pagination {
    fn url_for(path: &str, filter: &Filters, page: i64) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if let Some(flow_id) = filter.flow_id {
            query.append_pair("flow_id", &flow_id.to_string());
        }
        if let Some(run_id) = filter.run_id {
            query.append_pair("run_id", &run_id.to_string());
        }
        if let Some(message_id) = &filter.message_id {
            query.append_pair("message_id", message_id);
        }
        if let Some(status) = &filter.status {
            query.append_pair("status", status);
        }
        query.append_pair("page", &page.to_string());
        format!("{path}?{}", query.finish())
    }
    let page = filter.page.unwrap_or(0).max(0);
    Pagination {
        previous: (page > 0).then(|| url_for(path, filter, page.saturating_sub(1))),
        next: url_for(path, filter, page.saturating_add(1)),
        current: page.saturating_add(1),
    }
}

#[derive(Template)]
#[template(path = "runs/list.html")]
struct RunsPage<'a> {
    meta: PageMeta,
    runs: Vec<RunRow<'a>>,
    filter: &'a Filters,
    pagination: Pagination,
}

pub fn runs(runs: &[Run], filter: &Filters) -> String {
    render(&RunsPage {
        meta: PageMeta::new("Runs", "runs"),
        runs: run_rows(runs),
        filter,
        pagination: pagination("/runs", filter),
    })
}

#[derive(Template)]
#[template(path = "runs/detail.html")]
struct RunPage<'a> {
    meta: PageMeta,
    run: RunRow<'a>,
    messages: Vec<MessageRow<'a>>,
}

pub fn run_details(run: &Run, messages: &[Message]) -> String {
    let rows = run_rows(std::slice::from_ref(run));
    // The page has exactly one row; taking ownership avoids a second wrapper constructor.
    let Some(run_view) = rows.into_iter().next() else {
        return error_page("Run not available");
    };
    render(&RunPage {
        meta: PageMeta::new("Run details", "runs"),
        run: run_view,
        messages: message_rows(messages),
    })
}

#[derive(Template)]
#[template(path = "messages/list.html")]
struct MessagesPage<'a> {
    meta: PageMeta,
    messages: Vec<MessageRow<'a>>,
    filter: &'a Filters,
    pagination: Pagination,
}

pub fn messages(messages: &[Message], filter: &Filters) -> String {
    render(&MessagesPage {
        meta: PageMeta::new("Webhook Messages", "messages"),
        messages: message_rows(messages),
        filter,
        pagination: pagination("/messages", filter),
    })
}

#[derive(Template)]
#[template(path = "messages/detail.html")]
struct MessagePage<'a> {
    meta: PageMeta,
    message: MessageRow<'a>,
    attempts: Vec<AttemptRow<'a>>,
}

pub fn message_details(message: &Message, attempts: &[Attempt]) -> String {
    render(&MessagePage {
        meta: PageMeta::new("Message details", "messages"),
        message: MessageRow {
            message,
            status_class: outcome_class(&message.status),
        },
        attempts: attempt_rows(attempts),
    })
}

#[derive(Template)]
#[template(path = "attempts/list.html")]
struct AttemptsPage<'a> {
    meta: PageMeta,
    attempts: Vec<AttemptRow<'a>>,
    filter: &'a Filters,
    pagination: Pagination,
}

pub fn attempts(attempts: &[Attempt], filter: &Filters) -> String {
    render(&AttemptsPage {
        meta: PageMeta::new("Delivery Attempts", "attempts"),
        attempts: attempt_rows(attempts),
        filter,
        pagination: pagination("/delivery-attempts", filter),
    })
}

#[cfg(test)]
mod tests {
    use super::{QueryFeedback, query_feedback};

    #[test]
    fn query_feedback_renders_rows_and_headings() {
        let html = query_feedback(&QueryFeedback {
            error: None,
            columns: vec!["SHIFT_ID".to_owned()],
            rows: vec![vec!["2026-11-21-A".to_owned()]],
            truncated: false,
        });
        assert!(html.contains("SHIFT_ID"));
        assert!(html.contains("2026-11-21-A"));
    }

    #[test]
    fn previews_are_html_escaped() {
        let html = query_feedback(&QueryFeedback {
            error: Some("<script>alert('test')</script>".to_owned()),
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
        });
        assert!(!html.contains("<script>"));
        assert!(html.contains("&#60;script&#62;"));
        assert!(html.contains("&#60;/script&#62;"));
    }

    #[test]
    fn query_values_and_column_names_are_html_escaped() {
        let html = query_feedback(&QueryFeedback {
            error: None,
            columns: vec!["<column>".to_owned()],
            rows: vec![vec!["<script>alert('test')</script>".to_owned()]],
            truncated: false,
        });
        assert!(!html.contains("<column>"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&#60;column&#62;"));
        assert!(html.contains("&#60;script&#62;alert(&#39;test&#39;)&#60;/script&#62;"));
    }
}
