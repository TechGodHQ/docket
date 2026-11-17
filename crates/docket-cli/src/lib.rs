//! docket CLI library: the handwritten part of the hydra project.
//!
//! `api/operations.yaml` is the single source of truth; the committed
//! artifacts under `generated/` are produced by `cargo run -p
//! docket-codegen --bin docket-codegen -- write` and gated by `-- check`
//! in CI. Every generated surface funnels into one dispatch:
//! [`execute_operation`].

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use clap::Parser;
use docket_core::{
    ActionDescriptor, DecisionLogEntry, DocketConfig, DocketInfo, ItemStatus, ItemStore, StoreError,
};
use serde_json::Value;

pub mod generated {
    include!("../generated/http.rs");
}

pub mod generated_cli {
    include!("../generated/cli.rs");
}

/// Constant path to generated MCP tool schemas.
pub const GENERATED_MCP_JSON: &str = include_str!("../generated/mcp.json");

/// Shared application state: the ledger.
#[derive(Clone)]
pub struct AppState {
    store: Arc<dyn ItemStore>,
}

impl AppState {
    /// Build state over any [`ItemStore`] implementation.
    #[must_use]
    pub fn new(store: Arc<dyn ItemStore>) -> Self {
        Self { store }
    }
}

/// Typed operation error carrying an HTTP status, so every surface reports
/// the same status/message pair.
#[derive(Debug)]
pub struct OperationError {
    /// HTTP status for the HTTP surface.
    pub status: StatusCode,
    /// Human-readable message used on all surfaces.
    pub message: String,
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for OperationError {}

fn bad_request(message: String) -> OperationError {
    OperationError {
        status: StatusCode::BAD_REQUEST,
        message,
    }
}

fn not_found(id: &str) -> OperationError {
    OperationError {
        status: StatusCode::NOT_FOUND,
        message: format!("item not found: {id}"),
    }
}

fn store_error(err: StoreError) -> OperationError {
    match err {
        StoreError::NotFound(id) => not_found(&id),
        other => OperationError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: other.to_string(),
        },
    }
}

fn query_str(
    input: &generated::GeneratedOperationInput,
    key: &str,
) -> Result<String, OperationError> {
    input
        .query
        .get(key)
        .cloned()
        .ok_or_else(|| bad_request(format!("missing query parameter: {key}")))
}

fn query_opt(input: &generated::GeneratedOperationInput, key: &str) -> Option<String> {
    input.query.get(key).cloned()
}

fn parse_status(raw: &str) -> Result<ItemStatus, OperationError> {
    match raw {
        "pending" => Ok(ItemStatus::Pending),
        "dispatched" => Ok(ItemStatus::Dispatched),
        "dismissed" => Ok(ItemStatus::Dismissed),
        other => Err(bad_request(format!(
            "unknown status '{other}'; expected pending|dispatched|dismissed"
        ))),
    }
}

fn path_str(
    input: &generated::GeneratedOperationInput,
    key: &str,
) -> Result<String, OperationError> {
    input
        .path
        .get(key)
        .cloned()
        .ok_or_else(|| bad_request(format!("missing path parameter: {key}")))
}

