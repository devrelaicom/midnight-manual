//! Bearer-token extraction middleware (FR-058, FR-117).
//!
//! Behavior:
//!
//! - When `Authorization: Bearer <jwt>` is present and the JWT verifies
//!   under [`AppState`]'s signing secret, an [`AuthContext`] is inserted
//!   into the request's extension map and the request continues.
//! - When the header is absent, the request continues unauthenticated —
//!   protected handlers opt in via the [`AuthContext`] extractor (which
//!   returns 401 if the extension isn't present).
//! - When the header is present but the JWT is rejected (bad signature,
//!   expired, malformed), the middleware does NOT short-circuit. Instead it
//!   stashes a [`BearerRejection`] and continues, so the inner
//!   [`rate_limit`](crate::middleware::rate_limit) layer still charges the
//!   caller's bucket before the 401 is returned (issue #176 L14). Without this,
//!   a client spamming `Authorization: Bearer <garbage>` would 401 for free and
//!   never be charged against any rate-limit bucket. The rate-limit layer emits
//!   the 401 (via [`unauthorized_response`]) once it has charged the request, so
//!   the on-the-wire 401 shape is unchanged.
//!
//! The middleware is a no-op when auth isn't configured (i.e. the server
//! was booted without `MIDNIGHT_MANUAL_USER_STORE` /
//! `MIDNIGHT_MANUAL_JWT_SECRET`); the request continues anonymous.

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use mnm_auth::{verify_jwt, Role, SigningSecret, Tier};
use time::OffsetDateTime;

use crate::app::AppState;

/// Resolved bearer-derived identity. Inserted into request extensions by
/// [`layer`]; extract via `Extension<AuthContext>` in handlers that require
/// authentication.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// JWT `sub` claim — the user_id for admin tokens, or the GitHub login
    /// for read-uplift tokens.
    pub sub: String,
    /// Caller role (admin / writer).
    pub role: Role,
    /// Caller tier (admin / read_uplift). Write endpoints MUST refuse any
    /// request whose tier is `read_uplift` before consulting the role.
    pub tier: Tier,
    /// JWT id — useful for future revocation lists / audit logging.
    pub jti: String,
}

/// A deferred bearer-auth failure. Inserted into request extensions by
/// [`layer`] when an `Authorization` header is present but unusable (not UTF-8,
/// not a `Bearer` token, or a JWT that fails verification). The inner
/// [`rate_limit`](crate::middleware::rate_limit) layer charges the request,
/// then converts this marker into the 401 via [`unauthorized_response`] so that
/// invalid-bearer floods are rate-limited rather than answered for free.
#[derive(Debug, Clone, Copy)]
pub struct BearerRejection {
    /// Static remediation hint, forwarded verbatim into the 401 body.
    pub remediation: &'static str,
}

impl AuthContext {
    /// Whether the caller may perform admin operations
    /// (`/v1/admin/*`). True only for `tier = Admin && role = Admin`.
    #[must_use]
    pub const fn can_admin(&self) -> bool {
        self.tier.can_write() && self.role.can_admin()
    }

    /// Whether the caller may perform write operations (ingest /
    /// source-version lifecycle). Requires both an admin-tier token and a
    /// writer-or-higher role.
    #[must_use]
    pub const fn can_write(&self) -> bool {
        self.tier.can_write() && self.role.can_write()
    }
}

/// Axum middleware function. Wire via
/// `axum::middleware::from_fn_with_state(state.clone(), bearer::layer)`.
///
/// This layer runs OUTSIDE (before) the rate-limit layer, so a valid token's
/// [`AuthContext`] is available when the limiter resolves the caller's tier.
/// On a bearer failure it stashes a [`BearerRejection`] and continues rather
/// than returning 401 itself, letting the rate-limit layer charge the request
/// first (issue #176 L14).
pub async fn layer(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    // Anonymous mode if auth subsystem isn't configured. The handler stack
    // will still answer reads; protected routes 401 on missing extension.
    let Some(auth) = state.auth.as_ref() else {
        return next.run(req).await;
    };

    // Classify the header into an OWNED outcome first, so no borrow into
    // `req.headers()` is held across the `extensions_mut()` mutation below.
    match classify(req.headers(), &auth.jwt_secret) {
        BearerOutcome::Absent => {}
        BearerOutcome::Authed(ctx) => {
            req.extensions_mut().insert(ctx);
        }
        BearerOutcome::Rejected(remediation) => {
            req.extensions_mut().insert(BearerRejection { remediation });
        }
    }
    next.run(req).await
}

/// The result of inspecting the `Authorization` header, decoupled from `req`
/// mutation so the borrow of `req.headers()` ends before we touch extensions.
enum BearerOutcome {
    /// No `Authorization` header — the request continues anonymous.
    Absent,
    /// A verified token, ready to be stashed as an [`AuthContext`].
    Authed(AuthContext),
    /// A present-but-unusable header — a 401 deferred to the rate-limit layer,
    /// carrying the remediation hint.
    Rejected(&'static str),
}

fn classify(headers: &HeaderMap, secret: &SigningSecret) -> BearerOutcome {
    let Some(header_value) = headers.get(header::AUTHORIZATION) else {
        return BearerOutcome::Absent;
    };
    let Ok(header_str) = header_value.to_str() else {
        return BearerOutcome::Rejected("Authorization header is not valid UTF-8");
    };
    let Some(token) = header_str.strip_prefix("Bearer ") else {
        return BearerOutcome::Rejected("Authorization header must use `Bearer <jwt>`");
    };
    match verify_jwt(secret, token, OffsetDateTime::now_utc()) {
        Ok(claims) => BearerOutcome::Authed(AuthContext {
            sub: claims.sub,
            role: claims.role,
            tier: claims.tier,
            jti: claims.jti,
        }),
        Err(e) => {
            tracing::warn!(error = %e, "jwt verification failed");
            BearerOutcome::Rejected("Run `mnm login` to obtain a fresh token")
        }
    }
}

/// Build the shared 401 body. Called by the rate-limit layer once it has
/// charged a request carrying a deferred [`BearerRejection`] (and directly in
/// its pass-through paths when rate limiting is disabled). Kept here so the
/// bearer-failure envelope has a single definition.
pub(crate) fn unauthorized_response(request_id: &str, remediation: &str) -> Response {
    // mnm-core's `unauthorized` builder isn't a thing yet; we hand-roll the
    // 401 here. The shape matches the typed envelope from other routes.
    let body = serde_json::json!({
        "error": {
            "code": "unauthorized",
            "message": "missing or invalid Authorization bearer",
            "remediation": remediation,
        },
        "request_id": request_id,
    });
    (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
}
