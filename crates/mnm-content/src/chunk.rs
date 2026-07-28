//! Shared chunker contract: one trait, one config, one output shape, used by
//! the markdown chunker, the code chunkers, and the line-window fallback.

use mnm_core::types::{DocumentKind, SymbolSegment};

/// One chunk emitted by any [`Chunker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Verbatim chunk content.
    pub content: String,
    /// Ancestor heading path (markdown). Empty for code/plaintext.
    pub heading_path: Vec<String>,
    /// Structured symbol path (code). Empty for markdown/plaintext.
    pub symbol_path: Vec<SymbolSegment>,
    /// Byte offset of the chunk's first character in the source.
    pub start_byte: usize,
    /// Byte offset just past the chunk's last character.
    pub end_byte: usize,
    /// Token count of this chunk (BPE, via [`crate::tokens::count`]).
    pub token_count: u32,
    /// 0-indexed position among the document's chunks.
    pub chunk_index: u32,
    /// True iff produced by the line-window fallback.
    pub fallback_used: bool,
}

/// Canonical default per-file size ceiling (10 MiB).
///
/// Single source of truth for the value shared by [`ChunkerConfig::default`],
/// the walker ([`crate::ingest::walker::Walker`]), and the CLI
/// `--max-file-size` default.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Canonical default per-file longest-line ceiling (10,000 bytes).
///
/// A single line longer than this marks machine-generated data — chain-specs,
/// minified bundles, serialized blobs — rather than authored prose or code.
/// Such files both pollute search (a wall of hex is not a searchable answer)
/// and can exceed the embedding token limit as one un-splittable chunk (a
/// multi-million-char line has no natural break to chunk on). Investigation of
/// a 7,737-file real ingest set found this threshold flags data/config/fixture
/// files with no false positives (the legit content ceiling sat well below).
///
/// Single source of truth for the value shared by [`ChunkerConfig::default`],
/// the walker ([`crate::ingest::walker::Walker`]), and the CLI
/// `--max-line-bytes` default. `0` disables the check (unbounded line length).
pub const DEFAULT_MAX_LINE_BYTES: usize = 10_000;

/// Configuration shared by all chunkers. Token-budgeted.
#[derive(Debug, Clone, Copy)]
pub struct ChunkerConfig {
    /// Max chunk size in BPE tokens before splitting. Greedy coalescing in
    /// every chunker packs units up to [`coalesce_target`] (90% of this).
    pub max_tokens: u32,
    /// Per-file size ceiling. Enforced by the *walker* (the caller), which
    /// skips files larger than this before they ever reach a chunker (EC-52) —
    /// see [`crate::ingest::walker::Walker::with_max_file_bytes`]. Chunkers
    /// themselves never read this field.
    pub max_file_bytes: u64,
    /// Per-file longest-line ceiling (bytes). Enforced by the *walker* (the
    /// caller), which skips files containing a line longer than this before
    /// they ever reach a chunker — see
    /// [`crate::ingest::walker::Walker::with_max_line_bytes`]. Chunkers
    /// themselves never read this field. `0` disables the check.
    pub max_line_bytes: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_tokens: 1024,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
        }
    }
}

/// Greedy coalescing target: 90% of `max_tokens` (D2). Both the markdown and
/// code coalescers pack sibling units up to this; only a single unit larger
/// than `max_tokens` is ever split.
#[must_use]
pub const fn coalesce_target(cfg: &ChunkerConfig) -> u32 {
    cfg.max_tokens.saturating_mul(9) / 10
}

/// Errors a chunker can surface for one file. Never panics; the planner maps
/// these to a per-file warning (default) or a run failure (`--strict`).
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    /// The parser failed badly enough that we fell back to line-window.
    /// Carries the reason for the warning message.
    #[error("parser fell back to line-window: {0}")]
    ParserFallback(String),
}

