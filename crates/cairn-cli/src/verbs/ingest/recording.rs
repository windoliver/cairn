//! Recording batch ingestion for `cairn ingest --recording`.

#![allow(
    dead_code,
    reason = "Recording planner pieces are staged before runtime ingestion wiring."
)]

use anyhow::Context as _;
use cairn_core::config::CairnConfig;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::domain::canonical::canonical_bytes;
use cairn_core::domain::identity::{keys::IdentityRevision, provision::ProvisionInput};
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, LocalSensorName, PayloadHash, Rfc3339Timestamp, ScopeTuple,
    SensorGateReason, SourceFamily,
};
use cairn_core::generated::common::Ulid as WireUlid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::ingest::{IngestData, IngestDataRecordingSummary};
use cairn_sensors_local::voice::{VoiceAudioChunk, VoiceDeviceMetadata, VoiceTranscriber};
use clap::ArgMatches;
use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;
use ulid::Ulid;

use crate::sensor_gate::{
    SensorDropBudgetMetric, SensorDropMetric, SensorGateStage, append_sensor_drop_metric,
    latest_sensor_consent_for_vault,
};
use crate::verbs::envelope::{
    emit_json, human_error, internal_error_response, invalid_args_response, new_operation_id,
};

/// Design sensor identity for user-triggered batch recording ingest.
const RECORDING_SENSOR_ID: &str = "snr:local:recording:default:v1";
const RECORDING_AUTHOR_ID: &str = "hmn:recording-ingest";
const RECORDING_CAPTURED_AT: &str = "2026-05-13T00:00:00Z";
const RECORDING_SESSION_ID: &str = "recording-batch";
const SUPPORTED_RECORDING_FORMATS: &str = "mp4, m4a, mp3, mkv, webm, wav";

#[derive(Debug, Clone, PartialEq)]
enum SegmentKind {
    AudioTranscript { speaker_id: String, confidence: f32 },
    FrameOcr { confidence: f32 },
}

#[derive(Debug, Clone, PartialEq)]
struct RecordingSegment {
    start_ms: u64,
    duration_ms: u64,
    text: String,
    kind: SegmentKind,
}

#[derive(Debug, Clone, PartialEq)]
struct RecordingPlan {
    media_path: PathBuf,
    media_hash: String,
    duration_ms: u64,
    file_size: u64,
    skipped_frames: u64,
    segments: Vec<RecordingSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandPlan {
    program: OsString,
    args: Vec<OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfmpegPlan {
    probe: CommandPlan,
    audio: CommandPlan,
    frames: CommandPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaProbe {
    duration_ms: u64,
    file_size: u64,
    has_audio: bool,
    has_video: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct SegmentPayload {
    segment_id: String,
    payload_hash: String,
    payload_json: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
struct StagedPayload {
    vault_relative_path: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
struct CaptureBatch {
    events: Vec<CaptureEvent>,
    payloads: Vec<StagedPayload>,
}

/// Run recording ingest, using deterministic fixture mode when configured.
#[must_use]
pub fn run(
    _sub: &ArgMatches,
    json: bool,
    recording_path: &Path,
    vault_root: &Path,
    config: CairnConfig,
) -> ExitCode {
    let started = Instant::now();

    if let Err(e) = validate_supported_recording_extension(recording_path) {
        return emit_invalid(json, &format!("{e:#}"));
    }

    if !recording_path.exists() {
        let reason = format!("path does not exist: {}", recording_path.display());
        return emit_invalid(json, &reason);
    }

    let media_size = match std::fs::metadata(recording_path) {
        Ok(metadata) => metadata.len(),
        Err(e) => {
            return emit_invalid(
                json,
                &format!(
                    "failed to read recording metadata {}: {e}",
                    recording_path.display()
                ),
            );
        }
    };
    if let Err(exit_code) = enforce_recording_sensor_gate(json, vault_root, &config, media_size) {
        return exit_code;
    }

    let plan = match build_recording_plan(recording_path) {
        Ok(plan) => plan,
        Err(e) => {
            return emit_invalid(json, &format!("{e:#}"));
        }
    };
    let batch = match build_capture_batch(&plan) {
        Ok(batch) => batch,
        Err(e) => {
            return emit_invalid(json, &format!("failed to plan recording ingest: {e:#}"));
        }
    };
    if batch.events.is_empty() {
        return emit_invalid(json, "recording produced no ingestible segments");
    }

    let summary_counts = SummaryCounts::from_plan(&plan);
    let record_id = WireUlid(batch.events[0].event_id.as_str().to_owned());
    let session_id = batch.events[0]
        .refs
        .as_ref()
        .and_then(|refs| refs.session_id.clone())
        .unwrap_or_else(|| RECORDING_SESSION_ID.to_owned());

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return emit_internal(json, &format!("runtime build: {e}")),
    };

    let result = rt.block_on(async { import_recording_batch(vault_root, config, batch).await });
    let policy_trace = match result {
        Ok(policy_trace) => policy_trace,
        Err(e) => return emit_internal(json, &format!("{e:#}")),
    };

    let resp = Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::Ingest(IngestData {
            cache_hits: None,
            cache_misses: None,
            cache_writes: None,
            files_processed: None,
            jsonl_summary: None,
            plan_ref: None,
            record_id,
            recording_summary: Some(IngestDataRecordingSummary {
                audio_segments: Some(summary_counts.audio_segments),
                elapsed_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
                frame_ocr_segments: Some(summary_counts.frame_ocr_segments),
                media_hash: Some(plan.media_hash.clone()),
                records_written: Some(summary_counts.segments),
                segments: Some(summary_counts.segments),
                skipped_frames: Some(plan.skipped_frames),
            }),
            session_id,
        })),
        error: None,
        operation_id: new_operation_id(),
        policy_trace,
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::Ingest,
    };

    if json {
        emit_json(&resp);
    } else if let Some(ResponseData::Ingest(data)) = resp.data.as_ref()
        && let Some(summary) = data.recording_summary.as_ref()
    {
        println!(
            "cairn ingest --recording: committed {} segments from {}",
            summary.segments.unwrap_or(0),
            summary.media_hash.as_deref().unwrap_or("unknown media")
        );
    }

    ExitCode::SUCCESS
}

fn enforce_recording_sensor_gate(
    json: bool,
    vault_root: &Path,
    config: &CairnConfig,
    media_size: u64,
) -> Result<(), ExitCode> {
    if !vault_root.join(".cairn").join("vault.id").exists() {
        return Ok(());
    }
    let observation = cairn_core::domain::BudgetObservation {
        items: 1,
        bytes: media_size,
    };
    let consent = match block_on(latest_sensor_consent_for_vault(
        vault_root,
        LocalSensorName::Recording,
    )) {
        Ok(consent) => consent,
        Err(error) => {
            return Err(emit_internal(
                json,
                &format!("failed to load recording sensor consent: {error:#}"),
            ));
        }
    };
    match crate::sensor_gate::evaluate_sensor_gate(
        config,
        consent,
        LocalSensorName::Recording,
        observation,
    ) {
        Ok(()) => Ok(()),
        Err(reason) => {
            let budget = (reason == SensorGateReason::BudgetExceeded)
                .then(|| {
                    crate::sensor_gate::sensor_budget(config, LocalSensorName::Recording).map(
                        |budget| SensorDropBudgetMetric {
                            max_items: budget.max_items,
                            max_bytes: budget.max_bytes,
                            observed_items: observation.items,
                            observed_bytes: observation.bytes,
                        },
                    )
                })
                .flatten();
            let metric = SensorDropMetric {
                event: crate::sensor_gate::SENSOR_DROP_EVENT,
                sensor: LocalSensorName::Recording,
                source_family: Some(SourceFamily::RecordingBatch),
                reason,
                stage: SensorGateStage::PreExtraction,
                operation_id: Some(new_operation_id().0),
                session_id: Some(RECORDING_SESSION_ID.to_owned()),
                turn_id: None,
                budget,
            };
            if let Err(error) = append_sensor_drop_metric(vault_root, &metric) {
                return Err(emit_internal(
                    json,
                    &format!("failed to write recording sensor drop metric: {error:#}"),
                ));
            }
            Err(emit_sensor_denied(json, reason))
        }
    }
}

fn block_on<T>(future: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)?
        .block_on(future)
}

#[derive(Debug, Clone, Copy)]
struct SummaryCounts {
    segments: u64,
    audio_segments: u64,
    frame_ocr_segments: u64,
}

impl SummaryCounts {
    fn from_plan(plan: &RecordingPlan) -> Self {
        let audio_segments = plan
            .segments
            .iter()
            .filter(|segment| matches!(segment.kind, SegmentKind::AudioTranscript { .. }))
            .count();
        let frame_ocr_segments = plan
            .segments
            .iter()
            .filter(|segment| matches!(segment.kind, SegmentKind::FrameOcr { .. }))
            .count();
        Self {
            segments: u64::try_from(plan.segments.len()).unwrap_or(u64::MAX),
            audio_segments: u64::try_from(audio_segments).unwrap_or(u64::MAX),
            frame_ocr_segments: u64::try_from(frame_ocr_segments).unwrap_or(u64::MAX),
        }
    }
}

async fn import_recording_batch(
    vault_root: &Path,
    config: CairnConfig,
    batch: CaptureBatch,
) -> anyhow::Result<Vec<ResponsePolicyTrace>> {
    let ctx = crate::verbs::signed::open_context(ResponseVerb::Ingest, vault_root, config)
        .await
        .map_err(signed_context_error)?;
    ensure_recording_issuer(&ctx).await?;
    let scope_binding = ScopeTuple {
        tenant: Some(super::DEFAULT_TENANT.to_owned()),
        workspace: Some(ctx.config.vault.name.clone()),
        entity: Some(super::INGEST_ENTITY.to_owned()),
        ..ScopeTuple::default()
    };

    let written = write_payloads(&ctx.vault_root, &batch.payloads).await?;
    let import_result = async {
        let response = crate::verbs::capture_trace::run_events_handler_with_scope(
            &ctx.store,
            &ctx.vault_root,
            batch.events,
            scope_binding,
        )
        .await?;
        if !response.failed_turns.is_empty() {
            anyhow::bail!(
                "capture import failed for recording ingest: {:?}",
                response.failed_turns
            );
        }
        Ok::<_, anyhow::Error>(response.policy_trace)
    }
    .await;

    if import_result.is_err() {
        cleanup_created_payloads(&ctx.vault_root, &written.created_paths).await;
    }

    import_result
}

async fn ensure_recording_issuer(
    ctx: &crate::verbs::signed::OpenedVerbContext,
) -> anyhow::Result<()> {
    let issuer = Identity::parse(super::DEFAULT_INGEST_ISSUER.to_owned())
        .context("parse default recording ingest issuer")?;
    let existing = ctx
        .identity
        .registry
        .get_identity(&issuer, IdentityVisibility::Operational)
        .await
        .context("identity lookup")?;
    if existing.is_some() {
        return Ok(());
    }

    let mut rng = rand_core::OsRng;
    let input = ProvisionInput {
        vault_id: ctx.identity.vault_id.clone(),
        id: issuer.clone(),
        kind: issuer.kind(),
        revision: IdentityRevision::FIRST,
    };
    ctx.identity
        .provision(issuer.kind(), input, &mut rng)
        .await
        .context("default recording ingest issuer provision")?;
    Ok(())
}

fn signed_context_error(response: Response) -> anyhow::Error {
    let error = response
        .error
        .map_or_else(|| "none".to_owned(), |error| error.to_string());
    anyhow::anyhow!(
        "signed context open: status={:?} error={error}",
        response.status
    )
}

#[derive(Debug, Default)]
struct PayloadWriteSet {
    created_paths: Vec<PathBuf>,
}

async fn write_payloads(
    vault_root: &Path,
    payloads: &[StagedPayload],
) -> anyhow::Result<PayloadWriteSet> {
    let mut written = PayloadWriteSet::default();
    for payload in payloads {
        let path = vault_root.join(&payload.vault_relative_path);
        if let Err(e) = write_one_payload(&path, &payload.bytes, &mut written).await {
            cleanup_created_payloads(vault_root, &written.created_paths).await;
            return Err(e);
        }
    }
    Ok(written)
}

async fn write_one_payload(
    path: &Path,
    bytes: &[u8],
    written: &mut PayloadWriteSet,
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("payload path has no parent: {}", path.display()))?;
    tokio::fs::create_dir_all(parent).await?;

    if tokio::fs::try_exists(path).await? {
        let metadata = tokio::fs::metadata(path).await?;
        if !metadata.is_file() {
            anyhow::bail!("payload path is not a file: {}", path.display());
        }
        let existing = tokio::fs::read(path).await?;
        if existing == bytes {
            return Ok(());
        }
        anyhow::bail!(
            "payload path already exists with different content: {}",
            path.display()
        );
    }

    let tmp_path = temp_payload_path(path)?;
    let write_result = async {
        tokio::fs::write(&tmp_path, bytes).await?;
        tokio::fs::rename(&tmp_path, path).await?;
        Ok::<_, std::io::Error>(())
    }
    .await;

    if let Err(e) = write_result {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(anyhow::Error::from(e))
            .with_context(|| format!("write payload {}", path.display()));
    }

    written.created_paths.push(path.to_path_buf());
    Ok(())
}

fn temp_payload_path(path: &Path) -> anyhow::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("payload path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("payload path has no UTF-8 filename: {}", path.display()))?;
    Ok(parent.join(format!(".{file_name}.{}.tmp", Ulid::new())))
}

async fn cleanup_created_payloads(vault_root: &Path, paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = tokio::fs::remove_file(path).await;
        cleanup_empty_payload_dirs(vault_root, path.parent()).await;
    }
}

