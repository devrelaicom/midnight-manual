//! GitHub OAuth endpoints (FR-062, FR-115, FR-117).
//!
//! `GET /v1/auth/github/start`
//!     Mints a CSRF state, redirects the user-agent to GitHub's authorize
//!     URL. Optional `cli_port` query param is preserved in state so the
//!     callback can redirect back to a CLI's local listener.
//!
//! `GET /v1/auth/github/callback`
//!     Receives `code` + `state` from GitHub, consumes the state
//!     (single-use), exchanges the code for an access token, verifies the
//!     user is an `active` member of the configured org, mints a 30-day
//!     read-uplift JWT, and either redirects the browser to the CLI's
//!     local listener (when `cli_port` was set) or returns a JSON body
//!     for manual / scripted use.
//!
//! Both endpoints 503 when GitHub OAuth env is not configured. The org
//! gate is mandatory (FR-062): a non-member receives 403 and no token.
//!
//! ## Security notes
//!
//! - `state` is a server-minted UUID; we never trust a client-supplied
//!   value. The CSRF check is "does this state exist in our store" plus
//!   the implicit binding that the user-agent's GitHub session must have
//!   produced the matching callback.
//!
//! - `cli_port` redirects target `127.0.0.1` only. We refuse to redirect
//!   to any other host even if someone tampers with the state store.
//!
//! - The access token GitHub mints for us never leaves the process — we
//!   use it for the two API calls (`/user`, `/user/memberships/orgs/<org>`)
//!   and drop it. The JWT we mint is keyed to the GitHub login, not the
//!   GitHub access token.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_auth::{mint_jwt, Claims, OAuthStateError};
use mn_core::error::{Error as CoreError, ErrorCode};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::app::{AppState, AuthState, GithubOAuthState};
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the GitHub-OAuth routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/github/start", get(start))
        .route("/v1/auth/github/callback", get(callback))
}

#[derive(Debug, Deserialize)]
struct StartQuery {
    /// Optional CLI local-listener port. When set, the callback redirects
    /// the user-agent to `http://127.0.0.1:<cli_port>/oauth?…` after a
    /// successful exchange.
    #[serde(default)]
    cli_port: Option<u16>,
}

async fn start(
    State(state): State<AppState>,
    axum::extract::Extension(req_id): axum::extract::Extension<RequestId>,
    Query(q): Query<StartQuery>,
) -> Response {
    let rid = req_id.as_str();
    let Some(gh) = github_state(&state) else {
        return error::service_unavailable("github oauth is not configured on this server", rid);
    };

    let now = OffsetDateTime::now_utc();
    let entry = gh
        .states
        .mint(q.cli_port, now, mn_auth::OAUTH_STATE_DEFAULT_TTL);

    // Build the authorize URL. We request `read:org` so the membership
    // probe succeeds; that's the minimum scope for `GET
    // /user/memberships/orgs/<org>`.
    let authorize_url = match url::Url::parse_with_params(
        &gh.authorize_url,
        &[
            ("client_id", gh.client_id.as_str()),
            ("redirect_uri", gh.redirect_url.as_str()),
            ("scope", "read:org"),
            ("state", entry.state_id.as_str()),
        ],
    ) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(request_id = rid, error = %e, "build github authorize url");
            return error::service_unavailable("github authorize url invalid", rid);
        }
    };
    Redirect::to(authorize_url.as_str()).into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    /// GitHub forwards `error` when the user denies consent.
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Serialize)]
struct CallbackBody {
    /// The minted read-uplift JWT.
    token: String,
    /// The authenticated GitHub login.
    github_login: String,
    /// Unix-seconds expiry of the JWT.
    expires_at: i64,
}

