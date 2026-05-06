//! Integration tests for the byte-oriented stdio relay (§5).

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