async fn cleanup_empty_payload_dirs(vault_root: &Path, start: Option<&Path>) {
    let mut current = start;
    while let Some(dir) = current {
        if dir == vault_root || !dir.starts_with(vault_root) {
            break;
        }
        if tokio::fs::remove_dir(dir).await.is_err() {
            break;
        }
        current = dir.parent();
    }
}

fn recording_dir(plan: &RecordingPlan) -> &str {
    plan.media_hash
        .strip_prefix("sha256:")
        .unwrap_or(plan.media_hash.as_str())
}

fn emit_invalid(json: bool, reason: &str) -> ExitCode {
    let resp = invalid_args_response(ResponseVerb::Ingest, "recording", reason);
    if json {
        emit_json(&resp);
    } else {
        human_error("ingest", "InvalidArgs", reason, &resp.operation_id);
    }
    ExitCode::from(64)
}

fn emit_sensor_denied(json: bool, reason: SensorGateReason) -> ExitCode {
    let message = format!("sensor_gate:{}", reason.as_str());
    let resp = invalid_args_response(ResponseVerb::Ingest, "recording", &message);
    if json {
        emit_json(&resp);
    } else {
        human_error("ingest", "Unauthorized", &message, &resp.operation_id);
    }
    match reason {
        SensorGateReason::Disabled => ExitCode::from(78),
        SensorGateReason::PrivacyDenied | SensorGateReason::BudgetExceeded => ExitCode::from(77),
    }
}

fn emit_internal(json: bool, message: &str) -> ExitCode {
    let resp = internal_error_response(ResponseVerb::Ingest, message);
    if json {
        emit_json(&resp);
    } else {
        human_error("ingest", "Internal", message, &resp.operation_id);
    }
    ExitCode::FAILURE
}

fn validate_supported_recording_extension(path: &Path) -> anyhow::Result<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported recording format: missing extension; supported: {SUPPORTED_RECORDING_FORMATS}"
            )
        })?;

    match ext.as_str() {
        "mp4" | "m4a" | "mp3" | "mkv" | "webm" | "wav" => Ok(()),
        other => anyhow::bail!(
            "unsupported recording format `{other}`; supported: {SUPPORTED_RECORDING_FORMATS}"
        ),
    }
}

fn validate_fixture_media_path(
    recording_path: &Path,
    fixture_media_path: &Path,
) -> anyhow::Result<()> {
    if recording_path == fixture_media_path || recording_path.ends_with(fixture_media_path) {
        return Ok(());
    }

    if fixture_media_path.is_absolute()
        && let (Ok(recording), Ok(fixture)) = (
            std::fs::canonicalize(recording_path),
            std::fs::canonicalize(fixture_media_path),
        )
        && recording == fixture
    {
        return Ok(());
    }

    anyhow::bail!(
        "recording fixture media_path does not match --recording path: fixture {}, recording {}",
        fixture_media_path.display(),
        recording_path.display()
    )
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixturePlan {
    media_path: PathBuf,
    media_sha256: String,
    duration_ms: u64,
    file_size: u64,
    #[serde(default)]
    audio: Vec<FixtureAudio>,
    #[serde(default)]
    frames: Vec<FixtureFrame>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureAudio {
    start_ms: u64,
    duration_ms: u64,
    speaker_id: String,
    confidence: f32,
    text: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureFrame {
    timestamp_ms: u64,
    duration_ms: u64,
    confidence: f32,
    text: String,
}

#[derive(Debug)]
struct FrameObservation {
    timestamp_ms: u64,
    fixture_index: usize,
    duration_ms: u64,
    confidence: f32,
    text: String,
}

#[derive(Debug, Clone, PartialEq)]
struct FrameOcrObservation {
    timestamp_ms: u64,
    duration_ms: u64,
    confidence: f32,
    text: String,
}

fn frame_observations_to_segments(
    mut observations: Vec<FrameOcrObservation>,
) -> (Vec<RecordingSegment>, u64) {
    let mut skipped = 0_u64;
    let mut previous: Option<String> = None;
    let mut segments = Vec::new();

    observations.sort_by_key(|observation| observation.timestamp_ms);
    for observation in observations {
        let text = normalize_text(&observation.text);
        if text.is_empty() || observation.duration_ms == 0 {
            skipped += 1;
            continue;
        }
        if previous.as_deref() == Some(text.as_str()) {
            skipped += 1;
            continue;
        }
        previous = Some(text.clone());
        segments.push(RecordingSegment {
            start_ms: observation.timestamp_ms,
            duration_ms: observation.duration_ms,
            text,
            kind: SegmentKind::FrameOcr {
                confidence: observation.confidence,
            },
        });
    }

    (segments, skipped)
}

fn build_ffmpeg_plan(input: &Path, temp_dir: &Path) -> FfmpegPlan {
    let audio_out = temp_dir.join("audio.wav");
    let frames_out = temp_dir.join("frame-%06d.png");
    let input_arg = command_path_arg(input);

    FfmpegPlan {
        probe: CommandPlan {
            program: OsString::from("ffprobe"),
            args: vec![
                OsString::from("-nostdin"),
                OsString::from("-v"),
                OsString::from("error"),
                OsString::from("-show_format"),
                OsString::from("-show_streams"),
                OsString::from("-of"),
                OsString::from("json"),
                input_arg.clone(),
            ],
        },
        audio: CommandPlan {
            program: OsString::from("ffmpeg"),
            args: vec![
                OsString::from("-nostdin"),
                OsString::from("-hide_banner"),
                OsString::from("-loglevel"),
                OsString::from("error"),
                OsString::from("-y"),
                OsString::from("-i"),
                input_arg.clone(),
                OsString::from("-vn"),
                OsString::from("-acodec"),
                OsString::from("pcm_s16le"),
                OsString::from("-ac"),
                OsString::from("1"),
                OsString::from("-ar"),
                OsString::from("16000"),
                audio_out.into_os_string(),
            ],
        },
        frames: CommandPlan {
            program: OsString::from("ffmpeg"),
            args: vec![
                OsString::from("-nostdin"),
                OsString::from("-hide_banner"),
                OsString::from("-loglevel"),
                OsString::from("error"),
                OsString::from("-y"),
                OsString::from("-i"),
                input_arg,
                OsString::from("-vf"),
                OsString::from("fps=1"),
                frames_out.into_os_string(),
            ],
        },
    }
}

fn command_path_arg(path: &Path) -> OsString {
    if path.is_relative() && path_first_component_starts_with_dash(path) {
        return PathBuf::from(".").join(path).into_os_string();
    }
    path.as_os_str().to_owned()
}

fn path_first_component_starts_with_dash(path: &Path) -> bool {
    let Some(std::path::Component::Normal(first)) = path.components().next() else {
        return false;
    };
    os_str_starts_with_dash(first)
}

#[cfg(unix)]
fn os_str_starts_with_dash(value: &OsStr) -> bool {
    value.as_bytes().first() == Some(&b'-')
}

#[cfg(not(unix))]
fn os_str_starts_with_dash(value: &OsStr) -> bool {
    value.to_string_lossy().starts_with('-')
}

fn parse_ffprobe_json(raw: &str) -> anyhow::Result<MediaProbe> {
    let value: serde_json::Value = serde_json::from_str(raw).context("malformed ffprobe JSON")?;
    let duration_raw = value
        .pointer("/format/duration")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ffprobe output missing format.duration"))?;
    let duration_ms = parse_duration_ms(duration_raw)
        .with_context(|| format!("invalid format.duration `{duration_raw}`"))?;
    let size_raw = value
        .pointer("/format/size")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ffprobe output missing format.size"))?;
    let file_size = size_raw
        .parse::<u64>()
        .with_context(|| format!("invalid format.size `{size_raw}`"))?;
    let streams = value
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("ffprobe output missing streams"))?;
    let has_audio = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("audio")
    });
    let has_video = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
    });

    if !has_audio && !has_video {
        anyhow::bail!("ffprobe output contains no audio or video streams");
    }

    Ok(MediaProbe {
        duration_ms,
        file_size,
        has_audio,
        has_video,
    })
}

