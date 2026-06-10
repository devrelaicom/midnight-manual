//! Opaque keyset-cursor encoding shared by the paginated list endpoints.
//! A cursor is the base64url (no pad) encoding of the last-seen sort key.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

/// Encode a sort-key value as an opaque cursor token.
#[must_use]
pub fn encode_cursor(last_key: &str) -> String {
    URL_SAFE_NO_PAD.encode(last_key)
}

/// Decode a cursor token back to the sort-key value. `None` on any malformed input.
#[must_use]
pub fn decode_cursor(cursor: &str) -> Option<String> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(decode_cursor(&encode_cursor("compact-docs")).as_deref(), Some("compact-docs"));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(decode_cursor("!!!not-base64!!!"), None);
    }

    #[test]
    fn rejects_invalid_utf8_payload() {
        // 0xFF is never valid UTF-8.
        let token = URL_SAFE_NO_PAD.encode([0xFF, 0xFE]);
        assert_eq!(decode_cursor(&token), None);
    }

    #[test]
    fn empty_key_round_trips() {
        assert_eq!(decode_cursor(&encode_cursor("")).as_deref(), Some(""));
    }
}
