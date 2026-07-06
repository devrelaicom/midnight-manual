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

/// Maximum length of a single newline-delimited message body accepted by
/// [`FrameReader::next_message`]. The MCP server is a long-lived child of an
/// arbitrary AI client, so we cap the per-message line length to refuse a
/// runaway line. 16 MiB is well above any reasonable real-world payload (a
/// 50-vector query at 1024 dims is ~600 KiB). The cap is enforced
/// *incrementally* while reading (see [`FrameReader::next_message`]), so a buggy
/// client that streams a huge payload without a `\n` is refused before it can
/// drive an unbounded allocation — not merely rejected after the whole line is
/// buffered.
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
            // The cap is enforced *while* reading, so an unterminated runaway
            // line can never grow the buffer past ~`MAX_BODY_BYTES`.
            let Some(mut buf) = read_line_capped(&mut self.inner, MAX_BODY_BYTES).await? else {
                return Ok(None); // clean EOF
            };
            // Strip the trailing LF and an optional preceding CR (CRLF tolerance).
            if buf.last() == Some(&b'\n') {
                buf.pop();
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
            }
            // Authoritative boundary check on the stripped body. `read_line_capped`
            // reads up to `MAX_BODY_BYTES + 2` raw bytes (body + optional `\r\n`),
            // so a body of exactly `MAX_BODY_BYTES` — even with CRLF — is accepted
            // and anything larger is rejected here.
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

/// Read one newline-terminated line from `r`, enforcing `max` **incrementally**.
///
/// Returns the raw line *including* its trailing `\n` (and optional `\r`), or
/// the trailing partial line at EOF. `Ok(None)` is a clean EOF with nothing
/// buffered.
///
/// Unlike `read_until`, which buffers the whole line before any size check, this
/// caps the accumulator as it fills: it reads at most `max + 2` bytes (a full
/// `max`-byte body plus a `\r\n`) before erroring, so a client that streams
/// without a `\n` cannot force an unbounded allocation. The `+ 2` headroom lets
/// the caller apply the authoritative post-strip check so a body of exactly
/// `max` bytes is still accepted.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidData`] if the raw line would exceed `max + 2`
/// bytes, or any underlying read error.
async fn read_line_capped<R>(r: &mut BufReader<R>, max: usize) -> io::Result<Option<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let raw_cap = max.saturating_add(2);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let available = r.fill_buf().await?;
        if available.is_empty() {
            // EOF: yield the trailing partial line, or None if nothing pending.
            return Ok((!buf.is_empty()).then_some(buf));
        }
        // Take through the first `\n` (inclusive) if present, else the whole slice.
        let newline_at = available.iter().position(|&b| b == b'\n');
        let take = newline_at.map_or(available.len(), |i| i + 1);
        // Enforce the cap BEFORE copying so a runaway line is refused without
        // ever allocating past `raw_cap`.
        if buf.len() + take > raw_cap {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message exceeds MAX_BODY_BYTES ({max})"),
            ));
        }
        buf.extend_from_slice(&available[..take]);
        r.consume(take);
        if newline_at.is_some() {
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
    async fn exactly_max_body_bytes_is_accepted() {
        // The `raw_cap = MAX_BODY_BYTES + 2` headroom in `read_line_capped`
        // exists precisely so a body of *exactly* MAX_BODY_BYTES is accepted —
        // with either LF or CRLF framing — while MAX_BODY_BYTES + 1 is rejected
        // (see `oversize_line_is_rejected`). This regression-locks that off-by-one
        // boundary rather than resting on hand-derivation (#174).
        let body = vec![b'x'; MAX_BODY_BYTES];

        // LF framing: body + '\n'.
        let mut lf = body.clone();
        lf.push(b'\n');
        let msg = read_one(&lf)
            .await
            .expect("exactly-cap body (LF) must be accepted");
        assert_eq!(msg.len(), MAX_BODY_BYTES);
        assert_eq!(msg, body);

        // CRLF framing: body + '\r\n' — the stripped body is still MAX_BODY_BYTES.
        let mut crlf = body.clone();
        crlf.extend_from_slice(b"\r\n");
        let msg = read_one(&crlf)
            .await
            .expect("exactly-cap body (CRLF) must be accepted");
        assert_eq!(msg.len(), MAX_BODY_BYTES);
        assert_eq!(msg, body);
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

    /// A client that streams forever without ever emitting a `\n` must be
    /// refused by the cap *incrementally*, not after the whole line is buffered.
    /// This endless reader would drive an unbounded allocation (OOM/hang) under
    /// the old `read_until`-then-check code; with the bounded loop the read
    /// terminates with `InvalidData` after touching only a bounded prefix.
    #[tokio::test]
    async fn unterminated_stream_is_capped_incrementally() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        use tokio::io::ReadBuf;

        /// Serves `b'x'` in small chunks and never signals EOF or a newline,
        /// counting how many bytes it has handed out.
        struct EndlessNoNewline {
            served: usize,
        }
        impl tokio::io::AsyncRead for EndlessNoNewline {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                let n = buf.remaining().min(64);
                for _ in 0..n {
                    buf.put_slice(b"x");
                }
                self.served += n;
                Poll::Ready(Ok(()))
            }
        }

        // A tiny cap keeps the test cheap: the loop must bail almost immediately.
        const MAX: usize = 8;
        let mut reader = BufReader::new(EndlessNoNewline { served: 0 });
        let err = read_line_capped(&mut reader, MAX).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("MAX_BODY_BYTES"));
        // Proof it stopped early: it drained only a bounded prefix of the
        // otherwise-infinite stream rather than the whole thing.
        assert!(
            reader.get_ref().served <= 64 * 1024,
            "expected a bounded read, but the reader served {} bytes",
            reader.get_ref().served,
        );
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
