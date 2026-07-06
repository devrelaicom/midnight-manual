//! `mnm facets` — print the corpus's filterable facets and corpus-derived
//! values (`GET /v1/facets`), so users can construct valid `mnm search`
//! filters.
//!
//! Read-only and anonymous (the endpoint is public, like `/v1/search`). The
//! body shape is `{ "modes": [..], "filters": [{ key, type, negatable,
//! values?, truncated?, total? }] }`; see `midnight-manual-server`'s `routes::facets`.

use anyhow::{anyhow, Context as _, Result};

/// `mnm facets` arguments. (No flags of its own; `--json` is the global flag.)
#[derive(Debug, clap::Args)]
pub struct Args {}

/// Fetch and print the corpus's filterable facets.
///
/// # Errors
///
/// Returns an error on network failure, a non-2xx response, or a body that
/// does not parse as JSON.
pub async fn run(
    _args: Args,
    server: Option<&str>,
    config: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server, config);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let value = get_facets(&client, &format!("{server_url}/v1/facets")).await?;
    emit_facets(&value, json);
    Ok(())
}

async fn get_facets(client: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("{status} from facets: {body}"));
    }
    serde_json::from_str(&body).context("parse facets response body")
}

fn emit_facets(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    println!("modes: {}", join_strs(&v["modes"]));
    let Some(filters) = v["filters"].as_array() else {
        println!("(unexpected response shape)");
        return;
    };
    for f in filters {
        print_facet(f);
    }
}

fn print_facet(f: &serde_json::Value) {
    let key = f["key"].as_str().unwrap_or("?");
    let ty = f["type"].as_str().unwrap_or("?");
    let neg = if f["negatable"].as_bool().unwrap_or(false) {
        " (negatable)"
    } else {
        ""
    };
    let vals = format_values(f);
    println!("  {key:<16} [{ty}]{neg}{vals}");
}

/// Render a facet's `values` suffix, appending a truncation hint when the
/// corpus enumeration was capped (`truncated` / `total` are set by the server
/// for high-cardinality open-set facets).
fn format_values(f: &serde_json::Value) -> String {
    let Some(values) = f.get("values") else {
        return String::new();
    };
    let joined = join_strs(values);
    if f["truncated"].as_bool().unwrap_or(false) {
        let total = f["total"].as_i64().unwrap_or_default();
        format!(" — {joined} (+{total}+ total)")
    } else {
        format!(" — {joined}")
    }
}

/// Join a JSON array of strings into a comma-separated list, falling back to
/// the value's compact JSON form for non-string-array shapes.
fn join_strs(v: &serde_json::Value) -> String {
    v.as_array().map_or_else(
        || v.to_string(),
        |arr| {
            arr.iter()
                .map(|x| x.as_str().map_or_else(|| x.to_string(), ToOwned::to_owned))
                .collect::<Vec<_>>()
                .join(", ")
        },
    )
}
