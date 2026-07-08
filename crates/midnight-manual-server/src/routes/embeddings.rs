//! POST /v1/embeddings — server-side Voyage embedding with tiered token limits.
//!
//! Clients that cannot embed locally POST raw text here; the server calls Voyage
//! (BYOK, configured at boot) and returns the vectors plus the caller's
//! remaining token budget. Token accounting is per-subject (anon IP / SSO user /
//! admin) with rolling hourly + daily ceilings (see [`crate::tokenlimit`]).
//!
//! Privacy: rejection responses (429 over-budget) carry only window/limit/reset
//! metadata — never the submitted input text (Constitution VII).

use std::net::SocketAddr;
use std::path::Path;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use mnm_core::error::{Error as CoreError, ErrorCode};
use mnm_embedding::voyage::InputType;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;
use crate::tokenlimit::{Reject, TokenTier, Window, WindowInfo};

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

/// Whether the request targets the general (contextualized) or code model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedType {
    /// voyage-context-3 via `/v1/contextualizedembeddings`. The default.
    #[default]
    General,
    /// voyage-code-3 via the flat `/v1/embeddings`.
    Code,
}

/// Flat texts, or caller-provided context groups (general type only).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingsInput {
    /// Each string is its own single-chunk document (correct for queries).
    Flat(Vec<String>),
    /// Caller-provided context groups (server-proxy ingestion).
    Nested(Vec<Vec<String>>),
}

impl Default for EmbeddingsInput {
    fn default() -> Self {
        Self::Flat(Vec::new())
    }
}

impl EmbeddingsInput {
    fn is_empty(&self) -> bool {
        match self {
            Self::Flat(v) => v.is_empty(),
            Self::Nested(g) => g.iter().all(Vec::is_empty),
        }
    }

    /// Total text count across both shapes (nested groups flattened).
    fn flat_len(&self) -> usize {
        match self {
            Self::Flat(v) => v.len(),
            Self::Nested(g) => g.iter().map(Vec::len).sum(),
        }
    }
}

