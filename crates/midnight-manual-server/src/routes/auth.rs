//! Auth endpoints — Ed25519 challenge-response (FR-056).
//!
//! `POST /v1/auth/challenge` mints a single-use 32-byte nonce keyed by
//! `user_id`. The server returns `{challenge_id, nonce_b64}`; the client
//! signs `nonce_b64` (decoded back to bytes) with their Ed25519 private
//! key.
//!
//! `POST /v1/auth/verify` consumes the challenge (single-use semantics —
//! re-using a `challenge_id` always fails), looks up the user's public key
//! in the in-memory user store, verifies the signature, and on success
//! mints a 1-hour HS256 JWT.
//!
//! Both endpoints return 503 `service_unavailable` when the server was
//! booted without `MIDNIGHT_MANUAL_USER_STORE` and
//! `MIDNIGHT_MANUAL_JWT_SECRET` — anonymous / read-only deployments stay
//! useful without admin auth configured.
//!
//! GitHub OAuth (FR-062 / FR-117) lands in a follow-up PR.

use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mnm_auth::{
    mint_jwt, parse_public_key_wire, verify_signature, ChallengeError, Claims, KeyError,
    DEFAULT_ADMIN_TTL, SIGNATURE_LEN,
};
use mnm_core::error::{Error as CoreError, ErrorCode};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the auth routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/challenge", post(challenge))
        .route("/v1/auth/verify", post(verify))
}

#[derive(Debug, Deserialize)]
struct ChallengeRequest {
    user_id: String,
}

#[derive(Debug, Serialize)]
struct ChallengeResponse {
    /// Opaque server identifier; client returns this on `verify`.
    challenge_id: String,
    /// 32 bytes of random nonce, base64 (no padding). Client decodes,
    /// signs, returns `signature_b64`.
    nonce_b64: String,
    /// Token expires (in seconds from now) if not consumed.
    expires_in_s: i64,
}

async fn challenge(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    Json(req): Json<ChallengeRequest>,
) -> Response {
    let rid = req_id.as_str();

    let Some(auth) = state.auth.as_ref() else {
        return error::service_unavailable("admin auth is not configured on this server", rid);
    };

    if req.user_id.trim().is_empty() {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("user_id must be non-empty")
                .remediation("supply your registered user_id (see your user-store row)")
                .build(),
            rid,
        );
    }

    // The user store is loaded once at boot. We refuse to mint a challenge
    // for a user that doesn't exist so a brute-force enumeration of
    // user_ids can't be done against the verify endpoint — both endpoints
    // give the same `not_found` shape (spec doesn't require timing-uniform
    // responses, but the surface is identical between known and unknown
    // user_ids until the verify step).
    if auth.user_store.get(&req.user_id).is_none() {
        return error::not_found(format!("user `{}` not found in user store", req.user_id), rid);
    }

    let now = OffsetDateTime::now_utc();
    let challenge = auth
        .challenges
        .mint(&req.user_id, now, time::Duration::seconds(60));
    let nonce_b64 = STANDARD_NO_PAD.encode(challenge.nonce);
    let body = ChallengeResponse {
        challenge_id: challenge.challenge_id,
        nonce_b64,
        expires_in_s: (challenge.expires_at - now).whole_seconds(),
    };
    Json(body).into_response()
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    challenge_id: String,
    /// Caller's Ed25519 signature over the nonce, base64 (with or without
    /// padding).
    signature_b64: String,
}

#[derive(Debug, Serialize)]
struct VerifyResponse {
    /// Bearer token. `Authorization: Bearer <token>` on every subsequent
    /// authenticated request.
    token: String,
    /// User id the token was minted for.
    user_id: String,
    /// Token expiry, Unix seconds. Matches the JWT `exp` claim.
    expires_at: i64,
}

#[allow(clippy::too_many_lines)]
async fn verify(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    Json(req): Json<VerifyRequest>,
) -> Response {
    let rid = req_id.as_str();

    let Some(auth) = state.auth.as_ref() else {
        return error::service_unavailable("admin auth is not configured on this server", rid);
    };

    // Decode the signature. Accept either padded or unpadded base64 to be
    // permissive on what clients produce.
    let sig_bytes = STANDARD_NO_PAD
        .decode(req.signature_b64.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&req.signature_b64))
        .map_err(|e| e.to_string());
    let signature_bytes = match sig_bytes {
        Ok(b) => b,
        Err(msg) => {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(format!("signature_b64 is not valid base64: {msg}"))
                    .remediation("sign the decoded nonce bytes and base64-encode the signature")
                    .build(),
                rid,
            );
        }
    };
    if signature_bytes.len() != SIGNATURE_LEN {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!(
                    "signature must be {SIGNATURE_LEN} bytes (got {})",
                    signature_bytes.len()
                ))
                .remediation("Ed25519 signatures are 64 bytes")
                .build(),
            rid,
        );
    }
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&signature_bytes);

    // Single-use consume. Whatever the outcome, the challenge is now gone
    // from the in-memory store.
    let now = OffsetDateTime::now_utc();
    let challenge = match auth.challenges.consume(&req.challenge_id, now) {
        Ok(c) => c,
        Err(ChallengeError::NotFound) => {
            return error::not_found(
                "challenge_id not found (already consumed or never minted)",
                rid,
            );
        }
        Err(ChallengeError::Expired) => {
            return error::into_response(
                CoreError::builder(ErrorCode::Unauthorized)
                    .message("challenge expired")
                    .remediation("call /v1/auth/challenge again and sign the new nonce within 60s")
                    .build(),
                rid,
            );
        }
    };

    // Look up the user's public key.
    let Some(user) = auth.user_store.get(&challenge.user_id) else {
        // Theoretically reachable only if the user store was reloaded
        // between mint and consume — we don't support that today, but a
        // future hot-reload feature would.
        return error::not_found(
            format!("user `{}` not found in user store", challenge.user_id),
            rid,
        );
    };
    let public = match parse_public_key_wire(&user.public_key) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(
                request_id = rid,
                user_id = %user.user_id,
                error = %e,
                "stored public_key failed to parse — user store is corrupt",
            );
            return error::service_unavailable("user store corrupt", rid);
        }
    };

    let verify_result = verify_signature(&public, &challenge.nonce, &signature);
    if matches!(verify_result, Err(KeyError::BadSignature)) {
        return error::into_response(
            CoreError::builder(ErrorCode::Forbidden)
                .message("signature did not verify under the registered public key")
                .remediation("re-check the local keypair against the user-store row")
                .build(),
            rid,
        );
    }

    let claims = Claims::admin(&user.user_id, user.role, now, DEFAULT_ADMIN_TTL);
    let token = match mint_jwt(&auth.jwt_secret, &claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(request_id = rid, error = %e, "jwt mint failed");
            return error::service_unavailable("jwt mint failed", rid);
        }
    };

    Json(VerifyResponse {
        token,
        user_id: user.user_id.clone(),
        expires_at: claims.exp,
    })
    .into_response()
}