/// The chunking contract. Implementations: markdown, each code language,
/// and the line-window fallback.
pub trait Chunker {
    /// Chunk `body`. Returns at least one chunk for non-empty, non-whitespace
    /// input; an empty vec for empty/whitespace input.
    ///
    /// # Errors
    /// Returns [`ChunkError`] only when the caller asked for strict behavior;
    /// the default implementations recover internally and return `Ok`.
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError>;
}

/// Chunk one document body with the chunker that matches its [`DocumentKind`].
///
/// Single source of truth for the kind → chunker dispatch, shared by the ingest
/// [`PlanBuilder`](crate::ingest::PlanBuilder) (new-document path) and the CLI
/// carried-document path (which re-chunks carry-forward docs only to recover
/// their document-level token total — no embedding is performed on them). Both
/// callers MUST route through this function so the dispatch can never drift
/// between them.
///
/// `ext` is the lowercased file extension (no leading dot); it selects the
/// per-language code chunker and is ignored for markdown/plaintext. A parser
/// failure inside any chunker is recovered internally (line-window fallback),
/// so this never errors and an empty/whitespace body yields no chunks.
///
/// This also upholds the "never panics" half of the contract: a chunker (or a
/// dependency it calls) that `panic!`s — rather than returning `Err` — is
/// caught by a [`std::panic::catch_unwind`] boundary and degraded to the
/// line-window fallback (issue #121). This convenience wrapper logs a
/// *file-less* warning on a caught panic; callers that hold the source path
/// (the ingest planner and the CLI carried/re-embed paths) use
/// [`chunk_document_guarded`] instead so the warning names the file — and so the
/// planner can honour `--strict`.
#[must_use]
pub fn chunk_document(
    kind: DocumentKind,
    ext: &str,
    body: &str,
    cfg: &ChunkerConfig,
) -> Vec<Chunk> {
    let (chunks, panicked) = chunk_document_guarded(kind, ext, body, cfg);
    if let Some(reason) = panicked {
        tracing::warn!(reason = %reason, "chunker panicked; fell back to line-window");
    }
    chunks
}

/// [`chunk_document`] with the caught-panic reason surfaced to the caller.
///
/// Returns the chunks plus `Some(reason)` when a chunker panic was caught (and
/// the body was re-chunked with the line-window fallback), or `None` on the
/// normal path. The ingest planner uses this so it can emit a per-file warning
/// by default or fail the run under `--strict`; everyone else can use the
/// simpler [`chunk_document`].
///
/// The caught panic's default hook still prints the upstream "thread '…'
/// panicked at …" line to stderr before we recover, which is intentionally left
/// intact so the offending dependency/site can be pinned and reported upstream.
#[must_use]
pub fn chunk_document_guarded(
    kind: DocumentKind,
    ext: &str,
    body: &str,
    cfg: &ChunkerConfig,
) -> (Vec<Chunk>, Option<String>) {
    guard(body, cfg, || dispatch_chunk(kind, ext, body, cfg))
}

/// Kind → chunker dispatch, no panic boundary. Callers route through
/// [`chunk_document`] / [`chunk_document_guarded`], never this directly, so the
/// boundary can never be bypassed.
fn dispatch_chunk(kind: DocumentKind, ext: &str, body: &str, cfg: &ChunkerConfig) -> Vec<Chunk> {
    // Test-only seam: a deterministic way to exercise the panic boundary and the
    // planner's degrade/strict branches without depending on a specific
    // dependency bug (which would stop reproducing the day the dependency is
    // fixed). Never compiled into shipped binaries.
    #[cfg(test)]
    assert!(body != PANIC_SENTINEL, "dispatch_chunk test sentinel panic");

    match kind {
        DocumentKind::Markdown => crate::markdown::MarkdownChunker
            .chunk(body, cfg)
            .unwrap_or_default(),
        DocumentKind::Code => {
            let lang = crate::code::language::Language::for_extension(ext);
            crate::code::chunker_for_ext(lang, ext)
                .chunk(body, cfg)
                .unwrap_or_default()
        }
        DocumentKind::Plaintext => crate::code::line_window::LineWindowChunker
            .chunk(body, cfg)
            .unwrap_or_default(),
    }
}

