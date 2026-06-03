//! Stdio JSON-RPC framing for the MCP server (FR-036).
//!
//! The Model Context Protocol uses LSP-style `Content-Length`-framed JSON over
//! stdio. Each message is:
//!
//! ```text
//! Content-Length: <bytes>\r\n
//! \r\n
//! <UTF-8 JSON body, exactly `bytes` long>
//! ```
//!
//! Other headers (e.g. `Content-Type`) are tolerated and ignored.

use std::io::{self, Write as _};

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

/// Maximum body size accepted by [`FrameReader::next_message`]. The MCP server
/// is a long-lived child of an arbitrary AI client, so we refuse oversize
/// `Content-Length` declarations before allocating. 16 MiB is well above any
/// reasonable real-world payload (a 50-vector query at 1024 dims is ~600 KiB).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Tokio-friendly reader that yields one JSON message at a time over a
/// `Content-Length`-framed stream.
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

    /// Read one framed message, returning the JSON body as bytes.
    ///
    /// Returns `Ok(None)` on clean EOF (no further messages).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] on read failure, malformed headers, or
    /// missing `Content-Length`.
    pub async fn next_message(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut content_length: Option<usize> = None;
        let mut header_line = String::new();

        loop {
            header_line.clear();
            let n = self.inner.read_line(&mut header_line).await?;
            if n == 0 {
                // EOF before any header → clean shutdown if we haven't started
                // a message yet, otherwise a protocol error.
                return if content_length.is_none() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "EOF after Content-Length header but before body",
                    ))
                };
            }
            let trimmed = header_line.trim_end_matches(|c| c == '\r' || c == '\n');
            if trimmed.is_empty() {
                // End of header block.
                break;
            }
            // Parse `Header: value`. Case-insensitive header name.
            let Some((name, value)) = trimmed.split_once(':') else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("malformed header line: {trimmed:?}"),
                ));
            };
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid Content-Length: {value:?}"),
                    )
                })?);
            }
            // Other headers ignored.
        }

        let Some(len) = content_length else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no Content-Length header before body",
            ));
        };

        if len > MAX_BODY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Content-Length {len} exceeds MAX_BODY_BYTES ({MAX_BODY_BYTES})"),
            ));
        }

        let mut body = vec![0u8; len];
        self.inner.read_exact(&mut body).await?;
        Ok(Some(body))
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

    /// Write one framed JSON message.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] on write failure.
    pub async fn write_message(&mut self, body: &[u8]) -> io::Result<()> {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.inner.write_all(header.as_bytes()).await?;
        self.inner.write_all(body).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

/// Synchronous helper for unit tests: frame a JSON body into bytes.
#[must_use]
pub fn frame_blocking(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 32);
    let _ = write!(&mut out, "Content-Length: {}\r\n\r\n", body.len());
    out.extend_from_slice(body);
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
        let msg = read_one(&framed).await.expect("message");
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn case_insensitive_content_length_header() {
        let body = b"{}";
        let framed =
            format!("content-LENGTH: {}\r\n\r\n{}", body.len(), std::str::from_utf8(body).unwrap());
        let msg = read_one(framed.as_bytes()).await.unwrap();
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn ignores_extra_headers() {
        let body = b"{}";
        let framed = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let msg = read_one(framed.as_bytes()).await.unwrap();
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn eof_returns_none() {
        let mut reader = FrameReader::new(&b""[..]);
        assert!(reader.next_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_content_length_errors() {
        let mut reader = FrameReader::new(&b"X-Foo: bar\r\n\r\nbody"[..]);
        let err = reader.next_message().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn oversize_content_length_is_rejected() {
        // 17 MiB declared body, well over MAX_BODY_BYTES — must error before
        // allocating, not after running OOM.
        let framed = b"Content-Length: 17825793\r\n\r\n".to_vec();
        let mut reader = FrameReader::new(&framed[..]);
        let err = reader.next_message().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("MAX_BODY_BYTES"));
    }

    #[tokio::test]
    async fn malformed_header_errors() {
        let mut reader = FrameReader::new(&b"no_colon_here\r\n\r\n"[..]);
        let err = reader.next_message().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
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
    async fn writer_emits_framed_bytes() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = FrameWriter::new(&mut buf);
            w.write_message(b"{\"x\":1}").await.unwrap();
        }
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.starts_with("Content-Length: 7\r\n\r\n"));
        assert!(s.ends_with("{\"x\":1}"));
    }
}