/// The single dispatch every generated surface funnels through.
///
/// # Errors
/// Returns a typed [`OperationError`] for unknown operations, missing
/// parameters, unknown statuses, producer-law violations, and store
/// failures.
pub async fn execute_operation(
    state: &AppState,
    operation: &str,
    input: generated::GeneratedOperationInput,
) -> Result<Value, OperationError> {
    match operation {
        "docket_info" => {
            let info = DocketInfo {
                service: "docket",
                version: env!("CARGO_PKG_VERSION"),
                role: "decision ledger, never executor",
            };
            serde_json::to_value(info).map_err(|e| bad_request(format!("serialize failed: {e}")))
        }
        "list_items" => {
            let status = match query_opt(&input, "status") {
                Some(raw) => Some(parse_status(&raw)?),
                None => None,
            };
            let source = query_opt(&input, "source");
            let items = state
                .store
                .list(status, source.as_deref())
                .map_err(store_error)?;
            serde_json::to_value(items).map_err(|e| bad_request(format!("serialize failed: {e}")))
        }
        "get_item" => {
            let id = path_str(&input, "id")?;
            let item = state
                .store
                .get(&id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(&id))?;
            serde_json::to_value(item).map_err(|e| bad_request(format!("serialize failed: {e}")))
        }
        "create_item" => {
            let source = query_str(&input, "source")?;
            let title = query_str(&input, "title")?;
            let context = query_str(&input, "context")?;
            let recommendation = query_str(&input, "recommendation")?;
            let action = ActionDescriptor {
                target: query_str(&input, "action_target")?,
                operation: query_str(&input, "action_operation")?,
                params: query_opt(&input, "action_params")
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
            };
            let item = state
                .store
                .create(&source, &title, &context, &recommendation, &action)
                .map_err(store_error)?;
            serde_json::to_value(item).map_err(|e| bad_request(format!("serialize failed: {e}")))
        }
        "record_decision" => {
            let id = path_str(&input, "id")?;
            let actor = query_str(&input, "actor")?;
            let decision = query_str(&input, "decision")?;
            let note = query_opt(&input, "note");
            let new_status = match query_opt(&input, "new_status") {
                Some(raw) => parse_status(&raw)?,
                None => ItemStatus::Dispatched,
            };
            if state.store.get(&id).map_err(store_error)?.is_none() {
                return Err(not_found(&id));
            }
            let entry = DecisionLogEntry {
                at: now_rfc3339(),
                actor,
                decision,
                note,
            };
            let item = state
                .store
                .record_decision(&id, entry, new_status)
                .map_err(store_error)?;
            serde_json::to_value(item).map_err(|e| bad_request(format!("serialize failed: {e}")))
        }
        other => Err(bad_request(format!("unknown operation: {other}"))),
    }
}

/// Current UTC time in RFC 3339 (second precision, no external deps).
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// HTTP adapter: run the operation and map typed errors to status codes.
///
/// # Panics
/// Never.
#[allow(clippy::unused_async)]
pub async fn execute_operation_http(
    state: &AppState,
    operation: &str,
    input: generated::GeneratedOperationInput,
) -> Response {
    match execute_operation(state, operation, input).await {
        Ok(value) => axum::Json(value).into_response(),
        Err(err) => (
            err.status,
            axum::Json(serde_json::json!({ "error": err.message })),
        )
            .into_response(),
    }
}

/// Build the HTTP router for docket.
pub fn http_router(state: AppState) -> axum::Router {
    generated::generated_router().with_state(state)
}