/// Body that makes [`dispatch_chunk`] panic, for tests of the panic boundary.
#[cfg(test)]
pub(crate) const PANIC_SENTINEL: &str = "__mnm_chunk_panic_sentinel__";

/// Run `chunk` inside a panic boundary. On a caught unwind, re-chunk `body` with
/// the line-window fallback and return the panic's message so the caller can
/// warn or fail. The line-window chunker is pure string slicing and does not
/// itself panic, so the fallback is safe to run outside the boundary.
fn guard(
    body: &str,
    cfg: &ChunkerConfig,
    chunk: impl FnOnce() -> Vec<Chunk>,
) -> (Vec<Chunk>, Option<String>) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(chunk)) {
        Ok(chunks) => (chunks, None),
        Err(payload) => {
            let reason = panic_reason(payload.as_ref());
            let fallback = crate::code::line_window::LineWindowChunker
                .chunk(body, cfg)
                .unwrap_or_default();
            (fallback, Some(reason))
        }
    }
}

/// Best-effort human-readable message from a caught panic payload.
pub(crate) fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_token_budgeted() {
        let c = ChunkerConfig::default();
        assert_eq!(c.max_tokens, 1024);
        assert_eq!(c.max_file_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn coalesce_target_is_90_pct_of_max_tokens() {
        // 921 == 1024 * 9 / 10 (integer division).
        assert_eq!(coalesce_target(&ChunkerConfig::default()), 921);
    }

    #[test]
    fn guarded_chunk_reports_no_panic_on_the_normal_path() {
        let (chunks, panicked) = chunk_document_guarded(
            DocumentKind::Markdown,
            "md",
            "# Title\n\nSome body text.",
            &ChunkerConfig::default(),
        );
        assert!(panicked.is_none(), "no panic expected on well-formed input");
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }

    #[test]
    fn guard_catches_a_panic_and_degrades_to_line_window() {
        let cfg = ChunkerConfig::default();
        let body = "line one\nline two\nline three";
        // A chunker that panics instead of returning Err (mirrors a dependency
        // like pulldown-cmark blowing up on adversarial input — issue #121).
        let (chunks, panicked) = guard(body, &cfg, || panic!("boom from a chunker"));

        let reason = panicked.expect("the panic must be caught and reported, not propagated");
        assert!(
            reason.contains("boom from a chunker"),
            "reason should carry the panic message: {reason}",
        );
        // Recovered: non-empty body still yields chunks, all flagged as fallback.
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.fallback_used));
    }

    #[test]
    fn guard_passes_through_chunks_when_no_panic() {
        let cfg = ChunkerConfig::default();
        let (chunks, panicked) = guard("anything", &cfg, Vec::new);
        assert!(panicked.is_none());
        assert!(chunks.is_empty());
    }

    #[test]
    fn panic_reason_handles_string_and_non_string_payloads() {
        let cfg = ChunkerConfig::default();
        // A formatted `panic!` produces a `String` payload (not `&'static str`).
        let (_, panicked) = guard("body", &cfg, || panic!("{}", String::from("owned message")));
        assert_eq!(panicked.as_deref(), Some("owned message"));
        // A non-string payload falls back to a fixed label.
        let (_, panicked) = guard("body", &cfg, || std::panic::panic_any(42u8));
        assert_eq!(panicked.as_deref(), Some("non-string panic payload"));
    }

    #[test]
    fn chunk_document_recovers_from_the_panic_sentinel() {
        // The public, path-less entry point also survives a chunker panic.
        let chunks =
            chunk_document(DocumentKind::Markdown, "md", PANIC_SENTINEL, &ChunkerConfig::default());
        // PANIC_SENTINEL is non-empty, so the line-window fallback emits chunks.
        assert!(chunks.iter().all(|c| c.fallback_used));
    }
}
