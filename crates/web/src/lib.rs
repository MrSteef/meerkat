//! Server-rendered, loopback-first configuration and observability interface.
//! No browser build pipeline or client-side application framework is required.
mod pages;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header::CONTENT_TYPE};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use meerkat_core::{QueryExecutor, validate_read_only_sql};
use meerkat_store::Store;
use meerkat_store::model::{Flow, FlowInput, HistoryFilter};
use serde::Deserialize;

#[derive(Clone)]
pub struct WebState {
    pub store: Store,
    pub executor: Arc<dyn QueryExecutor>,
    pub query_capacity: Arc<tokio::sync::Semaphore>,
}

#[derive(Debug)]
struct WebError(String);
impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Html(pages::error_page(&self.0))).into_response()
    }
}
fn web_err(error: impl std::fmt::Display) -> WebError {
    WebError(error.to_string())
}
type WebResult = Result<Html<String>, WebError>;

#[derive(Debug, Deserialize, Default)]
struct Filters {
    flow_id: Option<i64>,
    run_id: Option<i64>,
    message_id: Option<String>,
    status: Option<String>,
    page: Option<i64>,
}
impl Filters {
    fn history(&self) -> HistoryFilter {
        HistoryFilter {
            flow_id: self.flow_id,
            run_id: self.run_id,
            message_id: self.message_id.clone(),
            status: self.status.clone(),
            page: self.page.unwrap_or(0),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct FlowForm {
    flow_id: Option<i64>,
    name: String,
    enabled: Option<String>,
    sql_query: String,
    key_columns: String,
    trigger_on_first_run: Option<String>,
    weekday: i64,
    start_time: String,
    end_time: String,
    timezone: String,
    poll_interval_seconds: i64,
    stop_after_detection: Option<String>,
    url: String,
    headers: String,
    payload_template: String,
    reset_baseline: Option<String>,
}
impl FlowForm {
    fn blank() -> Self {
        Self {
            flow_id: None,
            name: String::new(),
            enabled: Some("on".to_owned()),
            sql_query: "SELECT SHIFT_ID FROM YOUR_READINESS_QUERY".to_owned(),
            key_columns: "SHIFT_ID".to_owned(),
            trigger_on_first_run: None,
            weekday: 6,
            start_time: "13:00".to_owned(),
            end_time: "19:00".to_owned(),
            timezone: "Europe/Amsterdam".to_owned(),
            poll_interval_seconds: 60,
            stop_after_detection: Some("on".to_owned()),
            url: "http://127.0.0.1:8080/hook".to_owned(),
            headers: "{}".to_owned(),
            payload_template: "{\"text\":\"All waves are active\"}".to_owned(),
            reset_baseline: Some("on".to_owned()),
        }
    }
    fn from_flow(flow: &Flow) -> Self {
        let keys = serde_json::from_str::<Vec<String>>(&flow.key_columns).unwrap_or_default();
        Self {
            flow_id: Some(flow.id),
            name: flow.name.clone(),
            enabled: (flow.enabled != 0).then(|| "on".to_owned()),
            sql_query: flow.sql_query.clone(),
            key_columns: keys.join(", "),
            trigger_on_first_run: (flow.trigger_on_first_run != 0).then(|| "on".to_owned()),
            weekday: flow.weekday,
            start_time: flow.start_time.clone(),
            end_time: flow.end_time.clone(),
            timezone: flow.timezone.clone(),
            poll_interval_seconds: flow.poll_interval_seconds,
            stop_after_detection: (flow.stop_after_detection != 0).then(|| "on".to_owned()),
            url: flow.url.clone(),
            headers: flow.headers.clone(),
            payload_template: flow.payload_template.clone(),
            reset_baseline: None,
        }
    }
    fn into_input(self) -> FlowInput {
        let keys = self
            .key_columns
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        FlowInput {
            name: self.name,
            enabled: self.enabled.is_some(),
            sql_query: self.sql_query,
            key_columns: keys,
            trigger_on_first_run: self.trigger_on_first_run.is_some(),
            weekday: self.weekday,
            start_time: self.start_time,
            end_time: self.end_time,
            timezone: self.timezone,
            poll_interval_seconds: self.poll_interval_seconds,
            stop_after_detection: self.stop_after_detection.is_some(),
            url: self.url,
            headers: self.headers,
            payload_template: self.payload_template,
        }
    }
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::to("/flows") }))
        .route(
            "/static/styles.css",
            get(|| async {
                (
                    [(CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../static/styles.css"),
                )
            }),
        )
        .route(
            "/static/app.js",
            get(|| async {
                (
                    [(CONTENT_TYPE, "application/javascript; charset=utf-8")],
                    include_str!("../static/app.js"),
                )
            }),
        )
        .route("/flows", get(flows).post(create_flow))
        .route("/flows/new", get(new_flow))
        .route("/flows/test", post(test_query))
        .route("/flows/{id}", get(flow_details).post(update_flow))
        .route("/flows/{id}/edit", get(edit_flow))
        .route("/flows/{id}/delete", post(delete_flow))
        .route("/runs", get(runs))
        .route("/runs/{id}", get(run_details))
        .route("/messages", get(messages))
        .route("/messages/{id}", get(message_details))
        .route("/delivery-attempts", get(attempts))
        .with_state(state)
}

async fn flows(State(state): State<WebState>) -> WebResult {
    let list = state.store.list_flows().await.map_err(web_err)?;
    Ok(Html(pages::flows(&list)))
}
async fn new_flow() -> Html<String> {
    Html(pages::editor(&FlowForm::blank(), None))
}
async fn edit_flow(Path(id): Path<i64>, State(state): State<WebState>) -> WebResult {
    let flow = state
        .store
        .flow(id)
        .await
        .map_err(web_err)?
        .ok_or_else(|| web_err("Flow not found"))?;
    Ok(Html(pages::editor(&FlowForm::from_flow(&flow), None)))
}
async fn create_flow(State(state): State<WebState>, Form(form): Form<FlowForm>) -> Response {
    match state.store.create_flow(&form.clone().into_input()).await {
        Ok(id) => Redirect::to(&format!("/flows/{id}")).into_response(),
        Err(error) => {
            let feedback = pages::QueryFeedback::error(format!("Could not save flow: {error}"));
            (
                StatusCode::BAD_REQUEST,
                Html(pages::editor(&form, Some(&feedback))),
            )
                .into_response()
        }
    }
}
async fn update_flow(
    Path(id): Path<i64>,
    State(state): State<WebState>,
    Form(form): Form<FlowForm>,
) -> Response {
    let reset = form.reset_baseline.is_some();
    match state
        .store
        .update_flow(id, &form.clone().into_input(), reset)
        .await
    {
        Ok(()) => Redirect::to(&format!("/flows/{id}")).into_response(),
        Err(error) => {
            let feedback = pages::QueryFeedback::error(format!("Could not save flow: {error}"));
            (
                StatusCode::BAD_REQUEST,
                Html(pages::editor(&form, Some(&feedback))),
            )
                .into_response()
        }
    }
}
async fn delete_flow(
    Path(id): Path<i64>,
    State(state): State<WebState>,
) -> Result<Redirect, WebError> {
    state.store.delete_flow(id).await.map_err(web_err)?;
    Ok(Redirect::to("/flows"))
}
async fn test_query(
    State(state): State<WebState>,
    headers: HeaderMap,
    Form(form): Form<FlowForm>,
) -> Html<String> {
    let report = match validate_read_only_sql(&form.sql_query) {
        Err(error) => pages::QueryFeedback::error(format!("SQL validation: {error}")),
        Ok(()) => match state.query_capacity.acquire().await {
            Ok(_permit) => match state.executor.execute(&form.sql_query).await {
                Ok(result) => pages::QueryFeedback::from_query(&result),
                Err(error) => {
                    pages::QueryFeedback::error(format!("Query failed: {}", error.message))
                }
            },
            Err(error) => {
                pages::QueryFeedback::error(format!("Query capacity unavailable: {error}"))
            }
        },
    };
    let htmx = headers
        .get("HX-Request")
        .is_some_and(|value| value == "true");
    if htmx {
        Html(pages::query_feedback(&report))
    } else {
        Html(pages::editor(&form, Some(&report)))
    }
}
async fn flow_details(Path(id): Path<i64>, State(state): State<WebState>) -> WebResult {
    let flow = state
        .store
        .flow(id)
        .await
        .map_err(web_err)?
        .ok_or_else(|| web_err("Flow not found"))?;
    let filter = HistoryFilter {
        flow_id: Some(id),
        ..HistoryFilter::default()
    };
    let runs = state.store.runs(&filter).await.map_err(web_err)?;
    let messages = state.store.messages(&filter).await.map_err(web_err)?;
    Ok(Html(pages::flow_details(&flow, &runs, &messages)))
}
async fn runs(Query(filter): Query<Filters>, State(state): State<WebState>) -> WebResult {
    let rows = state.store.runs(&filter.history()).await.map_err(web_err)?;
    Ok(Html(pages::runs(&rows, &filter)))
}
async fn run_details(Path(id): Path<i64>, State(state): State<WebState>) -> WebResult {
    let run = state
        .store
        .run(id)
        .await
        .map_err(web_err)?
        .ok_or_else(|| web_err("Run not found"))?;
    let filter = HistoryFilter {
        run_id: Some(id),
        ..HistoryFilter::default()
    };
    let messages = state.store.messages(&filter).await.map_err(web_err)?;
    Ok(Html(pages::run_details(&run, &messages)))
}
async fn messages(Query(filter): Query<Filters>, State(state): State<WebState>) -> WebResult {
    let rows = state
        .store
        .messages(&filter.history())
        .await
        .map_err(web_err)?;
    Ok(Html(pages::messages(&rows, &filter)))
}
async fn message_details(Path(id): Path<String>, State(state): State<WebState>) -> WebResult {
    let message = state
        .store
        .message(&id)
        .await
        .map_err(web_err)?
        .ok_or_else(|| web_err("Message not found"))?;
    let attempts = state.store.message_attempts(&id).await.map_err(web_err)?;
    Ok(Html(pages::message_details(&message, &attempts)))
}
async fn attempts(Query(filter): Query<Filters>, State(state): State<WebState>) -> WebResult {
    let rows = state
        .store
        .attempts(&filter.history())
        .await
        .map_err(web_err)?;
    Ok(Html(pages::attempts(&rows, &filter)))
}