#[allow(clippy::too_many_lines)]
async fn callback(
    State(state): State<AppState>,
    axum::extract::Extension(req_id): axum::extract::Extension<RequestId>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let rid = req_id.as_str();
    let Some(auth) = state.auth.as_deref() else {
        return error::service_unavailable("admin auth is not configured on this server", rid);
    };
    let Some(gh) = auth.github_oauth.as_ref() else {
        return error::service_unavailable("github oauth is not configured on this server", rid);
    };

    if let Some(err) = q.error.as_deref() {
        return error::into_response(
            CoreError::builder(ErrorCode::Forbidden)
                .message(format!(
                    "github denied the oauth grant: {err}{}",
                    q.error_description
                        .as_deref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default()
                ))
                .remediation("re-run `mnm auth github` and approve the consent prompt")
                .build(),
            rid,
        );
    }
    let Some(code) = q.code.as_deref().filter(|s| !s.is_empty()) else {
        return invalid_request("missing `code` query parameter", rid);
    };
    let Some(state_id) = q.state.as_deref().filter(|s| !s.is_empty()) else {
        return invalid_request("missing `state` query parameter", rid);
    };

    let now = OffsetDateTime::now_utc();
    let oauth_state = match gh.states.consume(state_id, now) {
        Ok(s) => s,
        Err(OAuthStateError::NotFound) => {
            return invalid_request(
                "state token not found (already consumed or never minted)",
                rid,
            );
        }
        Err(OAuthStateError::Expired) => {
            return error::into_response(
                CoreError::builder(ErrorCode::Unauthorized)
                    .message("state token expired")
                    .remediation("re-run `mnm auth github`; the consent flow must complete inside 10 minutes")
                    .build(),
                rid,
            );
        }
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(request_id = rid, error = %e, "build http client");
            return error::service_unavailable("github oauth http client unavailable", rid);
        }
    };

    let access_token = match exchange_code(&client, gh, code).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "github code exchange failed");
            return error::into_response(
                CoreError::builder(ErrorCode::Unauthorized)
                    .message(format!("github code exchange failed: {e}"))
                    .remediation("re-run `mnm auth github`")
                    .build(),
                rid,
            );
        }
    };

    let github_login = match fetch_login(&client, gh, &access_token).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "github /user fetch failed");
            return error::into_response(
                CoreError::builder(ErrorCode::Unauthorized)
                    .message(format!("github /user fetch failed: {e}"))
                    .remediation("re-run `mnm auth github`; GitHub may be degraded")
                    .build(),
                rid,
            );
        }
    };

    if let Err(e) = check_org_membership(&client, gh, &access_token, &gh.org).await {
        tracing::info!(
            request_id = rid,
            github_login = %github_login,
            org = %gh.org,
            error = %e,
            "github org membership check failed",
        );
        return error::into_response(
            CoreError::builder(ErrorCode::Forbidden)
                .message(format!(
                    "github user `{github_login}` is not an active member of org `{}`",
                    gh.org,
                ))
                .remediation(format!(
                    "ask a maintainer to invite you to the `{}` GitHub org",
                    gh.org,
                ))
                .build(),
            rid,
        );
    }

    let claims = Claims::read_uplift(&github_login, now, gh.read_token_ttl);
    let token = match mint_jwt(&auth.jwt_secret, &claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(request_id = rid, error = %e, "jwt mint failed");
            return error::service_unavailable("jwt mint failed", rid);
        }
    };

    if let Some(port) = oauth_state.cli_port {
        let exp_str = claims.exp.to_string();
        let cli_url = match url::Url::parse_with_params(
            &format!("http://127.0.0.1:{port}/oauth"),
            &[
                ("token", token.as_str()),
                ("github_login", github_login.as_str()),
                ("expires_at", exp_str.as_str()),
            ],
        ) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(request_id = rid, error = %e, "build cli callback url");
                return error::service_unavailable("cli callback url invalid", rid);
            }
        };
        return Redirect::to(cli_url.as_str()).into_response();
    }

    Json(CallbackBody {
        token,
        github_login,
        expires_at: claims.exp,
    })
    .into_response()
}

fn github_state(state: &AppState) -> Option<&GithubOAuthState> {
    state
        .auth
        .as_deref()
        .and_then(|a: &AuthState| a.github_oauth.as_ref())
}

fn invalid_request(msg: impl Into<String>, rid: &str) -> Response {
    let core = CoreError::builder(ErrorCode::InvalidRequest)
        .message(msg)
        .remediation("re-run `mnm auth github` and let the flow complete")
        .build();
    error::into_response(core, rid)
}

#[derive(Debug, Deserialize)]
struct ExchangeResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn exchange_code(
    client: &reqwest::Client,
    gh: &GithubOAuthState,
    code: &str,
) -> Result<String, String> {
    let resp = client
        .post(&gh.token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("client_id", gh.client_id.as_str()),
            ("client_secret", gh.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", gh.redirect_url.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("token endpoint returned {status}"));
    }
    let body: ExchangeResponse = resp
        .json()
        .await
        .map_err(|e| format!("parse token response: {e}"))?;
    if let Some(err) = body.error {
        let desc = body.error_description.unwrap_or_default();
        return Err(format!("github reported `{err}`: {desc}"));
    }
    body.access_token
        .ok_or_else(|| "token endpoint returned no access_token".to_owned())
}

#[derive(Debug, Deserialize)]
struct UserResponse {
    login: Option<String>,
}

async fn fetch_login(
    client: &reqwest::Client,
    gh: &GithubOAuthState,
    access_token: &str,
) -> Result<String, String> {
    let resp = client
        .get(format!("{}/user", gh.api_base_url))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "midnight-manual")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("/user returned {}", resp.status()));
    }
    let body: UserResponse = resp.json().await.map_err(|e| format!("parse /user: {e}"))?;
    body.login
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "/user returned no login".to_owned())
}

#[derive(Debug, Deserialize)]
struct MembershipResponse {
    state: Option<String>,
}

async fn check_org_membership(
    client: &reqwest::Client,
    gh: &GithubOAuthState,
    access_token: &str,
    org: &str,
) -> Result<(), String> {
    let resp = client
        .get(format!("{}/user/memberships/orgs/{org}", gh.api_base_url))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "midnight-manual")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        return Err("not a member".to_owned());
    }
    if !status.is_success() {
        return Err(format!("membership endpoint returned {status}"));
    }
    let body: MembershipResponse = resp
        .json()
        .await
        .map_err(|e| format!("parse membership: {e}"))?;
    match body.state.as_deref() {
        Some("active") => Ok(()),
        Some(other) => Err(format!("membership state is `{other}`, expected `active`")),
        None => Err("membership response missing `state`".to_owned()),
    }
}