fn parse_duration_ms(raw: &str) -> anyhow::Result<u64> {
    if raw.is_empty() || raw.starts_with('-') {
        anyhow::bail!("duration must be a non-negative seconds string");
    }

    let (whole, fractional) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fractional.bytes().all(|b| b.is_ascii_digit())
    {
        anyhow::bail!("duration must contain only decimal digits");
    }

    let whole_ms = whole
        .parse::<u64>()?
        .checked_mul(1000)
        .ok_or_else(|| anyhow::anyhow!("duration milliseconds overflow"))?;
    let mut fractional_ms = 0_u64;
    let mut place = 100_u64;
    for digit in fractional.bytes().take(3) {
        fractional_ms += u64::from(digit - b'0') * place;
        place /= 10;
    }
    if fractional
        .as_bytes()
        .get(3)
        .is_some_and(|digit| *digit >= b'5')
    {
        fractional_ms += 1;
    }

    whole_ms
        .checked_add(fractional_ms)
        .ok_or_else(|| anyhow::anyhow!("duration milliseconds overflow"))
}

fn run_command_capture_stdout(plan: &CommandPlan) -> anyhow::Result<String> {
    let output = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run {}: {e}", command_program_display(plan)))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} failed with status {:?}: {}",
            command_program_display(plan),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).map_err(|e| {
        anyhow::anyhow!(
            "{} emitted non-UTF8 stdout: {e}",
            command_program_display(plan)
        )
    })
}

fn run_command_no_stdout(plan: &CommandPlan) -> anyhow::Result<()> {
    let output = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run {}: {e}", command_program_display(plan)))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} failed with status {:?}: {}",
            command_program_display(plan),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn command_program_display(plan: &CommandPlan) -> String {
    plan.program.to_string_lossy().into_owned()
}

fn run_tesseract_ocr(frame_path: &Path) -> anyhow::Result<String> {
    run_tesseract_ocr_with_program(frame_path, OsString::from("tesseract"))
}

fn run_tesseract_ocr_with_program(frame_path: &Path, program: OsString) -> anyhow::Result<String> {
    let plan = CommandPlan {
        program,
        args: vec![command_path_arg(frame_path), OsString::from("stdout")],
    };
    run_command_capture_stdout(&plan)
        .with_context(|| format!("tesseract OCR failed for {}", frame_path.display()))
}

fn frame_timestamp_ms_from_name(path: &Path) -> u64 {
    if path.extension().and_then(OsStr::to_str) != Some("png") {
        return 0;
    }

    let Some(stem) = path.file_stem().and_then(OsStr::to_str) else {
        return 0;
    };
    let Some(digits) = stem.strip_prefix("frame-") else {
        return 0;
    };
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return 0;
    }

    digits
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_sub(1)
        .saturating_mul(1000)
}

fn wav_bytes_to_chunks(
    bytes: &[u8],
    event_seed: &str,
    captured_at: &str,
) -> anyhow::Result<Vec<VoiceAudioChunk>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("extracted audio is not a RIFF/WAVE file");
    }

    let event_id = CaptureEventId::parse(event_seed)
        .with_context(|| format!("invalid recording audio event id `{event_seed}`"))?;
    let captured_at = Rfc3339Timestamp::parse(captured_at)
        .with_context(|| format!("invalid recording audio captured_at `{captured_at}`"))?;
    let mut offset = 12_usize;
    let mut sample_rate = None;
    let mut channels = None;
    let mut bits_per_sample = None;
    let mut data = None;

    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        offset += 8;
        if offset + len > bytes.len() {
            anyhow::bail!("WAV chunk length exceeds file size");
        }

        match id {
            b"fmt " => {
                if len < 16 {
                    anyhow::bail!("WAV fmt chunk too short");
                }
                let audio_format = u16::from_le_bytes(bytes[offset..offset + 2].try_into()?);
                if audio_format != 1 {
                    anyhow::bail!("only PCM WAV audio is supported");
                }
                channels = Some(u16::from_le_bytes(
                    bytes[offset + 2..offset + 4].try_into()?,
                ));
                sample_rate = Some(u32::from_le_bytes(
                    bytes[offset + 4..offset + 8].try_into()?,
                ));
                bits_per_sample = Some(u16::from_le_bytes(
                    bytes[offset + 14..offset + 16].try_into()?,
                ));
            }
            b"data" => data = Some(&bytes[offset..offset + len]),
            _ => {}
        }

        offset += len + (len % 2);
    }

    let sample_rate = sample_rate.ok_or_else(|| anyhow::anyhow!("WAV missing fmt sample rate"))?;
    if sample_rate == 0 {
        anyhow::bail!("WAV sample rate must be greater than zero");
    }
    let channels = channels.ok_or_else(|| anyhow::anyhow!("WAV missing channel count"))?;
    if channels != 1 {
        anyhow::bail!("recording audio extraction must produce mono WAV");
    }
    if bits_per_sample != Some(16) {
        anyhow::bail!("only 16-bit PCM WAV is supported");
    }
    let data = data.ok_or_else(|| anyhow::anyhow!("WAV missing data chunk"))?;
    if data.len() % 2 != 0 {
        anyhow::bail!("16-bit PCM data length must be even");
    }

    let mut samples = Vec::with_capacity(data.len() / 2);
    for pair in data.chunks_exact(2) {
        let sample = i16::from_le_bytes([pair[0], pair[1]]);
        samples.push(f32::from(sample) / 32768.0);
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let duration_ms = u64::try_from(samples.len())
        .map_err(|_| anyhow::anyhow!("WAV sample count is too large"))?
        .checked_mul(1000)
        .ok_or_else(|| anyhow::anyhow!("WAV duration milliseconds overflow"))?
        / u64::from(sample_rate);
    Ok(vec![VoiceAudioChunk {
        event_id,
        captured_at: captured_at.clone(),
        started_at: captured_at,
        duration_ms,
        samples,
        device: VoiceDeviceMetadata {
            name: "recording file".to_owned(),
            host: "ffmpeg".to_owned(),
            sample_rate_hz: sample_rate,
            channels,
        },
        refs: None,
    }])
}

fn transcribe_chunks<T: VoiceTranscriber>(
    chunks: &[VoiceAudioChunk],
    transcriber: &T,
) -> anyhow::Result<Vec<RecordingSegment>> {
    let mut segments = Vec::new();
    let mut cursor_ms = 0_u64;
    for chunk in chunks {
        let transcript = transcriber
            .transcribe(chunk)
            .map_err(|e| anyhow::anyhow!("recording audio transcription failed: {e}"))?;
        let text = normalize_text(&transcript.text);
        let duration_ms = chunk.duration_ms.max(1);
        if !text.is_empty() {
            segments.push(RecordingSegment {
                start_ms: cursor_ms,
                duration_ms,
                text,
                kind: SegmentKind::AudioTranscript {
                    speaker_id: transcript.speaker_id,
                    confidence: transcript.confidence,
                },
            });
        }
        cursor_ms = cursor_ms.saturating_add(duration_ms);
    }
    Ok(segments)
}

#[cfg(feature = "voice-runtime")]
fn build_sherpa_transcriber_from_env()
-> anyhow::Result<cairn_sensors_local::voice_runtime::SherpaOnnxTranscriber> {
    use cairn_sensors_local::voice_runtime::{SherpaOnnxTranscriber, SherpaOnnxTranscriberConfig};

    let model = std::env::var_os("CAIRN_SHERPA_MODEL").ok_or_else(|| {
        anyhow::anyhow!("CAIRN_SHERPA_MODEL must point to a SenseVoice ONNX model")
    })?;
    let tokens = std::env::var_os("CAIRN_SHERPA_TOKENS")
        .ok_or_else(|| anyhow::anyhow!("CAIRN_SHERPA_TOKENS must point to sherpa tokens.txt"))?;
    let config =
        SherpaOnnxTranscriberConfig::sense_voice(PathBuf::from(model), PathBuf::from(tokens));

    SherpaOnnxTranscriber::from_config(config)
        .map_err(|e| anyhow::anyhow!("load sherpa-onnx transcriber: {e}"))
}

#[cfg(not(feature = "voice-runtime"))]
fn build_sherpa_transcriber_from_env() -> anyhow::Result<()> {
    anyhow::bail!(
        "recording audio transcription requires building cairn-cli with --features voice-runtime"
    )
}

fn build_recording_plan(recording_path: &Path) -> anyhow::Result<RecordingPlan> {
    if let Some(fixture_path) = std::env::var_os("CAIRN_RECORDING_FIXTURE_JSON")
        && !fixture_path.as_os_str().is_empty()
    {
        let fixture_path = PathBuf::from(fixture_path);
        let fixture_raw = std::fs::read_to_string(&fixture_path).with_context(|| {
            format!(
                "failed to read CAIRN_RECORDING_FIXTURE_JSON {}",
                fixture_path.display()
            )
        })?;
        let plan = parse_fixture_plan(&fixture_raw).with_context(|| {
            format!(
                "failed to parse CAIRN_RECORDING_FIXTURE_JSON {}",
                fixture_path.display()
            )
        })?;
        validate_fixture_media_path(recording_path, &plan.media_path)?;
        return Ok(plan);
    }

    build_real_runtime_plan(recording_path)
}