/// Request body for `POST /v1/embeddings`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingsRequest {
    /// Texts to embed: a flat string array, or (general type only) nested
    /// context groups. Must be non-empty and at most 1000 texts total (the
    /// `MAX_INPUTS` batch cap).
    #[serde(default)]
    pub input: EmbeddingsInput,
    /// Which embedding model family to use: `"general"` (default;
    /// voyage-context-3 contextualized) or `"code"` (voyage-code-3 flat).
    #[serde(rename = "type", default)]
    pub embed_type: EmbedType,
    /// Whether the inputs are `"query"` (default) or `"document"` texts. Voyage
    /// optimises embeddings differently per type; anything other than
    /// `"document"` is treated as `"query"`.
    #[serde(default = "default_input_type")]
    pub input_type: String,
    /// Optional client-asserted model identifier. When present it MUST match
    /// the active model for the requested `type` (the corpus model for
    /// `general`, the code model for `code`), else the request is rejected
    /// (409).
    #[serde(default)]
    pub model: Option<String>,
    /// Admin-only opt-out from the site-wide token cap; ignored unless the
    /// caller is admin-tier. The server checks the admin role, not just the
    /// flag, so a non-admin setting it has no effect (the request is still
    /// counted against the global cap).
    #[serde(default)]
    pub no_global_limit: bool,
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
    // NOTE: reads only the connect-info Extension set by
    // `into_make_service_with_connect_info` (production); it does NOT observe
    // axum's `MockConnectInfo` test helper — inject a peer addr in tests via
    // `.layer(Extension(ConnectInfo(addr)))`, not `MockConnectInfo`.
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(req): Json<EmbeddingsRequest>,
) -> Response {
    let rid = req_id.as_str();

    // 1. The embedder for the requested type must be configured.
    //    (Both are None iff VOYAGE_API_KEY is unset; the per-type check is in
    //    step 8.)
    if state.voyage.is_none() && state.voyage_ctx.is_none() {
        return error::service_unavailable(
            "server embedding is not configured (no VOYAGE_API_KEY)",
            rid,
        );
    }

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

    // 3. Shape validation. Refuse oversize batches with 413 so the client
    //    batches client-side; nested context groups are general-type only and
    //    each group must fit the per-document context budget.
    if req.input.flat_len() > MAX_INPUTS {
        return error::payload_too_large(
            format!("input exceeds {MAX_INPUTS} texts; batch client-side"),
            rid,
        );
    }
    if matches!(req.embed_type, EmbedType::Code) && matches!(req.input, EmbeddingsInput::Nested(_))
    {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("nested input is only valid with type=general")
                .remediation("flatten `input` to a string array for type=code")
                .build(),
            rid,
        );
    }
    // Per-group budget (general/nested): ≈ tokens via the ~4-bytes/token
    // estimate; the 20% headroom on the real Voyage limit absorbs the
    // estimate's slack.
    if let EmbeddingsInput::Nested(groups) = &req.input {
        for (i, g) in groups.iter().enumerate() {
            if char_estimate(g) > u64::from(context_group_limit()) {
                return error::payload_too_large(
                    format!("input group {i} exceeds the per-document context limit; split it"),
                    rid,
                );
            }
        }
    }

    // 4. Snapshot the model for the requested type; enforce the optional pin.
    let resolved_wire: String = match req.embed_type {
        EmbedType::General => {
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
            cm.wire
        }
        EmbedType::Code => {
            let snapshot = state
                .code_model
                .read()
                .expect("code_model lock poisoned")
                .clone();
            let Some(cm) = snapshot else {
                return error::service_unavailable(
                    "server has no resolved code model; check boot logs",
                    rid,
                );
            };
            cm.wire
        }
    };
    if let Some(client_model) = req.model.as_ref() {
        if client_model != &resolved_wire {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!(
                        "model `{client_model}` does not match the active \
                         {} model `{resolved_wire}`",
                        match req.embed_type {
                            EmbedType::General => "corpus",
                            EmbedType::Code => "code",
                        },
                    ))
                    .remediation("omit `model` or set it to the active model wire id")
                    .context("corpus_model", resolved_wire.clone())
                    .context("client_model", client_model.clone())
                    .build(),
                rid,
            );
        }
    }

    // 5. Resolve the token subject, tier, and effective limits for this caller.
    let client_ip = crate::middleware::rate_limit::client_ip(
        &headers,
        &state.cfg.rate_limit_client_ip_header,
        peer.map(|Extension(ConnectInfo(sa))| sa.ip()),
    );
    let auth_ctx = auth.as_ref().map(|Extension(c)| c.clone());
    let (subject, tier, limits) = state.token_limiter.resolve(&client_ip, auth_ctx.as_ref());

    // The site-wide global cap counts EVERY tier by default. The only escape is
    // an explicit per-request opt-out (`no_global_limit`), honoured ONLY for
    // admin-tier callers — a non-admin setting the flag stays counted (the
    // server checks the admin role, not just the flag).
    let bypass_global = resolve_bypass_global(req.no_global_limit, tier);

    // 6. Single timestamp for the whole request so reserve/settle/snapshot agree.
    let now = OffsetDateTime::now_utc().unix_timestamp();

    // 7. Reserve an estimate up front (atomic against concurrent requests + the
    //    global cap), so a burst of large requests from one subject — or across
    //    rotated subjects — can't all pass and overshoot before they're charged.
    //    Exact pre-count via the Voyage tokenizer when available, else a
    //    char-based estimate (~4 bytes/token) so the gate still bites. Nested
    //    groups are flattened once here (token accounting is per-text either way).
    let flat_texts: std::borrow::Cow<'_, [String]> = match &req.input {
        EmbeddingsInput::Flat(v) => std::borrow::Cow::Borrowed(v.as_slice()),
        EmbeddingsInput::Nested(g) => {
            std::borrow::Cow::Owned(g.iter().flatten().cloned().collect())
        }
    };
    let estimate = count_tokens_best_effort(&flat_texts, &state.cache_dir)
        .map_or_else(|| char_estimate(&flat_texts), |n| n as u64);
    let reservation =
        match state
            .token_limiter
            .reserve(&subject, limits, estimate, now, bypass_global)
        {
            Ok(id) => id,
            Err(rej) => return token_limit_429(&rej, now, rid),
        };

    // 8. Call Voyage on the requested type's endpoint. Anything other than
    //    success releases the reservation; an unconfigured per-type embedder is
    //    503, an upstream fault is 502.
    let input_type = if req.input_type == "document" {
        InputType::Document
    } else {
        InputType::Query
    };
    let out = match (req.embed_type, &req.input) {
        (EmbedType::Code, EmbeddingsInput::Flat(texts)) => {
            let Some(voyage) = state.voyage.clone() else {
                state
                    .token_limiter
                    .release(&subject, reservation, bypass_global);
                return error::service_unavailable("code embedder not configured", rid);
            };
            voyage
                .embed(texts.clone(), input_type)
                .await
                .map(|o| (o.vectors, o.total_tokens))
        }
        (EmbedType::General, input) => {
            let Some(ctx) = state.voyage_ctx.clone() else {
                state
                    .token_limiter
                    .release(&subject, reservation, bypass_global);
                return error::service_unavailable("general embedder not configured", rid);
            };
            // Flat input = each text is its own single-chunk document (the
            // correct shape for queries); nested input passes through as the
            // caller's context groups.
            let groups: Vec<Vec<String>> = match input {
                EmbeddingsInput::Flat(texts) => texts.iter().cloned().map(|t| vec![t]).collect(),
                EmbeddingsInput::Nested(g) => g.clone(),
            };
            ctx.embed_groups(groups, input_type)
                .await
                .map(|o| (o.groups.into_iter().flatten().collect::<Vec<_>>(), o.total_tokens))
        }
        (EmbedType::Code, EmbeddingsInput::Nested(_)) => unreachable!("rejected in step 3"),
    };
    let (vectors, total_tokens) = match out {
        Ok(v) => v,
        Err(e) => {
            state
                .token_limiter
                .release(&subject, reservation, bypass_global);
            tracing::warn!(request_id = rid, error = %e, "voyage embedding failed");
            return error::bad_gateway(format!("voyage embedding failed: {e}"), rid);
        }
    };

    // 9. Settle the reservation with the ACTUAL tokens consumed, then snapshot
    //    the post-charge budget.
    state
        .token_limiter
        .settle(&subject, reservation, total_tokens, now, bypass_global);
    let info = state.token_limiter.snapshot_for(&subject, limits, now);

    // 10. Respond. Report the resolved model wire id for the requested type
    //     (not Voyage's raw model name) so the client can pin vectors to the
    //     corpus. `embeddings` is flattened row-per-chunk in input order for
    //     both shapes.
    Json(EmbeddingsResponse {
        model: resolved_wire,
        embeddings: vectors,
        usage: Usage { total_tokens },
        rate: Rate {
            hour: info.hour.into(),
            day: info.day.into(),
        },
    })
    .into_response()
}

