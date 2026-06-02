//! POST /v1/embeddings — server-side Voyage embedding with tiered token limits.
//!
//! Clients that cannot embed locally POST raw text here; the server calls Voyage
//! (BYOK, configured at boot) and returns the vectors plus the caller's
//! remaining token budget. Token accounting is per-subject (anon IP / SSO user /
//! admin) with rolling hourly + daily ceilings (see [`crate::tokenlimit`]).
//!
//! Privacy: rejection responses (429 over-budget) carry only window/limit/reset
//! metadata — never the submitted input text (Constitution VII).

use std::path::Path;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use mn_core::error::{Error as CoreError, ErrorCode};
use mn_embedding::voyage::InputType;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;
use crate::tokenlimit::{Reject, Window, WindowInfo};

/// Hard cap on inputs per request. Voyage itself caps a batch at 1 000 texts;
/// we refuse beyond that with 413 so the client batches rather than the upstream
/// returning an opaque error.
const MAX_INPUTS: usize = 1000;

const HDR_RETRY_AFTER: HeaderName = HeaderName::from_static("retry-after");
const HDR_TL_WINDOW: HeaderName = HeaderName::from_static("x-tokenlimit-window");
const HDR_TL_LIMIT: HeaderName = HeaderName::from_static("x-tokenlimit-limit");
const HDR_TL_RESET: HeaderName = HeaderName::from_static("x-tokenlimit-reset");

/// Mount the embeddings route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/embeddings", post(embeddings))
}

/// Request body for `POST /v1/embeddings`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingsRequest {
    /// Texts to embed. Must be non-empty and at most [`MAX_INPUTS`] entries.
    #[serde(default)]
    pub input: Vec<String>,
    /// Whether the inputs are `"query"` (default) or `"document"` texts. Voyage
    /// optimises embeddings differently per type; anything other than
    /// `"document"` is treated as `"query"`.
    #[serde(default = "default_input_type")]
    pub input_type: String,
    /// Optional client-asserted model identifier. When present it MUST match the
    /// corpus's active model wire id, else the request is rejected (409).
    #[serde(default)]
    pub model: Option<String>,
}

fn default_input_type() -> String {
    "query".to_owned()
}

/// Response body for a successful embedding request.
#[derive(Debug, Serialize)]
pub struct EmbeddingsResponse {
    /// The corpus model wire id the vectors are encoded with (e.g.
    /// `"voyage-code-3@1"`).
    pub model: String,
    /// One vector per input text, in the same order as `input`.
    pub embeddings: Vec<Vec<f32>>,
    /// Token usage for this request.
    pub usage: Usage,
    /// The caller's remaining token budget after this request was charged.
    pub rate: Rate,
}

/// Token usage for a single request.
#[derive(Debug, Serialize)]
pub struct Usage {
    /// Total tokens Voyage reported consuming for this request.
    pub total_tokens: u64,
}

/// Both rolling-window budgets for the caller.
#[derive(Debug, Serialize)]
pub struct Rate {
    /// Rolling 60-minute window.
    pub hour: RateWindow,
    /// Rolling 24-hour window.
    pub day: RateWindow,
}

/// One rolling-window budget snapshot.
#[derive(Debug, Serialize)]
pub struct RateWindow {
    /// Configured token ceiling for this window.
    pub limit: u64,
    /// Tokens remaining before the window is exhausted.
    pub remaining: u64,
    /// RFC3339 timestamp when the window's oldest bucket expires.
    pub reset_at: String,
}

impl From<WindowInfo> for RateWindow {
    fn from(w: WindowInfo) -> Self {
        Self {
            limit: w.limit,
            remaining: w.remaining,
            reset_at: iso(w.reset_at_secs),
        }
    }
}

