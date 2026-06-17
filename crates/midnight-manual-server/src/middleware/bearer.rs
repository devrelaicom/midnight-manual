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
//!   expired, malformed), the middleware short-circuits with a 401 so
//!   clients see the same shape regardless of which check tripped.
//!
//! The middleware is a no-op when auth isn't configured (i.e. the server
//! was booted without `MIDNIGHT_MANUAL_USER_STORE` /
//! `MIDNIGHT_MANUAL_JWT_SECRET`); the request continues anonymous.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use mnm_auth::{verify_jwt, Role, Tier};
use time::OffsetDateTime;

use crate::app::AppState;
use crate::middleware::request_id::RequestId;

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
pub async fn layer(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    // Anonymous mode if auth subsystem isn't configured. The handler stack
    // will still answer reads; protected routes 401 on missing extension.
    let Some(auth) = state.auth.as_ref() else {
        return next.run(req).await;
    };

    // Absent header → anonymous. Present but malformed → 401.
    let Some(header_value) = req.headers().get(header::AUTHORIZATION) else {
        return next.run(req).await;
    };
    let Ok(header_str) = header_value.to_str() else {
        return unauthorized_response(&req, "Authorization header is not valid UTF-8");
    };
    let Some(token) = header_str.strip_prefix("Bearer ") else {
        return unauthorized_response(&req, "Authorization header must use `Bearer <jwt>`");
    };

    match verify_jwt(&auth.jwt_secret, token, OffsetDateTime::now_utc()) {
        Ok(claims) => {
            req.extensions_mut().insert(AuthContext {
                sub: claims.sub,
                role: claims.role,
                tier: claims.tier,
                jti: claims.jti,
            });
            next.run(req).await
        }
        Err(e) => {
            tracing::warn!(error = %e, "jwt verification failed");
            unauthorized_response(&req, "Run `mnm login` to obtain a fresh token")
        }
    }
}

fn unauthorized_response(req: &Request, remediation: &str) -> Response {
    let rid = req
        .extensions()
        .get::<RequestId>()
        .map_or("", RequestId::as_str);
    // mnm-core's `unauthorized` builder isn't a thing yet; we hand-roll the
    // 401 here. The shape matches the typed envelope from other routes.
    let body = serde_json::json!({
        "error": {
            "code": "unauthorized",
            "message": "missing or invalid Authorization bearer",
            "remediation": remediation,
        },
        "request_id": rid,
    });
    (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
}
