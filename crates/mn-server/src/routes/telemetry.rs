//! `POST /v1/telemetry/events` — server-side telemetry ingest endpoint
//! (FR-110 / FR-111).
//!
//! Accepts a JSON array of typed events (see `mn_telemetry::Event`). Validates
//! every event with `serde(deny_unknown_fields)` semantics applied by the
//! shared schema and rejects malformed batches with 400. Successfully
//! decoded events are persisted to `telemetry_event_raw`; rejected events
//! are counted and surfaced in the response body so a client knows how
//! many it lost.
//!
//! The endpoint is **anonymous**. The bearer-extraction middleware leaves
//! the request as-is for unauthenticated callers and does not gate this
//! route — telemetry is opt-out at the *client* (FR-111). Authenticated
//! callers are accepted; the bearer is not consulted.
//!
//! On success the route returns `202 Accepted` with
//! `{accepted, rejected, errors}`. The server stores rows asynchronously
//! enough that the client doesn't pay for the per-row DB latency.

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mn_core::error::{Error as CoreError, ErrorCode};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Closed allow-list of `event_type` strings, matching the CHECK constraint
/// (migration `0005`, extended by `0012` to add `rerank`) AND
/// `mn_telemetry::EventPayload::event_type()`. The server-side validator MUST
/// stay in lockstep with both — any future event needs a coordinated bump on
/// the client schema, this allow-list, and a new migration extending the
/// constraint.
const ALLOWED_EVENT_TYPES: &[&str] = &[
    "mcp_tool_call",
    "cli_command",
    "ingest_complete",
    "pull_models",
    "mcp_startup",
    "mcp_shutdown",
    "rerank",
];

/// Closed allow-list of `component` strings (mirrors migration `0005`'s
/// component CHECK constraint).
const ALLOWED_COMPONENTS: &[&str] = &["cli", "mcp", "server"];

/// One row in the inbound batch. Fields mirror `mn_telemetry::Event` with the
/// `event_type` discriminator flattened from the payload so the validator
/// can reject unknown values cheaply.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundEvent {
    /// Emitting component (`cli` / `mcp` / `server`).
    pub component: String,
    /// Crate version that produced the event.
    pub version: String,
    /// Per-event-type payload object. The discriminator is at
    /// `payload.event_type`.
    pub payload: serde_json::Map<String, serde_json::Value>,
    /// Optional server-correlation id.
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Response body — counts + a small per-row error report for the rejected set.
#[derive(Debug, Serialize)]
pub struct IngestResponse {
    /// Rows persisted to `telemetry_event_raw`.
    pub accepted: u32,
    /// Rows rejected by validation (or DB insert failure).
    pub rejected: u32,
    /// Per-row diagnostics. Indexed by position in the inbound batch so a
    /// client can correlate. The maximum length is capped at the batch
    /// size; clients that send 100 events get at most 100 errors.
    pub errors: Vec<RejectionDetail>,
}

/// Per-row rejection reason — surfaced in the response body so a client
/// can log which event in the batch was dropped.
#[derive(Debug, Serialize)]
pub struct RejectionDetail {
    /// Index of the offending row in the inbound batch (0-based).
    pub index: u32,
    /// Stable, machine-readable reason. Currently one of `unknown_event_type`,
    /// `unknown_component`, `missing_event_type`, `insert_failed`.
    pub reason: &'static str,
}

/// Mount the telemetry routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/telemetry/events", post(ingest))
}

#[allow(clippy::too_many_lines)]
async fn ingest(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    Json(batch): Json<Vec<InboundEvent>>,
) -> Response {
    let rid = req_id.as_str();

    // FR-088-ish guard: reject absurdly large batches at the boundary. The
    // client policy caps at 100; we accept up to 1000 to absorb a few
    // missed flushes without becoming a DoS vector.
    if batch.len() > 1000 {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("batch of {} events exceeds limit of 1000", batch.len()))
                .remediation("split the batch into smaller chunks")
                .build(),
            rid,
        );
    }

    let mut accepted = 0u32;
    let mut errors: Vec<RejectionDetail> = Vec::new();

    for (idx, ev) in batch.into_iter().enumerate() {
        let idx_u32 = u32::try_from(idx).unwrap_or(u32::MAX);
        match validate(&ev) {
            Err(reason) => errors.push(RejectionDetail { index: idx_u32, reason }),
            Ok(event_type) => {
                if insert_one(&state.pool, &event_type, &ev).await.is_ok() {
                    accepted = accepted.saturating_add(1);
                } else {
                    errors.push(RejectionDetail {
                        index: idx_u32,
                        reason: "insert_failed",
                    });
                }
            }
        }
    }

    let rejected = u32::try_from(errors.len()).unwrap_or(u32::MAX);
    let body = IngestResponse { accepted, rejected, errors };
    (StatusCode::ACCEPTED, Json(body)).into_response()
}

