//! Health endpoints — `/healthz` (liveness) and `/readyz` (readiness, DB ping
//! + pgvector extension presence).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;

use crate::app::AppState;

/// Mount the health routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// `/healthz` — always returns 200 as long as the process is alive.
async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// `/readyz` — 200 only if the DB is reachable AND the pgvector extension is
/// installed. Returns 503 on either failure.
async fn readyz(State(state): State<AppState>) -> StatusCode {
    let row: Result<(bool,), sqlx::Error> =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'vector')")
            .fetch_one(&state.pool)
            .await;

    match row {
        Ok((true,)) => StatusCode::OK,
        Ok((false,)) => StatusCode::SERVICE_UNAVAILABLE,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