/// 80% of voyage-context-3's 32K per-document (inner list) token limit.
/// Mirrors the budget in `mnm_content::context_group::context_group_limit` (kept
/// in lockstep; inlined so midnight-manual-server does not grow an mnm-content
/// dependency for one constant). This is a coarse `char_estimate` pre-filter, so
/// it is intentionally looser than the client's BPE-token sizing.
const fn context_group_limit() -> u32 {
    32_000 / 10 * 8
}

/// Resolve whether this request bypasses the site-wide global token cap.
///
/// The bypass is honoured ONLY when the caller explicitly requested it AND the
/// caller is admin-tier. A non-admin requesting it has no effect — the request
/// stays counted against the global cap (Constitution: anti-Sybil cost ceiling).
const fn resolve_bypass_global(requested: bool, tier: TokenTier) -> bool {
    requested && matches!(tier, TokenTier::Admin)
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
        Window::Global => "global",
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

/// Best-effort EXACT token pre-count for the inputs.
///
/// When a voyage tokenizer is available under the model cache, sum its token
/// counts for an exact reservation. No such tokenizer ships today (and
/// `tokenizers` is not a direct dependency of this crate), so this returns
/// `None`; the caller falls back to [`char_estimate`].
// Intentionally not `const`: the real implementation will read a tokenizer file
// off disk (non-const I/O). The stub returns `None` until that lands, so the
// const-fn lint would push us toward a signature we'd have to revert.
#[allow(clippy::unnecessary_wraps, clippy::missing_const_for_fn)]
fn count_tokens_best_effort(_inputs: &[String], _cache_dir: &Path) -> Option<usize> {
    // TODO(token pre-count): load the voyage tokenizer (tokenizer.json under the
    // model cache) and sum token counts when one is available.
    None
}

/// Rough token estimate from input byte length (~4 bytes/token, min 1 per
/// input) when no tokenizer is available. Used to size the up-front reservation
/// so concurrent requests gate against a non-zero figure; the durable charge is
/// always reconciled to Voyage's reported count, so an imprecise estimate only
/// affects in-flight concurrency gating, never the final balance.
fn char_estimate(inputs: &[String]) -> u64 {
    inputs
        .iter()
        .map(|s| (s.len() as u64).div_ceil(4).max(1))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `type` defaults to `general` and flat input deserializes as
    /// [`EmbeddingsInput::Flat`]; an explicit `"type": "code"` plus a nested
    /// array deserializes as `Code` + [`EmbeddingsInput::Nested`].
    #[test]
    fn embed_type_deserializes_with_default() {
        let r: EmbeddingsRequest =
            serde_json::from_value(serde_json::json!({"input": ["x"]})).unwrap();
        assert_eq!(r.embed_type, EmbedType::General);
        assert!(matches!(r.input, EmbeddingsInput::Flat(ref v) if v.len() == 1));

        let r: EmbeddingsRequest =
            serde_json::from_value(serde_json::json!({"input": [["a","b"]], "type": "code"}))
                .unwrap();
        assert_eq!(r.embed_type, EmbedType::Code);
        assert!(matches!(r.input, EmbeddingsInput::Nested(_)));
    }

    /// `is_empty` / `flat_len` see through both input shapes: nested groups
    /// count their flattened chunk total, and all-empty groups are empty.
    #[test]
    fn embeddings_input_shape_helpers() {
        let flat = EmbeddingsInput::Flat(vec!["a".into(), "b".into()]);
        assert!(!flat.is_empty());
        assert_eq!(flat.flat_len(), 2);

        let nested = EmbeddingsInput::Nested(vec![vec!["a".into(), "b".into()], vec!["c".into()]]);
        assert!(!nested.is_empty());
        assert_eq!(nested.flat_len(), 3);

        assert!(EmbeddingsInput::Flat(vec![]).is_empty());
        assert!(EmbeddingsInput::Nested(vec![vec![], vec![]]).is_empty());
        // A missing `input` field defaults to empty flat (then 400s in the handler).
        assert!(EmbeddingsInput::default().is_empty());
    }

    /// THE security property: the global-cap bypass is honoured ONLY when the
    /// caller is admin-tier. A non-admin sending `no_global_limit: true` must NOT
    /// escape the site-wide cap — `resolve_bypass_global` returns `false`, so the
    /// request stays counted (Constitution: anti-Sybil cost ceiling). Checking
    /// the flag alone — without also checking the admin role — would be the bug
    /// this test exists to prevent.
    #[test]
    fn non_admin_cannot_bypass_global_cap_even_with_flag() {
        // Anonymous tier with the flag set still does NOT bypass.
        assert!(
            !resolve_bypass_global(true, TokenTier::Anonymous),
            "anonymous + no_global_limit must stay counted (no bypass)"
        );
        // The SSO/read-uplift tier likewise cannot bypass with the flag.
        assert!(
            !resolve_bypass_global(true, TokenTier::ReadUplift),
            "read-uplift + no_global_limit must stay counted (no bypass)"
        );
    }

    /// Full (flag × tier) matrix. The bypass is `true` for exactly one cell:
    /// admin-tier AND the flag set. Every other combination is `false`.
    #[test]
    fn resolve_bypass_global_matrix() {
        // (requested, tier, expected_bypass)
        let cases = [
            (true, TokenTier::Admin, true),       // admin opts out -> bypass
            (false, TokenTier::Admin, false),     // admin didn't ask -> counted
            (true, TokenTier::Anonymous, false),  // non-admin can't bypass even with flag
            (true, TokenTier::ReadUplift, false), // same for the SSO/uplift tier
            (false, TokenTier::Anonymous, false),
            (false, TokenTier::ReadUplift, false),
        ];
        for (requested, tier, expected) in cases {
            assert_eq!(
                resolve_bypass_global(requested, tier),
                expected,
                "resolve_bypass_global({requested}, {tier:?}) should be {expected}"
            );
        }
    }
}