fn build_real_runtime_plan(recording_path: &Path) -> anyhow::Result<RecordingPlan> {
    validate_supported_recording_extension(recording_path)?;

    let temp = tempfile::tempdir().context("create recording runtime temp directory")?;
    let ffmpeg = build_ffmpeg_plan(recording_path, temp.path());
    let probe =
        run_command_capture_stdout(&ffmpeg.probe).context("probe recording with ffprobe")?;
    let metadata = parse_ffprobe_json(&probe)?;
    let media_hash = sha256_file(recording_path)
        .with_context(|| format!("hash recording media {}", recording_path.display()))?;

    let mut segments = Vec::new();
    let mut skipped_frames = 0_u64;

    if metadata.has_audio {
        run_command_no_stdout(&ffmpeg.audio).context("extract recording audio with ffmpeg")?;
        let wav_path = temp.path().join("audio.wav");
        let wav = std::fs::read(&wav_path)
            .with_context(|| format!("read extracted recording audio {}", wav_path.display()))?;
        let chunks =
            wav_bytes_to_chunks(&wav, "01ARZ3NDEKTSV4RRFFQ69G5FE1", RECORDING_CAPTURED_AT)?;

        #[cfg(feature = "voice-runtime")]
        {
            let transcriber = build_sherpa_transcriber_from_env()?;
            segments.extend(transcribe_chunks(&chunks, &transcriber)?);
        }
        #[cfg(not(feature = "voice-runtime"))]
        {
            let _ = chunks;
            build_sherpa_transcriber_from_env()?;
        }
    }

    if metadata.has_video {
        run_command_no_stdout(&ffmpeg.frames).context("extract recording frames with ffmpeg")?;
        let mut observations = Vec::new();
        for entry in std::fs::read_dir(temp.path()).context("read extracted recording frames")? {
            let path = entry?.path();
            if path.extension().and_then(OsStr::to_str) != Some("png") {
                continue;
            }
            let text = run_tesseract_ocr(&path)?;
            observations.push(FrameOcrObservation {
                timestamp_ms: frame_timestamp_ms_from_name(&path),
                duration_ms: 1000,
                confidence: 1.0,
                text,
            });
        }
        let (ocr_segments, skipped) = frame_observations_to_segments(observations);
        skipped_frames = skipped;
        segments.extend(ocr_segments);
    }

    segments.sort_by_key(|segment| (segment.start_ms, segment.kind.sort_rank()));
    if segments.is_empty() {
        anyhow::bail!("recording produced no transcript or OCR segments");
    }

    Ok(RecordingPlan {
        media_path: recording_path.to_path_buf(),
        media_hash,
        duration_ms: metadata.duration_ms,
        file_size: metadata.file_size,
        skipped_frames,
        segments,
    })
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn parse_fixture_plan(raw: &str) -> anyhow::Result<RecordingPlan> {
    let fixture: FixturePlan = serde_json::from_str(raw)?;
    if !is_sha256_wire(&fixture.media_sha256) {
        anyhow::bail!("recording fixture media_sha256 must be sha256:<64 lowercase hex>");
    }

    let mut segments = Vec::new();
    for audio in fixture.audio {
        let text = normalize_text(&audio.text);
        if text.is_empty() || audio.duration_ms == 0 {
            continue;
        }
        segments.push(RecordingSegment {
            start_ms: audio.start_ms,
            duration_ms: audio.duration_ms,
            text,
            kind: SegmentKind::AudioTranscript {
                speaker_id: audio.speaker_id,
                confidence: audio.confidence,
            },
        });
    }

    let mut skipped_frames = 0_u64;
    let mut frames = Vec::new();
    for (fixture_index, frame) in fixture.frames.into_iter().enumerate() {
        let text = normalize_text(&frame.text);
        if text.is_empty() || frame.duration_ms == 0 {
            skipped_frames += 1;
            continue;
        }
        frames.push(FrameObservation {
            timestamp_ms: frame.timestamp_ms,
            fixture_index,
            duration_ms: frame.duration_ms,
            confidence: frame.confidence,
            text,
        });
    }

    frames.sort_by_key(|frame| (frame.timestamp_ms, frame.fixture_index));

    let mut previous_frame_text: Option<String> = None;
    for frame in frames {
        if previous_frame_text.as_deref() == Some(frame.text.as_str()) {
            skipped_frames += 1;
            continue;
        }
        previous_frame_text = Some(frame.text.clone());
        segments.push(RecordingSegment {
            start_ms: frame.timestamp_ms,
            duration_ms: frame.duration_ms,
            text: frame.text,
            kind: SegmentKind::FrameOcr {
                confidence: frame.confidence,
            },
        });
    }

    segments.sort_by_key(|segment| (segment.start_ms, segment.kind.sort_rank()));

    Ok(RecordingPlan {
        media_path: fixture.media_path,
        media_hash: fixture.media_sha256,
        duration_ms: fixture.duration_ms,
        file_size: fixture.file_size,
        skipped_frames,
        segments,
    })
}

fn build_segment_payload(
    plan: &RecordingPlan,
    segment: &RecordingSegment,
) -> anyhow::Result<SegmentPayload> {
    let track_kind = match &segment.kind {
        SegmentKind::AudioTranscript { .. } => "audio_transcript",
        SegmentKind::FrameOcr { .. } => "frame_ocr",
    };
    let normalized_for_id = normalize_text(&segment.text).to_lowercase();
    let id_input = format!(
        "{}\n{}\n{}\n{}\n{}",
        plan.media_hash, track_kind, segment.start_ms, segment.duration_ms, normalized_for_id
    );
    let segment_id = format!(
        "recseg-{}",
        hex_prefix(&Sha256::digest(id_input.as_bytes()), 24)
    );

    let detail = match &segment.kind {
        SegmentKind::AudioTranscript {
            speaker_id,
            confidence,
        } => serde_json::json!({
            "speaker_id": speaker_id,
            "confidence": confidence,
        }),
        SegmentKind::FrameOcr { confidence } => serde_json::json!({
            "confidence": confidence,
            "timestamp_ms": segment.start_ms,
        }),
    };
    let value = serde_json::json!({
        "media": {
            "path": plan.media_path.to_string_lossy(),
            "sha256": plan.media_hash,
            "file_size": plan.file_size,
            "duration_ms": plan.duration_ms,
        },
        "segment": {
            "id": segment_id,
            "track_kind": track_kind,
            "start_ms": segment.start_ms,
            "duration_ms": segment.duration_ms,
            "text": segment.text,
            "detail": detail,
        },
        "tools": {
            "ffmpeg": "fixture",
            "ocr": "fixture"
        }
    });
    let payload_json = canonical_bytes(&value)?;
    let payload_hash = format!("sha256:{:x}", Sha256::digest(&payload_json));

    Ok(SegmentPayload {
        segment_id,
        payload_hash,
        payload_json,
    })
}

fn build_segment_payloads(plan: &RecordingPlan) -> anyhow::Result<Vec<SegmentPayload>> {
    let mut seen_segment_ids = HashSet::new();
    let mut payloads = Vec::with_capacity(plan.segments.len());

    for segment in &plan.segments {
        let payload = build_segment_payload(plan, segment)?;
        if !seen_segment_ids.insert(payload.segment_id.clone()) {
            anyhow::bail!(
                "duplicate recording segment_id {} for media {} at start_ms {}",
                payload.segment_id,
                plan.media_hash,
                segment.start_ms
            );
        }
        payloads.push(payload);
    }

    Ok(payloads)
}

fn build_capture_batch(plan: &RecordingPlan) -> anyhow::Result<CaptureBatch> {
    let segment_payloads = build_segment_payloads(plan)?;
    let sensor = Identity::parse(RECORDING_SENSOR_ID)?;
    let author = Identity::parse(RECORDING_AUTHOR_ID)?;
    let recording_dir = plan
        .media_hash
        .strip_prefix("sha256:")
        .unwrap_or(plan.media_hash.as_str());
    let turn_id = format!("recording-{recording_dir}");
    let mut events = Vec::with_capacity(segment_payloads.len());
    let mut payloads = Vec::with_capacity(segment_payloads.len());

    for (ordinal, (segment, segment_payload)) in
        plan.segments.iter().zip(segment_payloads).enumerate()
    {
        let event_id = deterministic_event_id(&segment_payload.segment_id)?;
        let captured_at = segment_captured_at(segment, ordinal)?;
        let payload_ref = format!("sources/recordings/{recording_dir}/{event_id}.json");
        let event = CaptureEvent::try_new(
            event_id,
            sensor.clone(),
            CaptureMode::Explicit,
            vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: author.clone(),
                at: captured_at.clone(),
            }],
            Some(CaptureRefs {
                session_id: Some(RECORDING_SESSION_ID.to_owned()),
                turn_id: Some(turn_id.clone()),
                tool_id: None,
            }),
            PayloadHash::parse(segment_payload.payload_hash.clone())?,
            payload_ref.clone(),
            captured_at.clone(),
            CapturePayload::RecordingBatch {
                segment_start_ms: segment.start_ms,
                segment_duration_ms: segment.duration_ms,
            },
            SourceFamily::RecordingBatch,
        )?;

        payloads.push(StagedPayload {
            vault_relative_path: payload_ref,
            bytes: segment_payload.payload_json,
        });
        events.push(event);
    }

    Ok(CaptureBatch { events, payloads })
}

fn deterministic_event_id(seed: &str) -> anyhow::Result<CaptureEventId> {
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    let ulid = Ulid::from_bytes(bytes).to_string();
    Ok(CaptureEventId::parse(ulid)?)
}