/// Handler for `POST /v1/embeddings`.
///
/// The `Json` body extractor is intentionally last (it consumes the request),
/// after `State`, `RequestId`, `headers`, and the optional `AuthContext`.
#[allow(clippy::too_many_lines)]
async fn embeddings(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<EmbeddingsRequest>,
) -> Response {
    let rid = req_id.as_str();

    // 1. Server-side embedding must be configured (VOYAGE_API_KEY present).
    let Some(voyage) = state.voyage.clone() else {
        return error::service_unavailable(
            "server embedding is not configured (no VOYAGE_API_KEY)",
            rid,
        );
    };

    // 2. Reject an empty batch up front.
    if req.input.is_empty() {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("input must be non-empty")
                .remediation("supply one or more strings in the `input` array")
                .build(),
            rid,
        );
    }

    // 3. Refuse oversize batches with 413 so the client batches client-side.
    if req.input.len() > MAX_INPUTS {
        return error::payload_too_large(
            format!("input exceeds {MAX_INPUTS} texts; batch client-side"),
            rid,
        );
    }

    // 4. Snapshot the corpus model and enforce the optional model assertion.
    let snapshot = state
        .corpus_model
        .read()
        .expect("corpus_model lock poisoned")
        .clone();
    let Some(cm) = snapshot else {
        return error::service_unavailable(
            "server has no resolved corpus_model; check boot logs",
            rid,
        );
    };
    if let Some(client_model) = req.model.as_ref() {
        if client_model != &cm.wire {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!(
                        "model `{client_model}` does not match corpus model `{}`",
                        cm.wire,
                    ))
                    .remediation("omit `model` or set it to the corpus model wire id")
                    .context("corpus_model", cm.wire.clone())
                    .context("client_model", client_model.clone())
                    .build(),
                rid,
            );
        }
    }

    // 5. Resolve the token subject + effective limits for this caller.
    let client_ip =
        crate::middleware::rate_limit::client_ip(&headers, &state.cfg.rate_limit_client_ip_header);
    let auth_ctx = auth.as_ref().map(|Extension(c)| c.clone());
    let (subject, _tier, limits) = state.token_limiter.resolve(&client_ip, auth_ctx.as_ref());

    // 6. Single timestamp for the whole request so check/charge/snapshot agree.
    let now = OffsetDateTime::now_utc().unix_timestamp();

    // 7. Best-effort pre-count: reject before calling Voyage if the estimate
    //    already blows the budget. A 0 estimate (no tokenizer available) falls
    //    through to the post-charge accounting — by design (remaining > 0 path).
    let estimate = count_tokens_best_effort(&req.input, &state.cache_dir).unwrap_or(0) as u64;
    if let Err(rej) = state.token_limiter.check(&subject, limits, estimate, now) {
        return token_limit_429(&rej, now, rid);
    }

    // 8. Call Voyage. Anything other than success maps to 502 (upstream fault).
    let input_type = if req.input_type == "document" {
        InputType::Document
    } else {
        InputType::Query
    };
    let out = match voyage.embed(req.input.clone(), input_type).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "voyage embedding failed");
            return error::bad_gateway(format!("voyage embedding failed: {e}"), rid);
        }
    };

    // 9. Charge the actual tokens consumed, then snapshot the post-charge budget.
    state.token_limiter.charge(&subject, out.total_tokens, now);
    let info = state.token_limiter.snapshot_for(&subject, limits, now);

    // 10. Respond. Report the corpus model wire id (not Voyage's raw model name)
    //     so the client can pin vectors to the corpus.
    Json(EmbeddingsResponse {
        model: cm.wire,
        embeddings: out.vectors,
        usage: Usage { total_tokens: out.total_tokens },
        rate: Rate {
            hour: info.hour.into(),
            day: info.day.into(),
        },
    })
    .into_response()
}

/// Format unix-seconds as an RFC3339 timestamp. Returns an empty string for
/// out-of-range inputs rather than panicking.
fn iso(secs: i64) -> String {
    OffsetDateTime::from_unix_timestamp(secs)
        .map(|t| t.format(&Rfc3339).unwrap_or_default())
        .unwrap_or_default()
}

/// Build the 429 over-budget response.
///
/// Mirrors the rate-limit middleware's 429: a [`CoreError`] with
/// [`ErrorCode::RateLimited`], a `Retry-After` header, and `x-tokenlimit-*`
/// headers. PRIVACY: the body carries only window / limit / reset metadata —
/// never the submitted input text.
fn token_limit_429(rej: &Reject, now: i64, rid: &str) -> Response {
    let window = match rej.window {
        Window::Hour => "hour",
        Window::Day => "day",
    };
    // Seconds until the window resets, floored at 0 (never negative on the wire).
    let retry_after = rej.reset_at_secs.saturating_sub(now).max(0);
    let reset_at = iso(rej.reset_at_secs);

    let err = CoreError::builder(ErrorCode::RateLimited)
        .message(format!("token limit exceeded for the {window} window ({} tokens)", rej.limit))
        .remediation("retry after the window resets; see Retry-After header")
        .context("error", "token_limit_exceeded")
        .context("window", window)
        .context("limit", rej.limit)
        .context("reset_at", reset_at.clone())
        .build();
    let mut resp = error::into_response(err, rid);
    let h = resp.headers_mut();
    set_str(h, &HDR_RETRY_AFTER, &retry_after.to_string());
    set_str(h, &HDR_TL_WINDOW, window);
    set_str(h, &HDR_TL_LIMIT, &rej.limit.to_string());
    set_str(h, &HDR_TL_RESET, &reset_at);
    resp
}

fn set_str(headers: &mut HeaderMap, name: &HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(name.clone(), v);
    }
}

/// Best-effort token pre-count for the inputs.
///
/// When a voyage tokenizer is available under the model cache, sum its token
/// counts for an exact pre-check. No such tokenizer ships today (and
/// `tokenizers` is not a direct dependency of this crate), so this always
/// returns `None` for now — the design accepts the `estimate = 0` fallback,
/// which lets the request through to be charged on Voyage's reported count.
// Intentionally not `const`: the real implementation will read a tokenizer file
// off disk (non-const I/O). The stub returns `None` until that lands, so the
// const-fn lint would push us toward a signature we'd have to revert.
#[allow(clippy::unnecessary_wraps, clippy::missing_const_for_fn)]
fn count_tokens_best_effort(_inputs: &[String], _cache_dir: &Path) -> Option<usize> {
    // TODO(token pre-count): load the voyage tokenizer (tokenizer.json under the
    // model cache) and sum token counts when one is available.
    None
}
