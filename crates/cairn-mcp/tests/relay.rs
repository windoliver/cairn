//! Integration tests for the byte-oriented stdio relay (§5).

use cairn_mcp::error::TransportError;
use cairn_mcp::relay::run_relay;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ── Task 16: blank-line drop + CRLF preservation ──────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn relay_drops_blank_lines_and_preserves_crlf() {
    let input: &[u8] = b"\n\r\n{\"a\":1}\r\n\n{\"b\":2}\n";
    let (mut tx, rx) = tokio::io::duplex(8192);
    let (out_tx, mut out_rx) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        run_relay(rx, out_tx).await.ok();
    });
    tx.write_all(input).await.unwrap();
    drop(tx);
    let mut got = Vec::new();
    out_rx.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"{\"a\":1}\r\n{\"b\":2}\n");
}

// ── Task 17: 16 MiB cap ───────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn relay_rejects_oversized_frame() {
    let input = vec![b'x'; 17 * 1024 * 1024]; // > 16 MiB, no newline
    let (mut tx, rx) = tokio::io::duplex(64 * 1024);
    let (out_tx, _out_rx) = tokio::io::duplex(64 * 1024);
    let h = tokio::spawn(async move { run_relay(rx, out_tx).await });
    tx.write_all(&input).await.ok();
    drop(tx);
    let res = h.await.unwrap();
    assert!(matches!(res, Err(TransportError::FrameTooLarge)));
}

#[tokio::test(flavor = "current_thread")]
async fn relay_passes_8mib_frame_intact() {
    let mut frame = vec![b'x'; 8 * 1024 * 1024];
    frame.push(b'\n');
    let (mut tx, rx) = tokio::io::duplex(64 * 1024);
    let (out_tx, mut out_rx) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        run_relay(rx, out_tx).await.ok();
    });
    tx.write_all(&frame).await.unwrap();
    drop(tx);
    let mut got = Vec::new();
    out_rx.read_to_end(&mut got).await.unwrap();
    assert_eq!(got.len(), frame.len());
}