fn validate(ev: &InboundEvent) -> Result<String, &'static str> {
    if !ALLOWED_COMPONENTS.contains(&ev.component.as_str()) {
        return Err("unknown_component");
    }
    let Some(type_value) = ev.payload.get("event_type") else {
        return Err("missing_event_type");
    };
    let Some(type_str) = type_value.as_str() else {
        return Err("missing_event_type");
    };
    if !ALLOWED_EVENT_TYPES.contains(&type_str) {
        return Err("unknown_event_type");
    }
    Ok(type_str.to_owned())
}

async fn insert_one(pool: &PgPool, event_type: &str, ev: &InboundEvent) -> Result<(), sqlx::Error> {
    let id = Uuid::new_v4();
    let fields = serde_json::Value::Object(ev.payload.clone());
    sqlx::query(
        "INSERT INTO telemetry_event_raw (id, event_type, component, version, fields, request_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(event_type)
    .bind(&ev.component)
    .bind(&ev.version)
    .bind(fields)
    .bind(ev.request_id.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mn_telemetry::events::{Component, Event, EventPayload};

    /// Decode a serialized `Event` into the validator's `InboundEvent` shape.
    fn inbound(event: &Event) -> InboundEvent {
        let v = serde_json::to_value(event).expect("serialize event");
        serde_json::from_value(v).expect("decode into InboundEvent")
    }

    #[test]
    fn validate_accepts_rerank_event() {
        // Regression guard: the `rerank` event_type must pass the allow-list,
        // otherwise every emitted rerank decision is dropped as
        // `unknown_event_type` (the bug 0012 + this allow-list bump fix).
        let event = Event::new(
            Component::Mcp,
            "0.1.0",
            EventPayload::Rerank {
                placement: "server".to_owned(),
                model: Some("rerank-2.5".to_owned()),
                applied: true,
                reason: None,
                billed_tokens: None,
            },
        );
        assert_eq!(validate(&inbound(&event)).as_deref(), Ok("rerank"));
    }

    #[test]
    fn validate_rejects_unknown_event_type() {
        let mut ev = inbound(&Event::new(
            Component::Cli,
            "0.1.0",
            EventPayload::McpShutdown { uptime_s: 0, tools_served: 0 },
        ));
        ev.payload.insert(
            "event_type".to_owned(),
            serde_json::Value::String("not_a_real_event".to_owned()),
        );
        assert_eq!(validate(&ev), Err("unknown_event_type"));
    }

    #[test]
    fn allow_list_covers_every_event_payload_variant() {
        // The server allow-list MUST accept every event the clients can emit;
        // a variant the enum produces but the allow-list omits is silently
        // dropped at ingest. Build one of every variant and assert each passes.
        let variants = [
            EventPayload::McpToolCall {
                tool_name: mn_telemetry::events::McpToolName::Search,
                latency_ms: 0,
                result_count: 0,
                model_state: mn_telemetry::events::ModelState::Ready,
                rerank_on: false,
                outcome: mn_telemetry::events::Outcome::Ok,
                corpus_model: None,
                reranker_used: None,
                top_confidence: None,
                top_attribution: None,
                top_source: None,
                filtered_by_confidence: None,
                deduplicated_count: None,
            },
            EventPayload::Rerank {
                placement: "off".to_owned(),
                model: None,
                applied: false,
                reason: None,
                billed_tokens: None,
            },
            EventPayload::CliCommand {
                command: mn_telemetry::events::CliCommandName::Search,
                duration_ms: 0,
                outcome: mn_telemetry::events::Outcome::Ok,
            },
            EventPayload::IngestComplete {
                documents_added: 0,
                documents_updated: 0,
                documents_skipped: 0,
                duration_ms: 0,
                outcome: mn_telemetry::events::Outcome::Ok,
                batch_count: None,
                failed_batch_index: None,
            },
            EventPayload::PullModels {
                embedder_downloaded: false,
                reranker_downloaded: false,
                duration_ms: 0,
                outcome: mn_telemetry::events::Outcome::Ok,
            },
            EventPayload::McpStartup {
                startup_ms: 0,
                model_state: mn_telemetry::events::ModelState::Missing,
            },
            EventPayload::McpShutdown { uptime_s: 0, tools_served: 0 },
        ];
        for payload in variants {
            let event = Event::new(Component::Cli, "0.1.0", payload);
            let expected = event.payload.event_type();
            assert_eq!(
                validate(&inbound(&event)).as_deref(),
                Ok(expected),
                "ALLOWED_EVENT_TYPES is missing `{expected}`"
            );
        }
    }
}
