//! Byte-oriented stdio relay — newline-framed JSON-RPC pass-through.
//!
//! [`run_relay`] reads bytes from any [`AsyncRead`], splits on `\n`,
//! drops blank lines, enforces a 16 MiB per-frame cap, and writes the
//! surviving frames verbatim to any [`AsyncWrite`].  CRLF line endings are
//! preserved.  At EOF, [`handle_eof_tail`] applies the same logic as
//! `rmcp::JsonRpcMessageCodec::decode_eof`: trailing bytes that parse as
//! JSON are forwarded with a synthetic `\n`; malformed bytes are dropped
//! with a `tracing::warn!`.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::TransportError;

/// Maximum bytes buffered for a single newline-delimited frame.
pub const FRAME_CAP_BYTES: usize = 16 * 1024 * 1024;

/// Relay newline-delimited frames from `input` to `output`.
///
/// # Errors
///
/// Returns [`TransportError::FrameTooLarge`] when a line exceeds
/// [`FRAME_CAP_BYTES`] without a newline, or [`TransportError::Io`] on any
/// underlying I/O failure.
pub async fn run_relay<R, W>(mut input: R, mut output: W) -> Result<(), TransportError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut tmp = [0u8; 8192];
    loop {
        let n = input.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        while let Some(nl_at) = memchr::memchr(b'\n', &buf) {
            let frame = &buf[..=nl_at];
            if frame_is_blank(frame) {
                // skip — do not forward blank lines
            } else {
                output.write_all(frame).await?;
            }
            buf.drain(..=nl_at);
        }
        if buf.len() > FRAME_CAP_BYTES {
            tracing::error!(
                buffered = buf.len(),
                cap = FRAME_CAP_BYTES,
                "stdio relay frame exceeded 16 MiB cap"
            );
            return Err(TransportError::FrameTooLarge);
        }
    }
    // EOF tail handling — forward valid JSON, drop malformed bytes.
    handle_eof_tail(&buf, &mut output).await?;
    Ok(())
}

/// Returns `true` when `bytes` contains only ASCII whitespace (before the
/// trailing `\r\n` / `\n`).
fn frame_is_blank(bytes: &[u8]) -> bool {
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    bytes[..end].iter().all(|b| matches!(*b, b' ' | b'\t'))
}

/// Handle bytes remaining in the buffer after EOF with no trailing newline.
///
/// Mirrors `rmcp::JsonRpcMessageCodec::decode_eof`: if the bytes parse as
/// valid JSON they are forwarded followed by a synthetic `\n`; otherwise
/// they are silently dropped after a `tracing::warn!`.
async fn handle_eof_tail<W: AsyncWrite + Unpin>(tail: &[u8], out: &mut W) -> std::io::Result<()> {
    if tail.is_empty() {
        return Ok(());
    }
    let trimmed = trim_ascii_ws(tail);
    if trimmed.is_empty() {
        return Ok(());
    }
    match serde_json::from_slice::<serde_json::Value>(trimmed) {
        Ok(_) => {
            out.write_all(tail).await?;
            out.write_all(b"\n").await?;
        }
        Err(e) => {
            tracing::warn!(
                bytes = tail.len(),
                error = %e,
                "stdio relay: dropping malformed EOF tail"
            );
        }
    }
    Ok(())
}

/// Trim leading and trailing ASCII whitespace from a byte slice.
fn trim_ascii_ws(b: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = b.len();
    while start < end && b[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && b[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &b[start..end]
}
