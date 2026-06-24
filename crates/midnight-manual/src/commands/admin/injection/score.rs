//! `mnm admin injection score <content>` — ad-hoc injection scoring.
//!
//! `POST /v1/admin/injection/score` runs the requested detector leg(s) over
//! arbitrary content and returns a [`mnm_core::injection::ScanReport`]. The
//! human path prints a readable breakdown of both legs, the blended score, and
//! the final verdict; `--json` emits the report verbatim.

use anyhow::{Context as _, Result};

use super::ScoreArgs;

/// Run `mnm admin injection score`.
///
/// # Errors
///
/// Returns an error on transport failure, a non-2xx response, a response body
/// that does not parse into a [`mnm_core::injection::ScanReport`], or when no
/// admin bearer can be resolved from `auth.toml`.
pub async fn run(args: ScoreArgs, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let token = crate::commands::ratelimits::require_admin_token_from(&mnm_core::config::StdEnv)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build HTTP client")?;

    let resp = client
        .post(format!("{server_url}/v1/admin/injection/score"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": args.content,
            "detector": args.detector,
        }))
        .send()
        .await
        .context("POST /v1/admin/injection/score")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(error_from_body(status, &body));
    }

    let report: mnm_core::injection::ScanReport =
        serde_json::from_str(&body).context("parse injection score response")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    Ok(())
}

/// Print a human-readable breakdown of a [`mnm_core::injection::ScanReport`].
fn print_report(report: &mnm_core::injection::ScanReport) {
    println!("Detectors run: {}", report.detectors_run.join(", "));

    println!("Pattern score: {:.2}", report.pattern.score);
    if report.pattern.matches.is_empty() {
        println!("  (no pattern matches)");
    } else {
        for m in &report.pattern.matches {
            let [start, end] = m.span;
            println!("  - {:?}  \"{}\"  [{start}..{end}]", m.technique, m.matched);
        }
    }

    match &report.model {
        Some(m) => {
            println!("Model: available={} score={:.2}", m.available, m.score);
            println!("  flagged windows: {}", m.flagged_windows.len());
        }
        None => println!("Model: not run"),
    }

    println!(
        "Blended score: {:.2}  (reject threshold {:.2})",
        report.blended_score, report.reject_threshold
    );
    println!("Verdict: {:?}", report.verdict);
}

/// Turn a non-2xx `(status, body)` into a clean `anyhow` error, preferring the
/// server's typed error envelope and falling back to the raw body.
fn error_from_body(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    crate::shared::decode_error_envelope(body).map_or_else(
        || anyhow::anyhow!("{status} from injection score: {body}"),
        |e| {
            anyhow::anyhow!(
                "{status} from injection score: {}: {} ({})",
                e.code,
                e.message,
                e.remediation
            )
        },
    )
}
