//! `cairn capture_trace` handler.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::domain::capture::CaptureEvent;
use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt as _, BufReader};

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Read newline-delimited JSON [`CaptureEvent`]s from `path`. Blank lines
/// are skipped; the first malformed line aborts the read.
///
/// # Errors
///
/// - File open / read failure (with path context).
/// - Any line that fails to parse as [`CaptureEvent`].
pub async fn read_jsonl_events(
    path: impl AsRef<Path>,
) -> anyhow::Result<Vec<CaptureEvent>> {
    let path = path.as_ref();
    let f = File::open(path)
        .await
        .with_context(|| format!("open trace JSONL at {}", path.display()))?;
    let mut lines = BufReader::new(f).lines();
    let mut events = Vec::new();
    let mut line_no = 0_usize;
    while let Some(line) = lines
        .next_line()
        .await
        .with_context(|| format!("read line from {}", path.display()))?
    {
        line_no += 1;
        if line.trim().is_empty() {
            continue;
        }
        let event: CaptureEvent = serde_json::from_str(&line)
            .with_context(|| format!("parse CaptureEvent at {}:{line_no}", path.display()))?;
        events.push(event);
    }
    Ok(events)
}

/// Run `cairn capture_trace`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let resp = unimplemented_response(ResponseVerb::CaptureTrace);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "capture_trace",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}
