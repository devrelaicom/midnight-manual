//! `mnm chunks neighbors <chunk-id>` — convenience verb that composes
//! `prev` + `show` + `next` into one rendering.
//!
//! Deferred from the chunk + document navigation design (§8). The design
//! called it "trivial to script", which it is — this verb just shaves the
//! three calls down to one for operators reading a chunk in context.
//!
//! The default `--count` is **2** chunks each side (so the rendered window
//! is five chunks wide, centred on the anchor). That's deliberately
//! smaller than the `5` default on `chunks {next,prev}`: those verbs are
//! used to walk a document, whereas `neighbors` is used to *read* a chunk
//! and you usually only need a couple of lines of context.
//!
//! Output shape mirrors `chunks next` and `chunks show`:
//!
//! * Human mode emits three labelled sections (`prev:`, `chunk:`, `next:`)
//!   reusing the existing render helpers so formatting stays consistent
//!   with `mnm chunks {show, next, prev}` byte-for-byte.
//! * JSON mode emits a composite envelope:
//!   `{"prev": {"chunks": [..]}, "chunk": {..}, "next": {"chunks": [..]}}`.
//!   That keeps each sub-payload identical to what the corresponding
//!   individual verb would have emitted — easy to splice into scripts that
//!   already parse those responses.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

/// Arguments for `mnm chunks neighbors`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Anchor chunk UUID.
    pub chunk_id: Uuid,
    /// Number of chunks to fetch on each side of the anchor.
    ///
    /// The server clamps to `1..=100`. Default is `2` — small on purpose;
    /// see the module-level docs for the rationale.
    #[arg(long, default_value_t = 2)]
    pub count: u32,
    /// Show full content for prev/next chunks instead of a 240-char preview.
    ///
    /// Matches the `--full` flag on `chunks {next,prev}`. The anchor chunk
    /// (`chunks show` behaviour) always renders the full body; the flag
    /// only affects the surrounding context.
    #[arg(long)]
    pub full: bool,
}

/// Run the `chunks neighbors` subcommand.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;

    // Fire the three GETs sequentially. Parallelism would shave a round trip
    // but the navigation endpoints are cheap and operator workflows are
    // interactive, so the simpler control flow wins.
    let prev_body = fetch(&client, &server_url, &args.chunk_id, Some(("prev", args.count))).await?;
    let show_body = fetch(&client, &server_url, &args.chunk_id, None).await?;
    let next_body = fetch(&client, &server_url, &args.chunk_id, Some(("next", args.count))).await?;

    if json {
        emit_json(&prev_body, &show_body, &next_body)?;
    } else {
        emit_human(&prev_body, &show_body, &next_body, args.full)?;
    }
    Ok(())
}

/// Issue a single GET against the navigation surface and return the body.
///
/// `dir` is `Some(("prev", n))` / `Some(("next", n))` for the list endpoints,
/// or `None` for the single-chunk `show` endpoint.
async fn fetch(
    client: &reqwest::Client,
    server_url: &str,
    chunk_id: &Uuid,
    dir: Option<(&str, u32)>,
) -> Result<String> {
    let url = match dir {
        Some((d, count)) => format!("{server_url}/v1/chunks/{chunk_id}/{d}?count={count}"),
        None => format!("{server_url}/v1/chunks/{chunk_id}"),
    };
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    Ok(body)
}

/// Render the composite JSON envelope to stdout.
fn emit_json(prev_body: &str, show_body: &str, next_body: &str) -> Result<()> {
    let prev: serde_json::Value =
        serde_json::from_str(prev_body).context("parse prev response body")?;
    let show: serde_json::Value =
        serde_json::from_str(show_body).context("parse show response body")?;
    let next: serde_json::Value =
        serde_json::from_str(next_body).context("parse next response body")?;
    let envelope = serde_json::json!({
        "prev": prev,
        "chunk": show,
        "next": next,
    });
    println!("{envelope}");
    Ok(())
}

/// Render the three labelled sections to stdout in human mode.
///
/// Reuses [`super::next::render_chunks`] and [`super::show::render_chunk`]
/// so the per-chunk formatting is byte-identical to the standalone verbs.
fn emit_human(prev_body: &str, show_body: &str, next_body: &str, full: bool) -> Result<()> {
    println!("prev:");
    super::next::render_chunks(prev_body, full)?;

    println!("chunk:");
    super::show::render_chunk(show_body)?;
    println!();

    println!("next:");
    super::next::render_chunks(next_body, full)?;
    Ok(())
}