/// HTTP entry: serve the clean API surface (design law (c)) on localhost.
///
/// # Errors
/// Propagates listener/serve failures.
pub async fn run_http() -> anyhow::Result<()> {
    let config = DocketConfig::default();
    let state = AppState::new(Arc::new(docket_core::InMemoryStore::new()));
    let app = http_router(state);
    let addr = format!("127.0.0.1:{}", config.http_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// CLI entry: parse args, dispatch through the same operation path.
///
/// # Errors
/// Propagates dispatch failures.
pub async fn run_cli() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "docket",
        about = "Decision ledger: items arrive decision-ready"
    )]
    struct Cli {
        #[command(subcommand)]
        command: generated_cli::GeneratedCommand,
    }

    let cli = Cli::parse();
    let state = AppState::new(Arc::new(docket_core::InMemoryStore::new()));

    let operation = cli.command.operation_name();
    let params = cli.command.parameters_json();
    let string_params: std::collections::BTreeMap<String, String> = params
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(_, v)| v.is_string())
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let path_params: std::collections::BTreeMap<String, String> = params
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, v)| !v.is_string() && string_params.contains_key(k.as_str()))
                .map(|(k, _)| (k.clone(), String::new()))
                .collect()
        })
        .unwrap_or_default();
    let mut path_map = std::collections::BTreeMap::new();
    let mut query_map = string_params;
    if let Some(v) = query_map.remove("id") {
        path_map.insert("id".to_string(), v);
    }
    let _ = path_params;
    let input = generated::GeneratedOperationInput {
        path: path_map,
        query: query_map,
        body: Value::Null,
    };

    let value = execute_operation(&state, operation, input)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use docket_core::InMemoryStore;

    fn state() -> AppState {
        AppState::new(Arc::new(InMemoryStore::new()))
    }

    fn input(path: &[(&str, &str)], query: &[(&str, &str)]) -> generated::GeneratedOperationInput {
        generated::GeneratedOperationInput {
            path: path
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            query: query
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: Value::Null,
        }
    }

    async fn seed(state: &AppState) -> String {
        let value = execute_operation(
            state,
            "create_item",
            input(
                &[],
                &[
                    ("source", "plane"),
                    ("title", "merge docket#1"),
                    ("context", "CI green; reviewer approved"),
                    ("recommendation", "merge now"),
                    ("action_target", "hermes-coding"),
                    ("action_operation", "merge_pr"),
                    ("action_params", "{\"pr\":1}"),
                ],
            ),
        )
        .await
        .expect("create");
        value["id"].as_str().expect("id").to_string()
    }

    #[tokio::test]
    async fn docket_info_states_the_invariant() {
        let value = execute_operation(&state(), "docket_info", input(&[], &[]))
            .await
            .expect("dispatch");
        assert_eq!(value["service"], "docket");
        assert_eq!(value["role"], "decision ledger, never executor");
    }

    #[tokio::test]
    async fn create_and_get_item_roundtrip() {
        let st = state();
        let id = seed(&st).await;
        let value = execute_operation(&st, "get_item", input(&[("id", &id)], &[]))
            .await
            .expect("get");
        assert_eq!(value["status"], "pending");
        assert_eq!(value["action_descriptor"]["target"], "hermes-coding");
        assert_eq!(value["context"], "CI green; reviewer approved");
    }

    #[tokio::test]
    async fn create_without_context_is_rejected() {
        let st = state();
        let err = execute_operation(
            &st,
            "create_item",
            input(
                &[],
                &[
                    ("source", "plane"),
                    ("title", "t"),
                    ("context", "  "),
                    ("recommendation", "rec"),
                    ("action_target", "comms"),
                    ("action_operation", "send_email"),
                ],
            ),
        )
        .await
        .expect_err("producer bug must be rejected");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("producer bug"));
    }

    #[tokio::test]
    async fn record_decision_appends_log() {
        let st = state();
        let id = seed(&st).await;
        let value = execute_operation(
            &st,
            "record_decision",
            input(
                &[("id", &id)],
                &[("actor", "shiv"), ("decision", "approve")],
            ),
        )
        .await
        .expect("decide");
        assert_eq!(value["status"], "dispatched");
        assert_eq!(value["decision_log"][0]["actor"], "shiv");
    }

    #[tokio::test]
    async fn decision_on_unknown_item_is_404() {
        let st = state();
        let err = execute_operation(
            &st,
            "record_decision",
            input(
                &[("id", "dkt-999999")],
                &[("actor", "shiv"), ("decision", "approve")],
            ),
        )
        .await
        .expect_err("should 404");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let st = state();
        seed(&st).await;
        let value = execute_operation(
            &st,
            "list_items",
            input(&[], &[("status", "pending"), ("source", "plane")]),
        )
        .await
        .expect("list");
        assert_eq!(value.as_array().expect("array").len(), 1);
    }

    #[tokio::test]
    async fn unknown_status_is_bad_request() {
        let err = execute_operation(&state(), "list_items", input(&[], &[("status", "nope")]))
            .await
            .expect_err("should reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_operation_is_rejected() {
        let err = execute_operation(&state(), "nope", input(&[], &[]))
            .await
            .expect_err("should reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn generated_mcp_manifest_is_embedded() {
        assert!(GENERATED_MCP_JSON.contains("create_item"));
    }

    #[test]
    fn now_rfc3339_shape() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.as_bytes()[10], b'T');
    }
}
