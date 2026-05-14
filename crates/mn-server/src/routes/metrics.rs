//! `GET /metrics` — Prometheus exposition (FR-111).
//!
//! Reads `telemetry_aggregate_daily` and renders one counter row per
//! `(event_type, component)` pair, plus a same-shape "today only" counter
//! so an operator can graph short-window event rates without scraping the
//! whole rollup history.
//!
//! Anonymous; no bearer required. The endpoint emits Prometheus text
//! exposition v0.0.4 — strictly numeric counters and HELP/TYPE comments.

use std::fmt::Write as _;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use sqlx::{PgPool, Row as _};

use crate::app::AppState;

/// Mount the `/metrics` route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics))
}

async fn metrics(State(state): State<AppState>) -> Response {
    match render(&state.pool).await {
        Ok(body) => {
            let mut resp = body.into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                // Prometheus text exposition; version 0.0.4 is the de-facto
                // current spec at the time of writing.
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            resp
        }
        Err(e) => {
            tracing::warn!(error = %e, "render /metrics failed");
            (StatusCode::SERVICE_UNAVAILABLE, "metrics temporarily unavailable\n").into_response()
        }
    }
}

async fn render(pool: &PgPool) -> Result<String, sqlx::Error> {
    let mut out = String::new();
    render_total_counter(&mut out, pool).await?;
    render_today_counter(&mut out, pool).await?;
    Ok(out)
}

async fn render_total_counter(out: &mut String, pool: &PgPool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT event_type, component, SUM(count)::bigint AS total \
         FROM telemetry_aggregate_daily \
         GROUP BY event_type, component \
         ORDER BY event_type, component",
    )
    .fetch_all(pool)
    .await?;
    out.push_str(
        "# HELP midnight_manual_telemetry_events_total Lifetime telemetry event count per type and component.\n\
         # TYPE midnight_manual_telemetry_events_total counter\n",
    );
    for row in &rows {
        let event_type: String = row.get("event_type");
        let component: String = row.get("component");
        let total: i64 = row.get("total");
        writeln!(
            out,
            "midnight_manual_telemetry_events_total{{event_type=\"{}\",component=\"{}\"}} {}",
            escape(&event_type),
            escape(&component),
            total,
        )
        .expect("write! into String never fails");
    }
    Ok(())
}

async fn render_today_counter(out: &mut String, pool: &PgPool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT event_type, component, count::bigint AS today \
         FROM telemetry_aggregate_daily \
         WHERE day = CURRENT_DATE \
         ORDER BY event_type, component",
    )
    .fetch_all(pool)
    .await?;
    out.push_str(
        "# HELP midnight_manual_telemetry_events_today Today's telemetry event count per type and component.\n\
         # TYPE midnight_manual_telemetry_events_today gauge\n",
    );
    for row in &rows {
        let event_type: String = row.get("event_type");
        let component: String = row.get("component");
        let today: i64 = row.get("today");
        writeln!(
            out,
            "midnight_manual_telemetry_events_today{{event_type=\"{}\",component=\"{}\"}} {}",
            escape(&event_type),
            escape(&component),
            today,
        )
        .expect("write! into String never fails");
    }
    Ok(())
}

/// Prometheus label-value escape: `\` → `\\`, `"` → `\"`, newline → `\n`.
/// Every label value we emit is from our own closed allow-lists so this is
/// belt-and-braces, but the escape is cheap and the lint catch tomorrow is
/// expensive.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escape_handles_special_chars() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape(r"a\b"), r"a\\b");
        assert_eq!(escape("line1\nline2"), "line1\\nline2");
    }
}
