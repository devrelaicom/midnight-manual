//! Stable content hash for documents and chunks.
//!
//! FR-014 (`content_hash` powers carry-forward optimization) requires that
//! identical content produces identical hashes regardless of trivial
//! whitespace or line-ending differences. SHA-256 over the normalized form.

use sha2::{Digest, Sha256};

/// Compute a stable hex-encoded SHA-256 hash over `content` after normalizing:
/// - CRLF → LF
/// - trailing whitespace on each line removed
/// - trailing blank lines removed
#[must_use]
pub fn document_hash(content: &str) -> String {
    let normalized = normalize(content);
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    hex_lower(&digest)
}

/// Compute a stable hex-encoded SHA-256 hash over a chunk's verbatim content.
/// Chunks are hashed without normalization because chunk boundaries matter.
#[must_use]
pub fn chunk_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    hex_lower(&digest)
}

/// Compose the document-hash input (spec §Architecture).
///
/// The verbatim frontmatter fence + NUL + the PROCESSED body. NUL keeps the
/// two segments unambiguous (a fence line can never contain NUL — non-UTF8 is
/// skipped upstream).
#[must_use]
pub fn hash_input(raw_frontmatter: Option<&str>, body: &str) -> String {
    format!("{}\0{}", raw_frontmatter.unwrap_or(""), body)
}

fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let s = s.replace("\r\n", "\n");
    for line in s.split('\n') {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    // Drop trailing blank lines
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_hash_is_normalized() {
        let a = "Hello world\n";
        let b = "Hello world  \n"; // trailing whitespace
        let c = "Hello world\r\n"; // CRLF
        assert_eq!(document_hash(a), document_hash(b));
        assert_eq!(document_hash(a), document_hash(c));
    }

    #[test]
    fn different_content_different_hash() {
        assert_ne!(document_hash("a"), document_hash("b"));
    }

    #[test]
    fn chunk_hash_is_exact() {
        let a = "Hello\n";
        let b = "Hello \n"; // trailing space matters for chunks
        assert_ne!(chunk_hash(a), chunk_hash(b));
    }

    #[test]
    fn hex_output_is_64_chars() {
        let h = document_hash("anything");
        assert_eq!(h.len(), 64);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
