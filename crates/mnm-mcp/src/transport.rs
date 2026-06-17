//! Stdio JSON-RPC framing for the MCP server (FR-036).
//!
//! The Model Context Protocol's stdio transport is **newline-delimited
//! JSON-RPC**: each message is a single JSON object serialized on one line and
//! terminated by `\n`. There are NO headers (no `Content-Length`), and the MCP
//! spec mandates that a message MUST NOT contain embedded newlines — the
//! framing relies on `\n` being a message boundary, never a byte inside a
//! message. (MCP spec, "Transports → stdio": messages are delimited by
//! newlines and individual messages MUST NOT contain embedded newlines.)
//!
//! ```text
//! {"jsonrpc":"2.0","id":1,"method":"initialize",...}\n
//! {"jsonrpc":"2.0","id":2,"method":"tools/list",...}\n
//! ```
//!
//! For tolerance we accept `\r\n` line endings on the way in and skip blank
//! lines. On the way out we serialize compact JSON (serde_json's default, no
//! embedded newlines) and append a single `\n`.

use std::io;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

/// Maximum length of a single newline-delimited line accepted by
/// [`FrameReader::next_message`]. The MCP server is a long-lived child of an
/// arbitrary AI client, so we cap the per-message line length to refuse a
/// runaway line. 16 MiB is well above any reasonable real-world payload (a
/// 50-vector query at 1024 dims is ~600 KiB).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Tokio-friendly reader that yields one JSON message at a time over a
/// newline-delimited stream.
pub struct FrameReader<R> {
    inner: BufReader<R>,
}

impl<R> FrameReader<R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    /// Build a reader wrapping `r`. The wrapper buffers — pass a raw `Stdin`,
    /// not an already-buffered reader.
    pub fn new(r: R) -> Self {
        Self { inner: BufReader::new(r) }
    }

    /// Read one newline-delimited message, returning the JSON body as bytes
    /// (the trailing `\n`, and an optional preceding `\r`, are stripped).
    ///
    /// Returns `Ok(None)` on clean EOF (no further messages). Blank lines
    /// (stray newlines / keepalives) are skipped.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] on read failure, or [`io::ErrorKind::InvalidData`]
    /// if a single line exceeds [`MAX_BODY_BYTES`].
    pub async fn next_message(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            let mut buf: Vec<u8> = Vec::new();
            // read_until includes the trailing '\n' (if present). Bounded after the
            // read by MAX_BODY_BYTES — for a trusted local child-process transport
            // this post-read cap is sufficient (a hostile parent already controls us).
            let n = self.inner.read_until(b'\n', &mut buf).await?;
            if n == 0 {
                return Ok(None); // clean EOF
            }
            // Strip the trailing LF and an optional preceding CR (CRLF tolerance).
            if buf.last() == Some(&b'\n') {
                buf.pop();
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
            }
            if buf.len() > MAX_BODY_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("message exceeds MAX_BODY_BYTES ({MAX_BODY_BYTES})"),
                ));
            }
            if buf.is_empty() {
                continue; // skip blank lines (stray newlines / keepalives)
            }
            return Ok(Some(buf));
        }
    }
}

/// Async writer that frames JSON messages on the way out.
pub struct FrameWriter<W> {
    inner: W,
}

impl<W> FrameWriter<W>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    /// Build a writer wrapping `w`.
    pub const fn new(w: W) -> Self {
        Self { inner: w }
    }

    /// Write one newline-delimited JSON message.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] on write failure.
    pub async fn write_message(&mut self, body: &[u8]) -> io::Result<()> {
        // MCP stdio messages are newline-delimited and MUST NOT contain embedded
        // newlines. The server serializes compact JSON (serde_json default), so a
        // single trailing '\n' frames each message.
        self.inner.write_all(body).await?;
        self.inner.write_all(b"\n").await?;
        self.inner.flush().await?;
        Ok(())
    }
}

/// Synchronous helper for unit tests: frame a JSON body into newline-delimited
/// bytes (`body + b"\n"`).
#[must_use]
pub fn frame_blocking(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 1);
    out.extend_from_slice(body);
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_one(input: &[u8]) -> Option<Vec<u8>> {
        let mut reader = FrameReader::new(input);
        reader.next_message().await.expect("read")
    }

    #[tokio::test]
    async fn round_trip_through_pipe() {
        let body = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}";
        let framed = frame_blocking(body);
        // The wire form is exactly `body + "\n"` — newline-delimited, no headers.
        assert_eq!(framed, [&body[..], b"\n"].concat());
        let msg = read_one(&framed).await.expect("message");
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn multiple_messages_in_stream() {
        let body1 = b"{\"a\":1}";
        let body2 = b"{\"b\":2}";
        let mut framed = frame_blocking(body1);
        framed.extend_from_slice(&frame_blocking(body2));
        let mut reader = FrameReader::new(&framed[..]);
        assert_eq!(reader.next_message().await.unwrap().as_deref(), Some(&body1[..]));
        assert_eq!(reader.next_message().await.unwrap().as_deref(), Some(&body2[..]));
        assert!(reader.next_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn eof_returns_none() {
        let mut reader = FrameReader::new(&b""[..]);
        assert!(reader.next_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn crlf_is_tolerated() {
        // A client that emits CRLF line endings (e.g. on Windows) must still be
        // read back as the bare body, with no trailing '\r'.
        let body = b"{\"x\":1}";
        let mut framed = body.to_vec();
        framed.extend_from_slice(b"\r\n");
        let msg = read_one(&framed).await.expect("message");
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn blank_lines_are_skipped() {
        // Stray leading newlines (keepalives, blank lines) are ignored; the
        // first real body is yielded, then EOF.
        let body = b"{\"x\":1}";
        let mut framed = b"\n\n".to_vec();
        framed.extend_from_slice(body);
        framed.push(b'\n');
        let mut reader = FrameReader::new(&framed[..]);
        assert_eq!(reader.next_message().await.unwrap().as_deref(), Some(&body[..]));
        assert!(reader.next_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversize_line_is_rejected() {
        // A single line longer than MAX_BODY_BYTES must error with InvalidData,
        // not be returned as a message.
        let mut framed = vec![b'x'; MAX_BODY_BYTES + 1];
        framed.push(b'\n');
        let mut reader = FrameReader::new(&framed[..]);
        let err = reader.next_message().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("MAX_BODY_BYTES"));
    }

    #[tokio::test]
    async fn writer_emits_newline_delimited() {
        // Anti-regression guard: the writer MUST frame with a trailing '\n'
        // and MUST NOT emit an LSP-style `Content-Length` header. This pins the
        // wire format so the old framing can never silently return.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = FrameWriter::new(&mut buf);
            w.write_message(b"{\"x\":1}").await.unwrap();
        }
        assert_eq!(buf, b"{\"x\":1}\n");
        assert!(
            !buf.windows(b"Content-Length".len())
                .any(|w| w == b"Content-Length"),
            "writer output must not contain a Content-Length header"
        );
    }
}