fn segment_captured_at(
    segment: &RecordingSegment,
    ordinal: usize,
) -> anyhow::Result<Rfc3339Timestamp> {
    let base =
        chrono::DateTime::parse_from_rfc3339(RECORDING_CAPTURED_AT)?.with_timezone(&chrono::Utc);
    let segment_offset_ms = i64::try_from(segment.start_ms)
        .map_err(|_| anyhow::anyhow!("segment start_ms does not fit timestamp offset"))?;
    let ordinal_offset_us = i64::try_from(ordinal)
        .map_err(|_| anyhow::anyhow!("segment ordinal does not fit timestamp offset"))?;
    let captured_at = base
        + chrono::Duration::milliseconds(segment_offset_ms)
        + chrono::Duration::microseconds(ordinal_offset_us);

    Ok(Rfc3339Timestamp::parse(
        captured_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
    )?)
}

impl SegmentKind {
    const fn sort_rank(&self) -> u8 {
        match self {
            Self::AudioTranscript { .. } => 0,
            Self::FrameOcr { .. } => 1,
        }
    }
}

fn normalize_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_sha256_wire(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    let mut out = String::with_capacity(chars);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
        if out.len() >= chars {
            out.truncate(chars);
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::domain::{CaptureMode, ChainRole, SourceId};
    use cairn_sensors_local::voice::{VoiceAudioChunk, VoiceTranscriber, VoiceTranscript};
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt as _;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    fn pcm16_wav_bytes(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let byte_rate = sample_rate * u32::from(channels) * 2;
        let block_align = channels * 2;
        let mut wav = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + u32::try_from(data_len).expect("data len")).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&u32::try_from(data_len).expect("data len").to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    fn pcm_wav_bytes_with_bits(
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        data: &[u8],
    ) -> Vec<u8> {
        let bytes_per_sample = bits_per_sample / 8;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bytes_per_sample);
        let block_align = channels * bytes_per_sample;
        let mut wav = Vec::with_capacity(44 + data.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + u32::try_from(data.len()).expect("data len")).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits_per_sample.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&u32::try_from(data.len()).expect("data len").to_le_bytes());
        wav.extend_from_slice(data);
        wav
    }

    fn test_chunk(duration_ms: u64) -> VoiceAudioChunk {
        VoiceAudioChunk {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FE0").expect("event id"),
            captured_at: Rfc3339Timestamp::parse("2026-05-13T12:00:00Z").expect("captured at"),
            started_at: Rfc3339Timestamp::parse("2026-05-13T12:00:00Z").expect("started at"),
            duration_ms,
            samples: vec![0.0],
            device: cairn_sensors_local::voice::VoiceDeviceMetadata {
                name: "recording file".to_owned(),
                host: "ffmpeg".to_owned(),
                sample_rate_hz: 16_000,
                channels: 1,
            },
            refs: None,
        }
    }

    const FIXTURE: &str = r#"{
      "media_path": "fixtures/v0/recordings/demo.mp4",
      "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
      "duration_ms": 5200,
      "file_size": 1234,
      "audio": [
        {"start_ms": 0, "duration_ms": 1800, "speaker_id": "unknown_speaker_01", "confidence": 0.91, "text": "alpha recording launch note"},
        {"start_ms": 3200, "duration_ms": 900, "speaker_id": "unknown_speaker_02", "confidence": 0.86, "text": "beta follow up action"}
      ],
      "frames": [
        {"timestamp_ms": 2000, "duration_ms": 1000, "confidence": 0.82, "text": "screen shows gamma config"},
        {"timestamp_ms": 3000, "duration_ms": 1000, "confidence": 0.80, "text": "screen shows gamma config"},
        {"timestamp_ms": 4200, "duration_ms": 1000, "confidence": 0.70, "text": "   "}
      ]
    }"#;

    #[test]
    fn wav_reader_yields_voice_audio_chunks() {
        let wav = pcm16_wav_bytes(16_000, 1, &[0_i16, 16_384, -16_384, i16::MAX]);
        let chunks =
            wav_bytes_to_chunks(&wav, "01ARZ3NDEKTSV4RRFFQ69G5FE0", "2026-05-13T12:00:00Z")
                .expect("wav chunks");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].event_id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FE0");
        assert_eq!(chunks[0].captured_at.as_str(), "2026-05-13T12:00:00Z");
        assert_eq!(chunks[0].started_at.as_str(), "2026-05-13T12:00:00Z");
        assert_eq!(chunks[0].device.sample_rate_hz, 16_000);
        assert_eq!(chunks[0].device.channels, 1);
        assert_eq!(chunks[0].duration_ms, 0);
        assert_eq!(chunks[0].samples.len(), 4);
        assert!(chunks[0].samples[0].abs() < f32::EPSILON);
        assert!((chunks[0].samples[1] - 0.5).abs() < 0.001);
        assert!((chunks[0].samples[2] + 0.5).abs() < 0.001);
        assert!(chunks[0].samples[3] <= 1.0);
    }

    #[test]
    fn wav_reader_rejects_malformed_or_non_wav_bytes() {
        for (bytes, expected) in [
            (b"not a wave".to_vec(), "not a RIFF/WAVE file"),
            (
                {
                    let mut wav = pcm16_wav_bytes(16_000, 1, &[0_i16]);
                    wav[40..44].copy_from_slice(&10_000_u32.to_le_bytes());
                    wav.truncate(44);
                    wav
                },
                "WAV chunk length exceeds file size",
            ),
        ] {
            let err =
                wav_bytes_to_chunks(&bytes, "01ARZ3NDEKTSV4RRFFQ69G5FE0", "2026-05-13T12:00:00Z")
                    .expect_err("wav should reject");
            let message = format!("{err:#}");
            assert!(
                message.contains(expected),
                "expected `{expected}` in {message}"
            );
        }
    }

    #[test]
    fn wav_reader_rejects_unsupported_channels_or_bit_depth() {
        let stereo = pcm16_wav_bytes(16_000, 2, &[0_i16, 1_i16]);
        let stereo_err = wav_bytes_to_chunks(
            &stereo,
            "01ARZ3NDEKTSV4RRFFQ69G5FE0",
            "2026-05-13T12:00:00Z",
        )
        .expect_err("stereo should reject");
        assert!(
            format!("{stereo_err:#}").contains("must produce mono WAV"),
            "unexpected error: {stereo_err:#}"
        );

        let pcm8 = pcm_wav_bytes_with_bits(16_000, 1, 8, &[0_u8, 128_u8]);
        let bit_depth_err =
            wav_bytes_to_chunks(&pcm8, "01ARZ3NDEKTSV4RRFFQ69G5FE0", "2026-05-13T12:00:00Z")
                .expect_err("8-bit PCM should reject");
        assert!(
            format!("{bit_depth_err:#}").contains("only 16-bit PCM WAV is supported"),
            "unexpected error: {bit_depth_err:#}"
        );
    }

    #[test]
    fn wav_reader_returns_no_chunks_for_zero_samples() {
        let wav = pcm16_wav_bytes(16_000, 1, &[]);
        let chunks =
            wav_bytes_to_chunks(&wav, "01ARZ3NDEKTSV4RRFFQ69G5FE0", "2026-05-13T12:00:00Z")
                .expect("empty wav parses");

        assert!(chunks.is_empty());
    }

    #[test]
    fn wav_reader_rejects_odd_pcm_data_length() {
        let wav = pcm_wav_bytes_with_bits(16_000, 1, 16, &[0_u8]);
        let err = wav_bytes_to_chunks(&wav, "01ARZ3NDEKTSV4RRFFQ69G5FE0", "2026-05-13T12:00:00Z")
            .expect_err("odd PCM byte length should reject");
        let message = format!("{err:#}");

        assert!(
            message.contains("16-bit PCM data length must be even"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn transcribe_chunks_maps_non_empty_transcripts_to_audio_segments() {
        struct MockTranscriber {
            transcripts: Vec<VoiceTranscript>,
            next: std::cell::Cell<usize>,
        }

        impl VoiceTranscriber for MockTranscriber {
            fn transcribe(&self, _chunk: &VoiceAudioChunk) -> Result<VoiceTranscript, String> {
                let index = self.next.get();
                self.next.set(index + 1);
                self.transcripts
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("missing transcript {index}"))
            }
        }

        let chunks = vec![test_chunk(0), test_chunk(250), test_chunk(100)];
        let transcriber = MockTranscriber {
            transcripts: vec![
                VoiceTranscript {
                    speaker_id: "speaker-a".to_owned(),
                    text: " alpha   launch ".to_owned(),
                    confidence: 0.91,
                },
                VoiceTranscript {
                    speaker_id: "speaker-b".to_owned(),
                    text: "   ".to_owned(),
                    confidence: 0.5,
                },
                VoiceTranscript {
                    speaker_id: "speaker-c".to_owned(),
                    text: "beta follow up".to_owned(),
                    confidence: 0.86,
                },
            ],
            next: std::cell::Cell::new(0),
        };

        let segments = transcribe_chunks(&chunks, &transcriber).expect("transcribes chunks");

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ms, 0);
        assert_eq!(segments[0].duration_ms, 1);
        assert_eq!(segments[0].text, "alpha launch");
        assert_eq!(segments[1].start_ms, 251);
        assert_eq!(segments[1].duration_ms, 100);
        assert_eq!(segments[1].text, "beta follow up");
        assert_eq!(
            segments
                .iter()
                .map(|segment| match &segment.kind {
                    SegmentKind::AudioTranscript {
                        speaker_id,
                        confidence,
                    } => (speaker_id.as_str(), *confidence),
                    SegmentKind::FrameOcr { .. } => panic!("expected audio transcript"),
                })
                .collect::<Vec<_>>(),
            vec![("speaker-a", 0.91), ("speaker-c", 0.86)]
        );
    }

    #[test]
    fn ffmpeg_commands_are_constructed_without_shell_interpolation() {
        let input = PathBuf::from("/tmp/demo file; $(touch nope).mp4");
        let temp = PathBuf::from("/tmp/cairn-recording");
        let commands = build_ffmpeg_plan(&input, &temp);

        assert_eq!(commands.probe.program, os("ffprobe"));
        assert_eq!(
            commands.probe.args,
            vec![
                os("-nostdin"),
                os("-v"),
                os("error"),
                os("-show_format"),
                os("-show_streams"),
                os("-of"),
                os("json"),
                input.as_os_str().to_owned(),
            ],
            "ffprobe should receive safe, distinct arguments"
        );
        assert_eq!(commands.audio.program, os("ffmpeg"));
        assert!(
            commands.audio.args.contains(&os("-nostdin")),
            "ffmpeg audio command should not read stdin"
        );
        assert!(commands.audio.args.contains(&os("-i")));
        assert!(
            commands.audio.args.contains(&input.as_os_str().to_owned()),
            "input path should be passed as its own argument"
        );
        assert!(
            commands
                .audio
                .args
                .contains(&temp.join("audio.wav").into_os_string()),
            "audio extraction target should be under temp dir"
        );
        assert_eq!(commands.frames.program, os("ffmpeg"));
        assert!(
            commands.frames.args.contains(&os("-nostdin")),
            "ffmpeg frame command should not read stdin"
        );
        assert!(
            commands.frames.args.contains(&input.as_os_str().to_owned()),
            "frame extraction input path should be passed as its own argument"
        );
        assert!(
            commands.frames.args.iter().any(|arg| arg == &os("fps=1")),
            "P0 frame sampling should default to fps=1"
        );
        assert!(
            commands
                .frames
                .args
                .contains(&temp.join("frame-%06d.png").into_os_string()),
            "frame target pattern should be under temp dir"
        );
    }

    #[test]
    fn command_plan_prefixes_relative_dash_paths_before_media_tools() {
        let input = PathBuf::from("-show_format.mp4");
        let temp = PathBuf::from("tmp");
        let commands = build_ffmpeg_plan(&input, &temp);
        let safe_input = PathBuf::from(".").join(&input).into_os_string();

        assert_eq!(
            commands.probe.args.last().expect("probe input arg"),
            &safe_input,
            "ffprobe input should not be parsed as an option"
        );
        assert!(
            commands.audio.args.iter().any(|arg| arg == &safe_input),
            "ffmpeg audio input should use the protected path arg"
        );
        assert!(
            commands.frames.args.iter().any(|arg| arg == &safe_input),
            "ffmpeg frame input should use the protected path arg"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_plan_preserves_non_utf8_path_bytes() {
        let input_os = std::ffi::OsString::from_vec(b"/tmp/demo-\xFF.mp4".to_vec());
        let input = PathBuf::from(&input_os);
        let temp = PathBuf::from("/tmp/cairn-recording");
        let commands = build_ffmpeg_plan(&input, &temp);

        assert!(
            commands
                .probe
                .args
                .iter()
                .any(|arg| arg.as_os_str().as_bytes() == input_os.as_bytes()),
            "probe argv should preserve non-UTF8 input bytes"
        );
        assert!(
            commands
                .audio
                .args
                .iter()
                .any(|arg| arg.as_os_str().as_bytes() == input_os.as_bytes()),
            "audio argv should preserve non-UTF8 input bytes"
        );
        assert!(
            commands
                .frames
                .args
                .iter()
                .any(|arg| arg.as_os_str().as_bytes() == input_os.as_bytes()),
            "frame argv should preserve non-UTF8 input bytes"
        );
    }

    #[test]
    fn ffprobe_json_extracts_duration_size_and_stream_presence() {
        let raw = r#"{
          "format": {"duration": "5.200000", "size": "1234"},
          "streams": [
            {"codec_type": "audio"},
            {"codec_type": "video"}
          ]
        }"#;
        let meta = parse_ffprobe_json(raw).expect("metadata parses");

        assert_eq!(meta.duration_ms, 5200);
        assert_eq!(meta.file_size, 1234);
        assert!(meta.has_audio);
        assert!(meta.has_video);
    }

    #[test]
    fn ffprobe_duration_rounds_to_nearest_millisecond() {
        for (duration, expected_ms) in [
            ("1.9994", 1999),
            ("1.9995", 2000),
            ("0.0004", 0),
            ("0.0005", 1),
            ("5.200000", 5200),
        ] {
            let raw = format!(
                r#"{{
                  "format": {{"duration": "{duration}", "size": "1234"}},
                  "streams": [{{"codec_type": "audio"}}]
                }}"#
            );
            let meta = parse_ffprobe_json(&raw).expect("metadata parses");

            assert_eq!(
                meta.duration_ms, expected_ms,
                "unexpected duration_ms for {duration}"
            );
        }
    }

    #[test]
    fn ffprobe_duration_overflow_is_rejected() {
        let raw = format!(
            r#"{{
              "format": {{"duration": "{}.9995", "size": "1234"}},
              "streams": [{{"codec_type": "audio"}}]
            }}"#,
            u64::MAX / 1000
        );
        let err = parse_ffprobe_json(&raw).expect_err("duration should overflow u64 millis");
        let message = format!("{err:#}");

        assert!(
            message.contains("duration milliseconds overflow"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn command_helper_reports_missing_command() {
        let plan = CommandPlan {
            program: os("cairn-definitely-missing-command-task-9"),
            args: Vec::new(),
        };
        let err = run_command_capture_stdout(&plan).expect_err("missing command should fail");
        let message = format!("{err:#}");

        assert!(
            message.contains("failed to run cairn-definitely-missing-command-task-9"),
            "unexpected error: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_helper_captures_utf8_stdout() {
        let plan = CommandPlan {
            program: os("/bin/sh"),
            args: vec![os("-c"), os("printf 'recording ok'")],
        };

        let stdout = run_command_capture_stdout(&plan).expect("stdout captures");

        assert_eq!(stdout, "recording ok");
    }

    #[cfg(unix)]
    #[test]
    fn command_helper_reports_nonzero_status_with_stderr() {
        let plan = CommandPlan {
            program: os("/bin/sh"),
            args: vec![os("-c"), os("printf 'planned failure' >&2; exit 7")],
        };
        let err = run_command_no_stdout(&plan).expect_err("nonzero exit should fail");
        let message = format!("{err:#}");

        assert!(
            message.contains("/bin/sh failed with status Some(7): planned failure"),
            "unexpected error: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_helper_reports_non_utf8_stdout() {
        let plan = CommandPlan {
            program: os("/bin/sh"),
            args: vec![os("-c"), os("printf '\\377'")],
        };
        let err = run_command_capture_stdout(&plan).expect_err("non-UTF8 stdout should fail");
        let message = format!("{err:#}");

        assert!(
            message.contains("/bin/sh emitted non-UTF8 stdout"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn ocr_outputs_become_deduped_frame_segments() {
        let frames = vec![
            FrameOcrObservation {
                timestamp_ms: 1000,
                duration_ms: 1000,
                confidence: 1.0,
                text: " screen shows gamma config ".to_owned(),
            },
            FrameOcrObservation {
                timestamp_ms: 2000,
                duration_ms: 1000,
                confidence: 0.9,
                text: "screen shows gamma config".to_owned(),
            },
            FrameOcrObservation {
                timestamp_ms: 3000,
                duration_ms: 1000,
                confidence: 1.0,
                text: String::new(),
            },
            FrameOcrObservation {
                timestamp_ms: 4000,
                duration_ms: 1000,
                confidence: 0.8,
                text: "delta dashboard".to_owned(),
            },
        ];

        let (segments, skipped) = frame_observations_to_segments(frames);

        assert_eq!(skipped, 2);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["screen shows gamma config", "delta dashboard"]
        );
        assert_eq!(
            segments.iter().map(|s| s.start_ms).collect::<Vec<_>>(),
            vec![1000, 4000]
        );
        match &segments[0].kind {
            SegmentKind::FrameOcr { confidence } => {
                assert!((*confidence - 1.0).abs() < f32::EPSILON);
            }
            SegmentKind::AudioTranscript { .. } => panic!("expected frame segment"),
        }
    }

    #[test]
    fn frame_observation_conversion_skips_empty_and_zero_duration_frames() {
        let frames = vec![
            FrameOcrObservation {
                timestamp_ms: 1000,
                duration_ms: 0,
                confidence: 0.7,
                text: "zero duration".to_owned(),
            },
            FrameOcrObservation {
                timestamp_ms: 2000,
                duration_ms: 1000,
                confidence: 0.6,
                text: " \n\t ".to_owned(),
            },
            FrameOcrObservation {
                timestamp_ms: 3000,
                duration_ms: 500,
                confidence: 0.8,
                text: "kept".to_owned(),
            },
        ];

        let (segments, skipped) = frame_observations_to_segments(frames);

        assert_eq!(skipped, 2);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "kept");
        assert_eq!(segments[0].duration_ms, 500);
    }

    #[test]
    fn frame_observation_conversion_preserves_non_adjacent_repeats() {
        let frames = vec![
            FrameOcrObservation {
                timestamp_ms: 1000,
                duration_ms: 1000,
                confidence: 0.8,
                text: "same".to_owned(),
            },
            FrameOcrObservation {
                timestamp_ms: 2000,
                duration_ms: 1000,
                confidence: 0.7,
                text: "different".to_owned(),
            },
            FrameOcrObservation {
                timestamp_ms: 3000,
                duration_ms: 1000,
                confidence: 0.6,
                text: " same ".to_owned(),
            },
        ];

        let (segments, skipped) = frame_observations_to_segments(frames);

        assert_eq!(skipped, 0);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["same", "different", "same"]
        );
    }

    #[test]
    fn frame_observation_conversion_sorts_chronologically_before_dedupe() {
        let frames = vec![
            FrameOcrObservation {
                timestamp_ms: 3000,
                duration_ms: 1000,
                confidence: 0.8,
                text: "later".to_owned(),
            },
            FrameOcrObservation {
                timestamp_ms: 1000,
                duration_ms: 1000,
                confidence: 0.7,
                text: "earlier".to_owned(),
            },
        ];

        let (segments, skipped) = frame_observations_to_segments(frames);

        assert_eq!(skipped, 0);
        assert_eq!(
            segments
                .iter()
                .map(|segment| (segment.start_ms, segment.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(1000, "earlier"), (3000, "later")]
        );
    }

    #[test]
    fn frame_timestamp_ms_from_fixed_pattern_name() {
        assert_eq!(
            frame_timestamp_ms_from_name(Path::new("/tmp/frames/frame-000001.png")),
            0
        );
        assert_eq!(
            frame_timestamp_ms_from_name(Path::new("/tmp/frames/frame-000000.png")),
            0
        );
        assert_eq!(
            frame_timestamp_ms_from_name(Path::new("/tmp/frames/frame-000042.png")),
            41_000
        );
    }

    #[test]
    fn malformed_frame_timestamp_names_default_to_zero() {
        for path in [
            Path::new("/tmp/frames/frame.png"),
            Path::new("/tmp/frames/frame-abc123.png"),
            Path::new("/tmp/frames/not-frame-000007.png"),
            Path::new("/tmp/frames/frame-000007.jpg"),
            Path::new("/tmp/frames/frame-00007.png"),
        ] {
            assert_eq!(
                frame_timestamp_ms_from_name(path),
                0,
                "malformed name should default to zero: {}",
                path.display()
            );
        }
    }

    #[test]
    fn tesseract_ocr_reports_missing_command_with_context() {
        let err = run_tesseract_ocr_with_program(
            Path::new("frame-000001.png"),
            OsString::from("cairn-definitely-missing-tesseract-ocr-task-11"),
        )
        .expect_err("missing tesseract command should fail");
        let message = format!("{err:#}");

        assert!(
            message.contains("tesseract OCR"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("failed to run cairn-definitely-missing-tesseract-ocr-task-11"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn ffprobe_json_reports_single_track_media() {
        let audio_only = parse_ffprobe_json(
            r#"{
              "format": {"duration": "1.999", "size": "42"},
              "streams": [{"codec_type": "audio"}]
            }"#,
        )
        .expect("audio-only metadata parses");
        assert_eq!(audio_only.duration_ms, 1999);
        assert!(audio_only.has_audio);
        assert!(!audio_only.has_video);

        let video_only = parse_ffprobe_json(
            r#"{
              "format": {"duration": "2.001", "size": "84"},
              "streams": [{"codec_type": "video"}]
            }"#,
        )
        .expect("video-only metadata parses");
        assert_eq!(video_only.duration_ms, 2001);
        assert!(!video_only.has_audio);
        assert!(video_only.has_video);
    }

    #[test]
    fn ffprobe_json_rejects_malformed_or_unusable_metadata() {
        for (raw, expected) in [
            ("not-json", "malformed ffprobe JSON"),
            (
                r#"{"format": {"size": "1234"}, "streams": [{"codec_type": "audio"}]}"#,
                "missing format.duration",
            ),
            (
                r#"{"format": {"duration": "abc", "size": "1234"}, "streams": [{"codec_type": "audio"}]}"#,
                "invalid format.duration",
            ),
            (
                r#"{"format": {"duration": "1.0"}, "streams": [{"codec_type": "audio"}]}"#,
                "missing format.size",
            ),
            (
                r#"{"format": {"duration": "1.0", "size": "large"}, "streams": [{"codec_type": "audio"}]}"#,
                "invalid format.size",
            ),
            (
                r#"{"format": {"duration": "1.0", "size": "10"}, "streams": []}"#,
                "contains no audio or video streams",
            ),
        ] {
            let err = parse_ffprobe_json(raw).expect_err("metadata should be rejected");
            let message = format!("{err:#}");
            assert!(
                message.contains(expected),
                "expected `{expected}` in error, got: {message}"
            );
        }
    }

    #[test]
    fn fixture_parser_orders_audio_and_deduped_ocr_segments() {
        let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");

        assert_eq!(
            plan.media_hash,
            "sha256:0000000000000000000000000000000000000000000000000000000000000087"
        );
        assert_eq!(plan.skipped_frames, 2);
        assert_eq!(
            plan.segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "alpha recording launch note",
                "screen shows gamma config",
                "beta follow up action",
            ],
        );
        assert_eq!(
            plan.segments.iter().map(|s| s.start_ms).collect::<Vec<_>>(),
            vec![0, 2000, 3200],
        );
    }

    #[test]
    fn segment_payloads_are_deterministic_and_body_safe() {
        let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");
        let segment = &plan.segments[0];

        let payload = build_segment_payload(&plan, segment).expect("payload builds");
        let again = build_segment_payload(&plan, segment).expect("payload builds twice");

        assert_eq!(payload.segment_id, again.segment_id);
        assert!(payload.segment_id.starts_with("recseg-"));
        assert_eq!(payload.segment_id.len(), "recseg-".len() + 24);
        let segment_id_suffix = payload
            .segment_id
            .strip_prefix("recseg-")
            .expect("recseg prefix");
        assert!(
            segment_id_suffix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "segment id suffix must be lowercase hex"
        );
        assert_eq!(payload.payload_hash, again.payload_hash);
        assert!(payload.payload_hash.starts_with("sha256:"));
        assert_eq!(payload.payload_hash.len(), "sha256:".len() + 64);
        assert!(
            payload.payload_hash["sha256:".len()..]
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "payload hash must be lowercase hex"
        );
        assert_eq!(
            payload.payload_hash,
            format!("sha256:{:x}", Sha256::digest(&payload.payload_json))
        );
        assert_eq!(payload.payload_json, again.payload_json);
        assert_eq!(payload.segment_id, "recseg-af067ba975f4fc04c20d1743");
        assert_eq!(
            payload.payload_json.as_slice(),
            br#"{"media":{"duration_ms":5200,"file_size":1234,"path":"fixtures/v0/recordings/demo.mp4","sha256":"sha256:0000000000000000000000000000000000000000000000000000000000000087"},"segment":{"detail":{"confidence":0.9100000262260437,"speaker_id":"unknown_speaker_01"},"duration_ms":1800,"id":"recseg-af067ba975f4fc04c20d1743","start_ms":0,"text":"alpha recording launch note","track_kind":"audio_transcript"},"tools":{"ffmpeg":"fixture","ocr":"fixture"}}"#
        );
        assert_eq!(
            payload.payload_hash,
            "sha256:4c23dd7dcc78517f84748fb146eca9cae2059ef67f93579e9f383c24ecd79edf"
        );

        let value: serde_json::Value =
            serde_json::from_slice(&payload.payload_json).expect("payload JSON");
        assert_eq!(value["media"]["sha256"], plan.media_hash);
        assert_eq!(value["segment"]["text"], "alpha recording launch note");
        assert!(
            value["media"].get("copied_path").is_none(),
            "payload must not imply media was copied"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "single fixture assertion keeps the recording event, payload, and source-link contract together"
    )]
    fn recording_plan_builds_valid_capture_events_and_payload_refs() {
        let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");
        let batch = build_capture_batch(&plan).expect("batch builds");

        assert_eq!(batch.events.len(), 3);
        assert_eq!(batch.payloads.len(), 3);
        assert_eq!(
            batch.events[0].source_family,
            cairn_core::domain::SourceFamily::RecordingBatch
        );
        assert_eq!(batch.events[0].capture_mode, CaptureMode::Explicit);
        assert_eq!(
            batch.events[0].sensor_id.as_str(),
            "snr:local:recording:default:v1"
        );
        assert!(
            batch.events[0]
                .payload_ref
                .starts_with("sources/recordings/")
        );
        assert_eq!(
            batch
                .events
                .iter()
                .map(|event| event.payload_ref.as_str())
                .collect::<Vec<_>>(),
            batch
                .payloads
                .iter()
                .map(|payload| payload.vault_relative_path.as_str())
                .collect::<Vec<_>>()
        );

        let again = build_capture_batch(&plan).expect("batch builds twice");
        assert_eq!(
            batch
                .events
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            again
                .events
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            "event ids must be deterministic"
        );
        assert_eq!(
            batch
                .events
                .iter()
                .map(|event| event.payload_ref.as_str())
                .collect::<Vec<_>>(),
            vec![
                format!(
                    "sources/recordings/0000000000000000000000000000000000000000000000000000000000000087/{}.json",
                    batch.events[0].event_id
                ),
                format!(
                    "sources/recordings/0000000000000000000000000000000000000000000000000000000000000087/{}.json",
                    batch.events[1].event_id
                ),
                format!(
                    "sources/recordings/0000000000000000000000000000000000000000000000000000000000000087/{}.json",
                    batch.events[2].event_id
                ),
            ],
            "payload refs should follow segment order and stable event ids"
        );

        let expected_boundaries = [(0, 1800), (2000, 1000), (3200, 900)];
        let expected_text = [
            "alpha recording launch note",
            "screen shows gamma config",
            "beta follow up action",
        ];
        for ((event, staged), &(start_ms, duration_ms)) in batch
            .events
            .iter()
            .zip(&batch.payloads)
            .zip(&expected_boundaries)
        {
            assert_eq!(
                event.actor_chain[0].role,
                ChainRole::Author,
                "explicit recording event should be human-authored"
            );
            assert_eq!(
                event.actor_chain[0].identity.as_str(),
                "hmn:recording-ingest"
            );
            assert_eq!(event.actor_chain[0].at, event.captured_at);
            assert_eq!(event.actor_chain.len(), 1);
            let payload_stem = staged
                .vault_relative_path
                .rsplit('/')
                .next()
                .and_then(|name| name.strip_suffix(".json"))
                .expect("payload filename stem");
            assert_eq!(payload_stem, event.event_id.as_str());
            SourceId::parse(payload_stem).expect("payload filename stem is valid source id");
            assert_eq!(
                event.payload,
                CapturePayload::RecordingBatch {
                    segment_start_ms: start_ms,
                    segment_duration_ms: duration_ms,
                }
            );
            assert_eq!(
                event.payload_hash.as_str(),
                format!("sha256:{:x}", Sha256::digest(&staged.bytes))
            );
            event.validate_for_capture().expect("recording event valid");
        }

        let mut sorted = batch.events.clone();
        sorted.sort_by(|left, right| {
            left.captured_at
                .cmp_chronological(&right.captured_at)
                .then_with(|| left.event_id.as_str().cmp(right.event_id.as_str()))
        });
        assert_eq!(
            sorted
                .iter()
                .zip(&batch.payloads)
                .map(|(event, payload)| {
                    assert_eq!(event.payload_ref, payload.vault_relative_path);
                    let value: serde_json::Value =
                        serde_json::from_slice(&payload.bytes).expect("payload JSON");
                    value["segment"]["text"].as_str().expect("text").to_owned()
                })
                .collect::<Vec<_>>(),
            expected_text
        );

        let mut duplicate_plan = plan;
        duplicate_plan.segments[1] = duplicate_plan.segments[0].clone();
        let err = build_capture_batch(&duplicate_plan)
            .expect_err("duplicate segment ids should reject batch construction");
        assert!(
            err.to_string().contains("duplicate recording segment_id"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn same_timestamp_segments_keep_plan_order_after_capture_trace_sort() {
        let plan = RecordingPlan {
            media_path: PathBuf::from("fixtures/v0/recordings/demo.mp4"),
            media_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000087"
                .to_owned(),
            duration_ms: 5200,
            file_size: 1234,
            skipped_frames: 0,
            segments: vec![
                RecordingSegment {
                    start_ms: 1000,
                    duration_ms: 500,
                    text: "audio first".to_owned(),
                    kind: SegmentKind::AudioTranscript {
                        speaker_id: "speaker_01".to_owned(),
                        confidence: 0.91,
                    },
                },
                RecordingSegment {
                    start_ms: 1000,
                    duration_ms: 1000,
                    text: "frame second".to_owned(),
                    kind: SegmentKind::FrameOcr { confidence: 0.82 },
                },
            ],
        };
        let batch = build_capture_batch(&plan).expect("batch builds");

        let mut sorted = batch.events.clone();
        sorted.sort_by(|left, right| {
            left.captured_at
                .cmp_chronological(&right.captured_at)
                .then_with(|| left.event_id.as_str().cmp(right.event_id.as_str()))
        });

        assert_eq!(
            sorted
                .iter()
                .map(|event| match event.payload {
                    CapturePayload::RecordingBatch {
                        segment_start_ms,
                        segment_duration_ms,
                    } => (segment_start_ms, segment_duration_ms),
                    _ => panic!("expected recording payload"),
                })
                .collect::<Vec<_>>(),
            vec![(1000, 500), (1000, 1000)],
            "ordinal timestamp offset must preserve plan order for tied start_ms"
        );
        assert_eq!(
            batch
                .events
                .iter()
                .map(|event| event.captured_at.as_str())
                .collect::<Vec<_>>(),
            vec!["2026-05-13T00:00:01.000000Z", "2026-05-13T00:00:01.000001Z"]
        );
    }

    #[test]
    fn duplicate_segment_ids_are_rejected_before_payloads_are_staged() {
        let plan = RecordingPlan {
            media_path: PathBuf::from("fixtures/v0/recordings/demo.mp4"),
            media_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000087"
                .to_owned(),
            duration_ms: 5200,
            file_size: 1234,
            skipped_frames: 0,
            segments: vec![
                RecordingSegment {
                    start_ms: 1000,
                    duration_ms: 500,
                    text: "Alpha  Recording".to_owned(),
                    kind: SegmentKind::AudioTranscript {
                        speaker_id: "speaker_01".to_owned(),
                        confidence: 0.91,
                    },
                },
                RecordingSegment {
                    start_ms: 1000,
                    duration_ms: 500,
                    text: " alpha recording ".to_owned(),
                    kind: SegmentKind::AudioTranscript {
                        speaker_id: "speaker_02".to_owned(),
                        confidence: 0.72,
                    },
                },
            ],
        };

        let err = build_segment_payloads(&plan).expect_err("duplicate segment ids are rejected");
        let message = err.to_string();
        assert!(
            message.contains("duplicate recording segment_id"),
            "unexpected error: {message}"
        );
        assert!(message.contains("recseg-"), "unexpected error: {message}");
    }

    #[test]
    fn payload_write_failure_keeps_existing_payload_and_removes_new_files() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let vault = tempfile::tempdir().expect("temp vault");
            let existing_rel = "sources/recordings/hash/existing.json";
            let created_rel = "sources/recordings/hash/created.json";
            let failing_rel = "sources/recordings/hash/failing.json";
            let existing_path = vault.path().join(existing_rel);
            std::fs::create_dir_all(existing_path.parent().expect("existing parent"))
                .expect("create existing parent");
            std::fs::write(&existing_path, b"existing bytes").expect("write existing");
            std::fs::create_dir_all(vault.path().join(failing_rel))
                .expect("create directory at failing payload path");

            let payloads = vec![
                StagedPayload {
                    vault_relative_path: existing_rel.to_owned(),
                    bytes: b"existing bytes".to_vec(),
                },
                StagedPayload {
                    vault_relative_path: created_rel.to_owned(),
                    bytes: b"created bytes".to_vec(),
                },
                StagedPayload {
                    vault_relative_path: failing_rel.to_owned(),
                    bytes: b"new bytes".to_vec(),
                },
            ];

            let err = write_payloads(vault.path(), &payloads)
                .await
                .expect_err("directory target should fail payload write");
            assert!(
                err.to_string().contains("payload path is not a file"),
                "unexpected error: {err:#}"
            );
            assert_eq!(
                std::fs::read(&existing_path).expect("read existing"),
                b"existing bytes"
            );
            assert!(
                !vault.path().join(created_rel).exists(),
                "newly created payload should be removed after failure"
            );
            assert!(
                vault.path().join(failing_rel).is_dir(),
                "pre-existing directory at failed path should not be removed"
            );
        });
    }

    #[test]
    fn frame_payload_uses_segment_start_as_timestamp_and_confidence_detail() {
        let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");
        let segment = plan
            .segments
            .iter()
            .find(|segment| matches!(segment.kind, SegmentKind::FrameOcr { .. }))
            .expect("frame segment");

        let payload = build_segment_payload(&plan, segment).expect("payload builds");
        let value: serde_json::Value =
            serde_json::from_slice(&payload.payload_json).expect("payload JSON");

        assert_eq!(value["segment"]["track_kind"], "frame_ocr");
        assert_eq!(value["segment"]["start_ms"], 2000);
        assert_eq!(value["segment"]["text"], "screen shows gamma config");
        let confidence = value["segment"]["detail"]["confidence"]
            .as_f64()
            .expect("confidence is numeric");
        assert!((confidence - 0.82).abs() < 0.000_001);
        assert_eq!(value["segment"]["detail"]["timestamp_ms"], 2000);
    }

    #[test]
    fn frame_dedupe_is_chronological_for_unsorted_fixtures() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "frames": [
                {"timestamp_ms": 3000, "duration_ms": 1000, "confidence": 0.70, "text": "same screen"},
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.90, "text": "same screen"}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(plan.skipped_frames, 1);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].start_ms, 1000);
        assert_eq!(plan.segments[0].text, "same screen");
    }

    #[test]
    fn invalid_media_sha256_is_rejected() {
        for media_sha256 in [
            "sha256:000000000000000000000000000000000000000000000000000000000000008",
            "sha256:000000000000000000000000000000000000000000000000000000000000008G",
            "0000000000000000000000000000000000000000000000000000000000000087",
        ] {
            let raw = format!(
                r#"{{
                  "media_path": "fixtures/v0/recordings/demo.mp4",
                  "media_sha256": "{media_sha256}",
                  "duration_ms": 5200,
                  "file_size": 1234
                }}"#
            );

            assert!(
                parse_fixture_plan(&raw).is_err(),
                "accepted invalid media_sha256: {media_sha256}"
            );
        }
    }

    #[test]
    fn unknown_fixture_fields_are_rejected() {
        let result = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "unexpected": true
            }"#,
        );

        assert!(result.is_err(), "accepted unknown fixture field");
    }

    #[test]
    fn whitespace_normalized_adjacent_frames_are_deduped() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "frames": [
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.82, "text": "screen   shows\nconfig"},
                {"timestamp_ms": 2000, "duration_ms": 1000, "confidence": 0.80, "text": " screen shows config "}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(plan.skipped_frames, 1);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].text, "screen shows config");
    }

    #[test]
    fn audio_sorts_before_frame_at_the_same_timestamp() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "audio": [
                {"start_ms": 1000, "duration_ms": 500, "speaker_id": "speaker_01", "confidence": 0.91, "text": "audio text"}
              ],
              "frames": [
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.82, "text": "frame text"}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(
            plan.segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["audio text", "frame text"],
        );
        assert!(matches!(
            plan.segments[0].kind,
            SegmentKind::AudioTranscript { .. }
        ));
        assert!(matches!(
            plan.segments[1].kind,
            SegmentKind::FrameOcr { .. }
        ));
    }

    #[test]
    fn empty_or_zero_duration_entries_are_skipped() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "audio": [
                {"start_ms": 0, "duration_ms": 0, "speaker_id": "speaker_01", "confidence": 0.91, "text": "zero audio"},
                {"start_ms": 500, "duration_ms": 500, "speaker_id": "speaker_02", "confidence": 0.80, "text": "   "},
                {"start_ms": 1000, "duration_ms": 500, "speaker_id": "speaker_03", "confidence": 0.70, "text": "kept audio"}
              ],
              "frames": [
                {"timestamp_ms": 1500, "duration_ms": 0, "confidence": 0.82, "text": "zero frame"},
                {"timestamp_ms": 2000, "duration_ms": 1000, "confidence": 0.80, "text": "   "},
                {"timestamp_ms": 2500, "duration_ms": 1000, "confidence": 0.70, "text": "kept frame"}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(plan.skipped_frames, 2);
        assert_eq!(
            plan.segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["kept audio", "kept frame"],
        );
    }

    #[test]
    fn kind_specific_fields_are_preserved() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "audio": [
                {"start_ms": 0, "duration_ms": 500, "speaker_id": "speaker_99", "confidence": 0.91, "text": "audio text"}
              ],
              "frames": [
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.82, "text": "frame text"}
              ]
            }"#,
        )
        .expect("fixture parses");

        match &plan.segments[0].kind {
            SegmentKind::AudioTranscript {
                speaker_id,
                confidence,
            } => {
                assert_eq!(speaker_id, "speaker_99");
                assert!((*confidence - 0.91).abs() < f32::EPSILON);
            }
            SegmentKind::FrameOcr { .. } => panic!("expected audio segment"),
        }
        match &plan.segments[1].kind {
            SegmentKind::FrameOcr { confidence } => {
                assert!((*confidence - 0.82).abs() < f32::EPSILON);
            }
            SegmentKind::AudioTranscript { .. } => panic!("expected frame segment"),
        }
    }
}
