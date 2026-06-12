//! Real BPE token counting matched to the production embedder.
//!
//! The ingest pipeline records a `token_count` for every chunk and document
//! (see `crate::ingest::plan::PlannedChunk` / `PlannedDocument`). That value
//! is surfaced to operators via `mnm ingest plan` as the upload-size estimate
//! and stored alongside chunk rows in the corpus database, so it needs to
//! match what the embedder actually consumes — not a coarse whitespace
//! approximation.
//!
//! We tokenize with a vendored `bge-base-en-v1.5` `WordPiece` vocabulary via
//! the `tokenizers` crate (a direct workspace dependency) so ingest planning can
//! encode text offline, without loading any model runtime.
//!
//! Design note: `docs/superpowers/specs/2026-05-27-bpe-token-counts-design.md`
//! Original requirement: `docs/superpowers/specs/2026-05-25-ingest-ux-design.md`
//! §3.4 (`token_count` row).

use std::sync::OnceLock;

use tokenizers::Tokenizer;

/// Vendored `bge-base-en-v1.5` tokenizer (sourced from
/// `Xenova/bge-base-en-v1.5/tokenizer.json`, 2026-05-27, sha256
/// `d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66`).
///
/// Embedded directly into the binary so ingest planning never hits the
/// network and never depends on the embedder being loaded.
const BGE_BASE_TOKENIZER_BYTES: &[u8] = include_bytes!("../assets/bge-base-en-v1.5/tokenizer.json");

/// Process-wide tokenizer instance. Parsed once on first call; subsequent
/// calls take a `Relaxed` load and reuse the same `Tokenizer`. `encode` is
/// `&self`-only, so there is no locking on the hot path.
static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();

/// Return the process-wide `bge-base-en-v1.5` tokenizer instance.
///
/// Initialised once via `OnceLock`; every subsequent call is a cheap load.
pub(crate) fn tokenizer() -> &'static Tokenizer {
    TOKENIZER.get_or_init(|| {
        // The bytes are compiled into the binary — a parse failure means the
        // build is broken, not a recoverable runtime condition.
        Tokenizer::from_bytes(BGE_BASE_TOKENIZER_BYTES)
            .expect("vendored bge-base-en-v1.5 tokenizer must parse")
    })
}

/// Count the BPE tokens in `text` using the `bge-base-en-v1.5` tokenizer.
///
/// The returned count includes the `[CLS]` and `[SEP]` special tokens that
/// the BERT post-processor inserts (so an empty string returns 2), because
/// that is the input length the embedder actually sees and pays for.
///
/// Saturates to `u32::MAX` if the token count would otherwise overflow `u32`
/// (a 4-billion-token chunk is well past anything realistic).
///
/// # Panics
///
/// Panics only if `tokenizers::Tokenizer::encode` itself panics — which it
/// does not for any non-malicious UTF-8 input in the `BertNormalizer` +
/// `WordPiece` configuration we use. The vendored `tokenizer.json` has
/// `truncation: null` and `padding: null`, so there is no internal failure
/// path that we map to an error.
#[must_use]
pub fn count(text: &str) -> u32 {
    let encoding = tokenizer()
        .encode(text, true)
        .expect("bge tokenizer encode is infallible for utf-8 input");
    u32::try_from(encoding.get_ids().len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_counts_only_special_tokens() {
        // BERT post-processor inserts [CLS] and [SEP] around every input,
        // so even an empty string costs 2 tokens at the embedder.
        assert_eq!(count(""), 2);
    }

    #[test]
    fn whitespace_only_counts_only_special_tokens() {
        // The BertNormalizer collapses whitespace, then the WordPiece
        // splitter has nothing to emit between the [CLS] / [SEP] frame.
        assert_eq!(count("   \n\t  "), 2);
    }

    #[test]
    fn ascii_words_match_wordpiece_pieces_plus_specials() {
        // "hello world how are you" — five common words, each is its own
        // single WordPiece in bert-base-uncased. Plus [CLS]/[SEP] = 7.
        assert_eq!(count("hello world how are you"), 7);
    }

    #[test]
    fn rare_words_split_into_subwords() {
        // "tokenization" is not a single piece in bert-base-uncased; it
        // splits into multiple WordPieces. Whichever the exact split is,
        // the count must exceed `1 word + 2 specials = 3` — that is the
        // key contract: BPE sees more pieces than `split_whitespace` does.
        let bpe = count("tokenization");
        assert!(
            bpe > 3,
            "rare word should split into subwords (got {bpe} pieces incl. specials)"
        );
    }

    #[test]
    fn code_identifiers_decompose_aggressively() {
        // Code-shaped text is where the whitespace approximation falls
        // down hardest: a single identifier like `HashMap<String, Vec<u8>>`
        // is one whitespace token but many BPE pieces. We assert only the
        // ordering property so the test does not pin a specific tokenizer
        // version, but the contract is "BPE is materially larger than
        // whitespace count" for code.
        let src = "let mut m: HashMap<String, Vec<u8>> = HashMap::new();";
        let whitespace_words = u32::try_from(src.split_whitespace().count()).unwrap();
        let bpe = count(src);
        assert!(
            bpe > whitespace_words * 2,
            "BPE should be at least 2x the whitespace count for code \
             (whitespace={whitespace_words}, bpe={bpe})"
        );
    }

    #[test]
    fn singleton_returns_same_tokenizer() {
        // Hot-path contract: the OnceLock initialisation runs exactly once
        // and subsequent callers reuse the same instance. We can't observe
        // the pointer directly through `&'static`, but two back-to-back
        // counts should hit the same instance without panicking.
        let a = count("the quick brown fox");
        let b = count("the quick brown fox");
        assert_eq!(a, b);
    }

    #[test]
    fn count_is_monotonic_in_prefix() {
        // Appending text never decreases the count (truncation is disabled
        // in the vendored config). Useful as a smoke test against
        // accidental clobbering of the [SEP] suffix on retokenization.
        let a = count("the quick");
        let b = count("the quick brown");
        let c = count("the quick brown fox jumps");
        assert!(a <= b);
        assert!(b <= c);
    }

    #[test]
    fn unicode_input_does_not_panic() {
        // BertNormalizer lower-cases and strips accents by default — the
        // count is whatever the model would see, but the contract is just
        // "no panic, count > 0".
        let s = "café Zürich 日本語 🦀";
        let n = count(s);
        assert!(n > 2, "non-empty input should produce >2 tokens (got {n})");
    }
}
