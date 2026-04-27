//! `CaptureEvent` schema — the unified envelope every sensor emits into the
//! ingestion pipeline (brief §5.0.a, §9).
//!
//! Every source — hooks, IDE, terminal, clipboard, voice, screen, batch
//! recordings, the explicit `cairn ingest` CLI, and proactive agent
//! emissions — produces the same `CaptureEvent` shape. The `source_family`
//! discriminator selects which `CapturePayload` variant carries the
//! modality-specific extraction hint; the rest of the envelope is uniform
//! so the pipeline (Capture → Tool-squash → Extract → Filter → Classify →
//! Scope → Store) can route, deduplicate, and attribute every event with
//! the same machinery.
//!
//! ## Scope of this module
//!
//! - **Schema only.** Extractor implementations (regex/llm/agent) live
//!   downstream of this module. We define the wire and in-memory shape, the
//!   newtypes that enforce per-field invariants, and the
//!   [`CaptureEvent::validate`] entry point that rejects malformed events
//!   before they enter the pipeline.
//! - **Sensor manifest validation** lives in
//!   [`super::capture_manifest`]; **mode → author attribution** lives in
//!   [`super::capture_attribution`]. Both are re-exported from the crate
//!   root for convenience.

use serde::{Deserialize, Serialize};

use crate::domain::{
    ActorChainEntry, ChainRole, DomainError, Identity, IdentityKind, Rfc3339Timestamp,
};

/// ULID identifier for a single `CaptureEvent` (Crockford base32, 26 chars).
///
/// Matches the `Ulid` schema in `crates/cairn-idl/schema/common/primitives.json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CaptureEventId(String);

impl CaptureEventId {
    /// Parse a wire-form ULID.
    ///
    /// Length is 26, alphabet is Crockford base32 (uppercase, no
    /// `I L O U`), and the first character is bounded to `[0..=7]`
    /// because a ULID encodes a 128-bit integer and the high 5 bits of
    /// the leading Crockford symbol must be zero — otherwise the value
    /// overflows 128 bits and downstream ULID decoders will misorder or
    /// reject it. Mirrors [`crate::domain::record::RecordId::parse`].
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.len() != 26 {
            return Err(DomainError::MalformedCapture {
                message: format!("event_id `{raw}`: ULID must be exactly 26 chars"),
            });
        }
        let bytes = raw.as_bytes();
        if !matches!(bytes[0], b'0'..=b'7') {
            return Err(DomainError::MalformedCapture {
                message: format!(
                    "event_id `{raw}`: first char must be `0`-`7` (128-bit ULID range)"
                ),
            });
        }
        if !bytes[1..].iter().copied().all(is_crockford_base32) {
            return Err(DomainError::MalformedCapture {
                message: format!("event_id `{raw}`: non-Crockford-base32 character"),
            });
        }
        Ok(Self(raw))
    }

    /// ULID string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

const fn is_crockford_base32(b: u8) -> bool {
    matches!(b,
        b'0'..=b'9'
        | b'A'..=b'H'
        | b'J' | b'K' | b'M' | b'N'
        | b'P'..=b'T'
        | b'V'..=b'Z'
    )
}

impl std::fmt::Display for CaptureEventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CaptureEventId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// The body portion of a [`crate::domain::Identity`] of kind
/// [`IdentityKind::Sensor`] — i.e. `<body>` in `snr:<body>`. Used as the
/// key for manifest lookup (see [`super::capture_manifest`]).
///
/// Construction validates the shape (`[A-Za-z0-9._:-]+`, non-empty)
/// without re-validating the `snr:` prefix; convert from a full
/// [`Identity`] via [`SensorLabel::from_identity`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SensorLabel(String);

impl SensorLabel {
    /// Parse a bare label body (no `snr:` prefix). Returns
    /// [`DomainError::MalformedCapture`] if empty or containing
    /// out-of-class characters.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(DomainError::MalformedCapture {
                message: "sensor_label: empty".to_owned(),
            });
        }
        if !raw.bytes().all(|b| {
            matches!(b,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-')
        }) {
            return Err(DomainError::MalformedCapture {
                message: format!("sensor_label `{raw}`: chars must be in [A-Za-z0-9._:-]"),
            });
        }
        Ok(Self(raw))
    }

    /// Extract the label body from a sensor [`Identity`].
    pub fn from_identity(id: &Identity) -> Result<Self, DomainError> {
        if id.kind() != IdentityKind::Sensor {
            return Err(DomainError::MalformedCapture {
                message: format!(
                    "sensor_label: identity `{}` is not a sensor (`snr:`)",
                    id.as_str()
                ),
            });
        }
        let body =
            id.as_str()
                .strip_prefix("snr:")
                .ok_or_else(|| DomainError::MalformedCapture {
                    message: format!(
                        "sensor_label: identity `{}` missing `snr:` prefix",
                        id.as_str()
                    ),
                })?;
        Self::parse(body.to_owned())
    }

    /// Label body string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SensorLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SensorLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// SHA-256 hash of the raw payload bytes for a [`CaptureEvent`], formatted
/// as `sha256:<64 lowercase hex>`. Same shape as
/// [`crate::domain::CanonicalRecordHash`] and the `target_hash` of a
/// signed intent — keeps every hash slot in the pipeline using a single
/// canonical encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PayloadHash(String);

impl PayloadHash {
    /// Parse a `sha256:<64 lowercase hex>` string.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let hex = raw
            .strip_prefix("sha256:")
            .ok_or_else(|| DomainError::InvalidPayloadHash {
                message: format!("`{raw}`: missing `sha256:` prefix"),
            })?;
        if hex.len() != 64 {
            return Err(DomainError::InvalidPayloadHash {
                message: format!("`{raw}`: hex must be exactly 64 chars"),
            });
        }
        if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DomainError::InvalidPayloadHash {
                message: format!("`{raw}`: hex must be lowercase [0-9a-f]"),
            });
        }
        Ok(Self(raw))
    }

    /// Wire-form `sha256:<hex>` slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PayloadHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PayloadHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// One of the three concurrent capture modes (brief §5.0.a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CaptureMode {
    /// Mode A — sensor-driven hook fired automatically; agent uninvolved.
    /// Author of the resulting record is the sensor's [`Identity`].
    Auto,
    /// Mode B — user explicitly invoked the skill / `cairn ingest` (e.g.
    /// "remember that …"). Author is the human; the agent (if any) is the
    /// delegator.
    Explicit,
    /// Mode C — agent decided to record (novel entity, correction, success
    /// strategy). Author is the agent identity.
    Proactive,
}

impl std::fmt::Display for CaptureMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CaptureMode {
    /// Stable wire-form slice (`auto` / `explicit` / `proactive`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Explicit => "explicit",
            Self::Proactive => "proactive",
        }
    }

    /// Parse from the wire form. Returns
    /// [`DomainError::UnsupportedCaptureMode`] on miss.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        match raw {
            "auto" => Ok(Self::Auto),
            "explicit" => Ok(Self::Explicit),
            "proactive" => Ok(Self::Proactive),
            other => Err(DomainError::UnsupportedCaptureMode {
                value: other.to_owned(),
            }),
        }
    }
}

/// Source-family discriminator for [`CapturePayload`]. Each variant maps
/// 1:1 to a sensor or input channel listed in brief §9.1 (local) and
/// §9.1.a (recording batch). The CLI / MCP / proactive-agent variants
/// model the three §5.0.a entry points that aren't strictly "sensors" but
/// share the same envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceFamily {
    /// Hook sensor — `SessionStart`, `UserPromptSubmit`, `PostToolUse`,
    /// `PreCompact`, `Stop` events from a cooperating harness.
    Hook,
    /// IDE sensor — file edits, diagnostics, tests, LSP events.
    Ide,
    /// Terminal sensor — captured commands and outputs.
    Terminal,
    /// Clipboard sensor — clipboard snapshots.
    Clipboard,
    /// Voice sensor — VAD-gated utterances, ASR transcript, speaker
    /// embeddings.
    Voice,
    /// Screen sensor — frame OCR + active-window + URL events.
    Screen,
    /// Recording-batch — `cairn ingest --recording <file>` audio + video
    /// after-the-fact extraction (brief §9.1.a).
    RecordingBatch,
    /// Explicit `cairn ingest` invocation (CLI or skill-triggered, Mode B).
    Cli,
    /// Explicit `cairn ingest` invocation through the MCP adapter (Mode B).
    Mcp,
    /// Proactive agent emission (Mode C) — no external sensor channel.
    Proactive,
}

impl SourceFamily {
    /// Stable wire-form slice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Ide => "ide",
            Self::Terminal => "terminal",
            Self::Clipboard => "clipboard",
            Self::Voice => "voice",
            Self::Screen => "screen",
            Self::RecordingBatch => "recording_batch",
            Self::Cli => "cli",
            Self::Mcp => "mcp",
            Self::Proactive => "proactive",
        }
    }

    /// Parse from wire form. Returns
    /// [`DomainError::UnsupportedSourceFamily`] on miss.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        match raw {
            "hook" => Ok(Self::Hook),
            "ide" => Ok(Self::Ide),
            "terminal" => Ok(Self::Terminal),
            "clipboard" => Ok(Self::Clipboard),
            "voice" => Ok(Self::Voice),
            "screen" => Ok(Self::Screen),
            "recording_batch" => Ok(Self::RecordingBatch),
            "cli" => Ok(Self::Cli),
            "mcp" => Ok(Self::Mcp),
            "proactive" => Ok(Self::Proactive),
            other => Err(DomainError::UnsupportedSourceFamily {
                value: other.to_owned(),
            }),
        }
    }
}

impl std::fmt::Display for SourceFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Modality-specific payload metadata for a [`CaptureEvent`].
///
/// The variant **must agree** with the event's [`SourceFamily`] —
/// validation enforces this. Raw content (utterances, frames, clipboard
/// text) stays behind `payload_ref` + `payload_hash` rather than inline.
///
/// **Treat the envelope as sensitive, not log-safe.** Several variants
/// carry high-signal user metadata that must not be emitted to
/// structured logs at `info` or above (CLAUDE.md §6.6, brief §14):
/// `Terminal.command`, `Screen.window_title`, `Screen.url`,
/// `Ide.file_path`, `Voice.speaker_id`, `Proactive.rationale`. A
/// downstream observer needs a redacted projection of the envelope —
/// not a structured-log dump of the raw struct — before any of these
/// fields leave `trace`. A separate sanitized log type is out of scope
/// for this issue and tracked as a follow-up.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source_family", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum CapturePayload {
    /// Hook event — names the harness hook plus session/turn/tool refs.
    Hook {
        /// Harness hook name (e.g., `SessionStart`, `PostToolUse`).
        hook_name: String,
        /// Optional tool name, present on `PreToolUse` / `PostToolUse`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
    },
    /// IDE event — file path, event kind (edit/diagnostic/test/lsp).
    Ide {
        /// Workspace-relative path of the affected file.
        file_path: String,
        /// Event subtype (`edit`, `diagnostic`, `test`, `lsp`).
        event_kind: String,
    },
    /// Terminal command + output.
    Terminal {
        /// Argv-style command line.
        command: String,
        /// Process exit code, if the command had completed at capture
        /// time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    /// Clipboard snapshot.
    Clipboard {
        /// Clipboard MIME type (`text/plain`, `image/png`, …).
        mime_type: String,
        /// Length of the clipboard payload in bytes.
        byte_len: u64,
    },
    /// Voice utterance (post-VAD, post-ASR).
    Voice {
        /// Speaker label assigned by diarization
        /// (e.g., `unknown_speaker_<ulid>` until enrolled).
        speaker_id: String,
        /// Number of milliseconds the utterance spans.
        duration_ms: u64,
        /// ASR confidence in `[0.0, 1.0]`.
        confidence: f32,
    },
    /// Screen frame.
    Screen {
        /// Active application bundle / process name.
        app: String,
        /// Active window title (may be redacted at the sensor boundary).
        window_title: String,
        /// URL extracted from the active tab, if a browser was active.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Aligned segment from a batch recording (brief §9.1.a).
    RecordingBatch {
        /// Path of the source recording file (under `sources/`).
        recording_path: String,
        /// Segment start offset within the recording, in milliseconds.
        segment_start_ms: u64,
        /// Segment duration in milliseconds.
        segment_duration_ms: u64,
    },
    /// `cairn ingest` invocation from the CLI (Mode B).
    Cli {
        /// Memory taxonomy kind hint declared by the user (§5.0.a Mode B).
        kind_hint: String,
    },
    /// `cairn ingest` invocation through the MCP adapter (Mode B).
    Mcp {
        /// Memory taxonomy kind hint declared by the user.
        kind_hint: String,
    },
    /// Proactive agent emission (Mode C).
    Proactive {
        /// Memory taxonomy kind the agent decided to emit (e.g.,
        /// `entity`, `feedback`, `strategy_success`, `knowledge_gap`).
        kind: String,
        /// Short rationale string the agent attached to the emission.
        rationale: String,
    },
}

impl std::fmt::Debug for CapturePayload {
    /// Redacted `Debug`: prints the variant name plus only the
    /// non-sensitive scalar fields. Identifier strings, command lines,
    /// titles, URLs, and rationale text are replaced with `<redacted>`
    /// so accidental `tracing`/panic dumps cannot leak user metadata
    /// (CLAUDE.md §6.6, brief §14). Use `serde_json::to_string` (which
    /// preserves all fields) when a full dump is intentionally needed
    /// at `trace`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hook { .. } => f
                .debug_struct("CapturePayload::Hook")
                .finish_non_exhaustive(),
            Self::Ide { .. } => f
                .debug_struct("CapturePayload::Ide")
                .finish_non_exhaustive(),
            Self::Terminal { exit_code, .. } => f
                .debug_struct("CapturePayload::Terminal")
                .field("exit_code", exit_code)
                .finish_non_exhaustive(),
            Self::Clipboard {
                mime_type,
                byte_len,
            } => f
                .debug_struct("CapturePayload::Clipboard")
                .field("mime_type", mime_type)
                .field("byte_len", byte_len)
                .finish(),
            Self::Voice {
                duration_ms,
                confidence,
                ..
            } => f
                .debug_struct("CapturePayload::Voice")
                .field("duration_ms", duration_ms)
                .field("confidence", confidence)
                .finish_non_exhaustive(),
            Self::Screen { .. } => f
                .debug_struct("CapturePayload::Screen")
                .finish_non_exhaustive(),
            Self::RecordingBatch {
                segment_start_ms,
                segment_duration_ms,
                ..
            } => f
                .debug_struct("CapturePayload::RecordingBatch")
                .field("segment_start_ms", segment_start_ms)
                .field("segment_duration_ms", segment_duration_ms)
                .finish_non_exhaustive(),
            Self::Cli { .. } => f
                .debug_struct("CapturePayload::Cli")
                .finish_non_exhaustive(),
            Self::Mcp { .. } => f
                .debug_struct("CapturePayload::Mcp")
                .finish_non_exhaustive(),
            Self::Proactive { kind, .. } => f
                .debug_struct("CapturePayload::Proactive")
                .field("kind", kind)
                .finish_non_exhaustive(),
        }
    }
}

impl CapturePayload {
    /// The [`SourceFamily`] this payload belongs to.
    #[must_use]
    pub const fn source_family(&self) -> SourceFamily {
        match self {
            Self::Hook { .. } => SourceFamily::Hook,
            Self::Ide { .. } => SourceFamily::Ide,
            Self::Terminal { .. } => SourceFamily::Terminal,
            Self::Clipboard { .. } => SourceFamily::Clipboard,
            Self::Voice { .. } => SourceFamily::Voice,
            Self::Screen { .. } => SourceFamily::Screen,
            Self::RecordingBatch { .. } => SourceFamily::RecordingBatch,
            Self::Cli { .. } => SourceFamily::Cli,
            Self::Mcp { .. } => SourceFamily::Mcp,
            Self::Proactive { .. } => SourceFamily::Proactive,
        }
    }

    /// Per-variant invariants beyond `serde` shape: required identifier
    /// strings are non-empty, scalars are in their documented range.
    /// Returns the first failure as
    /// [`DomainError::MalformedCapture`] / [`DomainError::OutOfRange`] /
    /// [`DomainError::EmptyField`].
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Hook { hook_name, .. } => {
                require_non_empty("hook_name", hook_name)?;
            }
            Self::Ide {
                file_path,
                event_kind,
            } => {
                require_non_empty("file_path", file_path)?;
                require_non_empty("event_kind", event_kind)?;
            }
            Self::Terminal { command, .. } => {
                require_non_empty("command", command)?;
            }
            Self::Clipboard { mime_type, .. } => {
                require_non_empty("mime_type", mime_type)?;
            }
            Self::Voice {
                speaker_id,
                confidence,
                ..
            } => {
                require_non_empty("speaker_id", speaker_id)?;
                if !confidence.is_finite() || !(0.0..=1.0).contains(confidence) {
                    return Err(DomainError::OutOfRange {
                        field: "confidence",
                        message: format!("expected `[0.0, 1.0]`, got `{confidence}`"),
                    });
                }
            }
            Self::Screen { app, .. } => {
                // `window_title` is intentionally allowed to be empty —
                // privacy-redacted captures and titleless OS surfaces
                // (notifications, lock screens) submit `""` rather than
                // forcing the sensor to invent a placeholder.
                require_non_empty("app", app)?;
            }
            Self::RecordingBatch {
                recording_path,
                segment_duration_ms,
                ..
            } => {
                // Same trust boundary as `payload_ref` — a downstream
                // batch extractor reopens this file, so it must be a
                // vault-relative `sources/...` path.
                validate_vault_relative_path("recording_path", recording_path)?;
                if *segment_duration_ms == 0 {
                    return Err(DomainError::OutOfRange {
                        field: "segment_duration_ms",
                        message: "must be > 0".to_owned(),
                    });
                }
            }
            Self::Cli { kind_hint } | Self::Mcp { kind_hint } => {
                require_non_empty("kind_hint", kind_hint)?;
            }
            Self::Proactive { kind, rationale } => {
                require_non_empty("kind", kind)?;
                require_non_empty("rationale", rationale)?;
            }
        }
        Ok(())
    }
}

fn require_non_empty(field: &'static str, value: &str) -> Result<(), DomainError> {
    if value.is_empty() {
        Err(DomainError::EmptyField { field })
    } else {
        Ok(())
    }
}

/// Mode A: the chain `Author` identity must equal `sensor_id` — the
/// sensor authors its own raw events.
fn bind_auto_author(chain: &[ActorChainEntry], sensor_id: &Identity) -> Result<(), DomainError> {
    let author = chain
        .iter()
        .find(|e| e.role == ChainRole::Author)
        .ok_or_else(|| DomainError::MissingSignature {
            message: "actor_chain has no `author` entry".to_owned(),
        })?;
    if &author.identity != sensor_id {
        return Err(DomainError::AttributionMismatch {
            message: format!(
                "mode `auto` requires author identity `{}` to equal sensor_id `{}`",
                author.identity.as_str(),
                sensor_id.as_str()
            ),
        });
    }
    Ok(())
}

/// Mode C: the proactive sensor label carries the agent slug
/// (`local:proactive:<agent>:v<N>`); the chain author must be an
/// `agt:<agent>:...` whose slug matches. Blocks cross-agent spoofing.
fn bind_proactive_author(
    chain: &[ActorChainEntry],
    sensor_id: &Identity,
    label: &str,
) -> Result<(), DomainError> {
    let author = chain
        .iter()
        .find(|e| e.role == ChainRole::Author)
        .ok_or_else(|| DomainError::MissingSignature {
            message: "actor_chain has no `author` entry".to_owned(),
        })?;
    let sensor_agent =
        sensor_label_proactive_agent(label).ok_or_else(|| DomainError::MalformedCapture {
            message: format!(
                "proactive sensor_id `{}` does not encode an agent slug",
                sensor_id.as_str()
            ),
        })?;
    let author_agent = identity_agent_slug(author.identity.as_str()).ok_or_else(|| {
        DomainError::AttributionMismatch {
            message: format!(
                "proactive author `{}` is not an `agt:<slug>:...` identity",
                author.identity.as_str()
            ),
        }
    })?;
    if sensor_agent != author_agent {
        return Err(DomainError::AttributionMismatch {
            message: format!(
                "proactive sensor agent `{sensor_agent}` does not match author agent \
                 `{author_agent}`"
            ),
        });
    }
    Ok(())
}

/// Any `Sensor`-role entry in the chain must equal `sensor_id`.
fn bind_chain_sensor_entries(
    chain: &[ActorChainEntry],
    sensor_id: &Identity,
) -> Result<(), DomainError> {
    for entry in chain {
        if entry.role == ChainRole::Sensor && &entry.identity != sensor_id {
            return Err(DomainError::AttributionMismatch {
                message: format!(
                    "actor_chain `sensor` entry `{}` does not match sensor_id `{}`",
                    entry.identity.as_str(),
                    sensor_id.as_str()
                ),
            });
        }
    }
    Ok(())
}

/// Extract the `<agent>` slug from a `local:proactive:<agent>:v<N>`
/// sensor label. Returns `None` for non-proactive labels.
fn sensor_label_proactive_agent(label: &str) -> Option<&str> {
    let rest = label.strip_prefix("local:proactive:")?;
    let (agent, _) = rest.split_once(':')?;
    if agent.is_empty() { None } else { Some(agent) }
}

/// Extract the `<slug>` from an `agt:<slug>:...` identity. Returns
/// `None` if the identity is not an agent identity. The slug is the
/// vendor / product key that pairs with proactive sensor labels — for
/// `agt:claude-code:opus-4-7:main:v1` the slug is `claude-code`.
fn identity_agent_slug(identity: &str) -> Option<&str> {
    let rest = identity.strip_prefix("agt:")?;
    let slug = rest.split(':').next()?;
    if slug.is_empty() { None } else { Some(slug) }
}

/// Optional turn / tool / session references that pin a capture event into
/// the per-session timeline. Absent when the source is a one-off batch
/// (clipboard, recording) with no live session context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRefs {
    /// Harness session id, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Per-session turn id, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Per-turn tool-call id, if the event was emitted under one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
}

/// The unified capture envelope.
///
/// Every sensor (or the explicit Mode B / Mode C entry points) emits this
/// shape into the ingestion pipeline. The envelope is signable as a unit:
/// the `event_id` is the natural primary key, the `payload_hash` binds it
/// to the raw bytes stored under `sources/`, and the `actor_chain` carries
/// the §4.2 attribution that downstream pipeline stages check against
/// [`super::capture_attribution::attribute`].
/// Wire-form mirror of [`CaptureEvent`] used as the deserialization
/// target. `serde(try_from = "CaptureEventRaw")` on `CaptureEvent`
/// routes every JSON payload through this struct first, then runs
/// [`CaptureEvent::validate`] before yielding the typed envelope. That
/// makes invalid states unconstructable at the trust boundary —
/// callers cannot accidentally skip validation by going through
/// `serde_json::from_str` (or any other format that uses serde).
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureEventRaw {
    event_id: CaptureEventId,
    sensor_id: Identity,
    capture_mode: CaptureMode,
    actor_chain: Vec<ActorChainEntry>,
    #[serde(default)]
    refs: Option<CaptureRefs>,
    payload_hash: PayloadHash,
    payload_ref: String,
    captured_at: Rfc3339Timestamp,
    payload: CapturePayload,
    source_family: SourceFamily,
}

impl TryFrom<CaptureEventRaw> for CaptureEvent {
    type Error = DomainError;

    fn try_from(raw: CaptureEventRaw) -> Result<Self, Self::Error> {
        let event = Self {
            event_id: raw.event_id,
            sensor_id: raw.sensor_id,
            capture_mode: raw.capture_mode,
            actor_chain: raw.actor_chain,
            refs: raw.refs,
            payload_hash: raw.payload_hash,
            payload_ref: raw.payload_ref,
            captured_at: raw.captured_at,
            payload: raw.payload,
            source_family: raw.source_family,
        };
        event.validate()?;
        Ok(event)
    }
}

/// The unified capture envelope (brief §5.0.a, §9). See module-level
/// docs for the full validation contract — including the §5.0.a
/// attribution rule, the closed source-family list, and the trust
/// boundary on `payload_ref`. JSON deserialization runs
/// [`CaptureEvent::validate`] via `serde(try_from = "CaptureEventRaw")`,
/// so malformed events cannot enter the type at the wire boundary.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "CaptureEventRaw")]
pub struct CaptureEvent {
    /// ULID identifying this event.
    pub event_id: CaptureEventId,
    /// Sensor identity that produced the event. For Mode B / Mode C
    /// entries (CLI, MCP, proactive) the originating surface still has a
    /// sensor identity (`snr:local:cli:*`, `snr:local:mcp:*`,
    /// `snr:local:proactive:<agent>:v1`) so attribution stays uniform.
    pub sensor_id: Identity,
    /// One of the §5.0.a capture modes.
    pub capture_mode: CaptureMode,
    /// Author + delegator + sensor entries; validated against
    /// [`super::validate_chain`] and [`super::attribute`].
    pub actor_chain: Vec<ActorChainEntry>,
    /// Optional session / turn / tool references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<CaptureRefs>,
    /// SHA-256 of the raw payload bytes referenced by `payload_ref`.
    pub payload_hash: PayloadHash,
    /// Vault-relative path of the raw bytes, beginning with `sources/`
    /// (brief §3). Storing a relative path — rather than an absolute
    /// `file://` URI — keeps the trust boundary tight: a downstream
    /// resolver joins this against the configured vault root, so the
    /// envelope alone cannot point outside the managed vault.
    pub payload_ref: String,
    /// Wall-clock instant the sensor observed the event.
    pub captured_at: Rfc3339Timestamp,
    /// Modality-specific payload metadata. Its tag must equal
    /// `source_family` below.
    pub payload: CapturePayload,
    /// Source-family discriminator. Stored alongside `payload` so a
    /// reader that only inspects the envelope (without parsing the
    /// payload) can still route the event.
    pub source_family: SourceFamily,
}

impl std::fmt::Debug for CaptureEvent {
    /// Redacted `Debug`: prints structural metadata
    /// (`event_id`, `sensor_id`, `capture_mode`, `source_family`,
    /// `captured_at`, `payload_hash`, `payload_ref`) plus the redacted
    /// payload from [`CapturePayload`'s `Debug`]. Keeps `tracing`/panic
    /// dumps free of user content. Use serde-JSON when a full dump is
    /// intentionally needed at `trace`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureEvent")
            .field("event_id", &self.event_id)
            .field("sensor_id", &self.sensor_id)
            .field("capture_mode", &self.capture_mode)
            .field("source_family", &self.source_family)
            .field("captured_at", &self.captured_at)
            .field("payload_hash", &self.payload_hash)
            .field("payload_ref", &self.payload_ref)
            .field("payload", &self.payload)
            .finish_non_exhaustive()
    }
}

impl CaptureEvent {
    /// Run every invariant on the event:
    ///
    /// 1. `payload_ref` is a non-empty vault-relative path beginning
    ///    with `sources/`, with no `..` segments, no leading `/`, no
    ///    NUL bytes, and no scheme/authority — the resolver joins it
    ///    against the configured vault root.
    /// 2. `sensor_id` is a `snr:` identity (Mode B / C still flow through
    ///    a sensor surface).
    /// 3. `payload.source_family() == self.source_family`.
    /// 4. Per-payload invariants
    ///    ([`CapturePayload::validate`]) — required identifiers
    ///    non-empty, `confidence` in `[0.0, 1.0]`, etc.
    /// 5. `capture_mode` and `source_family` are a declared §5.0.a pair
    ///    (Auto = sensor families, Explicit = `cli` / `mcp`,
    ///    Proactive = `proactive`).
    /// 6. `source_family` matches the family expected from the sensor
    ///    label — a `local:cli:` sensor cannot emit a `screen` payload.
    /// 7. `actor_chain` validates per §4.2
    ///    ([`super::actor_chain::validate_chain`]).
    /// 8. `actor_chain` author matches `capture_mode` per §5.0.a
    ///    ([`super::capture_attribution::attribute`]).
    /// 9. For Mode A captures, the `Author` entry's identity equals
    ///    `sensor_id` (the sensor authors its own raw events). For any
    ///    mode, if a `Sensor` role entry is present in the chain, it
    ///    must equal `sensor_id`.
    /// 10. Sensor label is in the declared P0 manifest
    ///     ([`super::capture_manifest::validate_label`]).
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_payload_ref(&self.payload_ref)?;

        if self.sensor_id.kind() != IdentityKind::Sensor {
            return Err(DomainError::MalformedCapture {
                message: format!(
                    "sensor_id `{}` is not a sensor identity (`snr:` prefix)",
                    self.sensor_id.as_str()
                ),
            });
        }
        if self.payload.source_family() != self.source_family {
            return Err(DomainError::MalformedCapture {
                message: format!(
                    "payload variant `{}` disagrees with source_family `{}`",
                    self.payload.source_family().as_str(),
                    self.source_family.as_str()
                ),
            });
        }
        self.payload.validate()?;
        if !mode_allows_family(self.capture_mode, self.source_family) {
            return Err(DomainError::MalformedCapture {
                message: format!(
                    "capture_mode `{}` is incompatible with source_family `{}`",
                    self.capture_mode.as_str(),
                    self.source_family.as_str()
                ),
            });
        }

        let label = SensorLabel::from_identity(&self.sensor_id)?;
        super::capture_manifest::validate_label(&label)?;

        let expected_family =
            family_for_label(&label).ok_or_else(|| DomainError::UndeclaredSensor {
                label: label.as_str().to_owned(),
            })?;
        if expected_family != self.source_family {
            return Err(DomainError::MalformedCapture {
                message: format!(
                    "sensor `{}` declares family `{}` but event source_family is `{}`",
                    self.sensor_id.as_str(),
                    expected_family.as_str(),
                    self.source_family.as_str()
                ),
            });
        }

        super::actor_chain::validate_chain(&self.actor_chain)?;
        super::capture_attribution::attribute(self.capture_mode, &self.actor_chain)?;

        match self.capture_mode {
            CaptureMode::Auto => {
                bind_auto_author(&self.actor_chain, &self.sensor_id)?;
            }
            CaptureMode::Proactive => {
                bind_proactive_author(&self.actor_chain, &self.sensor_id, label.as_str())?;
            }
            CaptureMode::Explicit => {}
        }
        bind_chain_sensor_entries(&self.actor_chain, &self.sensor_id)?;

        Ok(())
    }
}

/// True iff a [`CaptureMode`] is permitted to carry events of a given
/// [`SourceFamily`] per brief §5.0.a.
const fn mode_allows_family(mode: CaptureMode, family: SourceFamily) -> bool {
    matches!(
        (mode, family),
        (
            CaptureMode::Auto,
            SourceFamily::Hook
                | SourceFamily::Ide
                | SourceFamily::Terminal
                | SourceFamily::Clipboard
                | SourceFamily::Voice
                | SourceFamily::Screen
                | SourceFamily::RecordingBatch,
        ) | (CaptureMode::Explicit, SourceFamily::Cli | SourceFamily::Mcp)
            | (CaptureMode::Proactive, SourceFamily::Proactive)
    )
}

/// Map a declared [`SensorLabel`] prefix to the [`SourceFamily`] its
/// sensor is allowed to emit. Every prefix in
/// [`super::capture_manifest::P0_SENSOR_LABEL_PREFIXES`] has a fixed
/// family — a sensor cannot impersonate another capture surface.
///
/// `local:neuroskill:` is pinned to [`SourceFamily::Hook`] because brief
/// §9.1 describes it as "structured agent tool-call traces emitted by
/// the harness itself" — semantically the same channel as harness
/// `PostToolUse`/`PreToolUse` hooks, just emitted directly by the
/// harness's neuroskill protocol. If a future payload variant needs a
/// different family, add it to [`SourceFamily`] and pin the prefix
/// here in the same PR.
///
/// Returns `None` only for labels not in the manifest — callers should
/// run [`super::capture_manifest::validate_label`] before this so an
/// unpinned label is impossible at the call site.
fn family_for_label(label: &SensorLabel) -> Option<SourceFamily> {
    let s = label.as_str();
    if s.starts_with("local:hook:") || s.starts_with("local:neuroskill:") {
        Some(SourceFamily::Hook)
    } else if s.starts_with("local:ide:") {
        Some(SourceFamily::Ide)
    } else if s.starts_with("local:terminal:") {
        Some(SourceFamily::Terminal)
    } else if s.starts_with("local:clipboard:") {
        Some(SourceFamily::Clipboard)
    } else if s.starts_with("local:voice:") {
        Some(SourceFamily::Voice)
    } else if s.starts_with("local:screen:") {
        Some(SourceFamily::Screen)
    } else if s.starts_with("local:recording:") {
        Some(SourceFamily::RecordingBatch)
    } else if s.starts_with("local:cli:") {
        Some(SourceFamily::Cli)
    } else if s.starts_with("local:mcp:") {
        Some(SourceFamily::Mcp)
    } else if s.starts_with("local:proactive:") {
        Some(SourceFamily::Proactive)
    } else {
        None
    }
}

/// Reject `payload_ref` strings that would let a downstream stage
/// dereference bytes outside the managed `sources/` vault layer (§3).
///
/// Accepted shape: a non-empty vault-relative path that begins with the
/// literal prefix `sources/`, contains no `..` segment, no empty `//`
/// segment, no leading `/`, no NUL byte, no scheme/authority marker
/// (`://`), and no query or fragment. A downstream resolver joins this
/// against the configured vault root, so the envelope alone cannot
/// reference paths outside the vault. Pure syntactic check — no
/// filesystem access at this layer.
fn validate_payload_ref(raw: &str) -> Result<(), DomainError> {
    validate_vault_relative_path("payload_ref", raw)
}

/// Shared vault-relative path validator used by both `payload_ref` and
/// per-payload path fields like `RecordingBatch.recording_path`. See the
/// [`validate_payload_ref`] doc-comment for the accepted shape.
fn validate_vault_relative_path(field: &'static str, raw: &str) -> Result<(), DomainError> {
    if raw.is_empty() {
        return Err(DomainError::EmptyField { field });
    }
    if raw.contains('\0') {
        return Err(DomainError::MalformedCapture {
            message: format!("{field}: NUL byte not permitted"),
        });
    }
    // Reject backslashes regardless of host platform — vault paths are
    // forward-slash canonical. This blocks `sources/..\\..\\Win\\x` from
    // smuggling a parent-dir hop through a single segment on consumers
    // that normalize backslashes.
    if raw.contains('\\') {
        return Err(DomainError::MalformedCapture {
            message: format!("{field} `{raw}`: backslash not permitted"),
        });
    }
    if raw.contains("://") {
        return Err(DomainError::MalformedCapture {
            message: format!("{field} `{raw}`: scheme not permitted, use vault-relative path"),
        });
    }
    if raw.starts_with('/') {
        return Err(DomainError::MalformedCapture {
            message: format!("{field} `{raw}`: must not be absolute"),
        });
    }
    if !raw.starts_with("sources/") {
        return Err(DomainError::MalformedCapture {
            message: format!("{field} `{raw}`: must begin with `sources/`"),
        });
    }
    if raw.contains('?') || raw.contains('#') {
        return Err(DomainError::MalformedCapture {
            message: format!("{field} `{raw}`: query/fragment not permitted"),
        });
    }
    for segment in raw.split('/') {
        if segment.is_empty() {
            return Err(DomainError::MalformedCapture {
                message: format!("{field} `{raw}`: empty path segment"),
            });
        }
        if segment == ".." {
            return Err(DomainError::MalformedCapture {
                message: format!("{field} `{raw}`: `..` traversal not permitted"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ChainRole;

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid")
    }

    fn ulid_a() -> CaptureEventId {
        // Crockford-base32, lexically valid ULID.
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ULID")
    }

    fn sensor() -> Identity {
        Identity::parse("snr:local:hook:cc-session:v1").expect("valid")
    }

    fn entry(role: ChainRole, id: &str) -> ActorChainEntry {
        ActorChainEntry {
            role,
            identity: Identity::parse(id).expect("valid"),
            at: ts(),
        }
    }

    fn auto_event() -> CaptureEvent {
        CaptureEvent {
            event_id: ulid_a(),
            sensor_id: sensor(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![entry(ChainRole::Author, "snr:local:hook:cc-session:v1")],
            refs: Some(CaptureRefs {
                session_id: Some("sess-42".into()),
                turn_id: Some("turn-7".into()),
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(format!("sha256:{}", "ab".repeat(32))).expect("valid"),
            payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
            captured_at: ts(),
            payload: CapturePayload::Hook {
                hook_name: "PostToolUse".into(),
                tool_name: Some("Read".into()),
            },
            source_family: SourceFamily::Hook,
        }
    }

    #[test]
    fn captureeventid_parses_ulid() {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    }

    #[test]
    fn captureeventid_rejects_bad_length() {
        let err = CaptureEventId::parse("SHORT").unwrap_err();
        assert!(matches!(err, DomainError::MalformedCapture { .. }));
    }

    #[test]
    fn captureeventid_rejects_bad_chars() {
        // 'I' is excluded from Crockford base32.
        let err = CaptureEventId::parse("0IARZ3NDEKTSV4RRFFQ69G5FAV").unwrap_err();
        assert!(matches!(err, DomainError::MalformedCapture { .. }));
    }

    #[test]
    fn payload_hash_parses() {
        PayloadHash::parse(format!("sha256:{}", "0".repeat(64))).expect("valid");
    }

    #[test]
    fn payload_hash_rejects_uppercase() {
        let err = PayloadHash::parse(format!("sha256:{}", "A".repeat(64))).unwrap_err();
        assert!(matches!(err, DomainError::InvalidPayloadHash { .. }));
    }

    #[test]
    fn payload_hash_rejects_short() {
        let err = PayloadHash::parse(format!("sha256:{}", "a".repeat(63))).unwrap_err();
        assert!(matches!(err, DomainError::InvalidPayloadHash { .. }));
    }

    #[test]
    fn payload_hash_rejects_missing_prefix() {
        let err = PayloadHash::parse("a".repeat(64)).unwrap_err();
        assert!(matches!(err, DomainError::InvalidPayloadHash { .. }));
    }

    #[test]
    fn capture_mode_round_trip() {
        for mode in [
            CaptureMode::Auto,
            CaptureMode::Explicit,
            CaptureMode::Proactive,
        ] {
            let s = serde_json::to_string(&mode).expect("ser");
            let back: CaptureMode = serde_json::from_str(&s).expect("de");
            assert_eq!(back, mode);
        }
    }

    #[test]
    fn capture_mode_rejects_unknown() {
        let err = CaptureMode::parse("bogus").unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedCaptureMode { .. }));
    }

    #[test]
    fn source_family_round_trip() {
        for fam in [
            SourceFamily::Hook,
            SourceFamily::Ide,
            SourceFamily::Terminal,
            SourceFamily::Clipboard,
            SourceFamily::Voice,
            SourceFamily::Screen,
            SourceFamily::RecordingBatch,
            SourceFamily::Cli,
            SourceFamily::Mcp,
            SourceFamily::Proactive,
        ] {
            assert_eq!(SourceFamily::parse(fam.as_str()).expect("round-trip"), fam);
        }
    }

    #[test]
    fn source_family_rejects_unknown() {
        let err = SourceFamily::parse("smell").unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedSourceFamily { .. }));
    }

    #[test]
    fn sensor_label_from_identity_strips_prefix() {
        let lbl = SensorLabel::from_identity(&sensor()).expect("valid");
        assert_eq!(lbl.as_str(), "local:hook:cc-session:v1");
    }

    #[test]
    fn sensor_label_rejects_non_sensor_identity() {
        let usr = Identity::parse("usr:tafeng").expect("valid");
        let err = SensorLabel::from_identity(&usr).unwrap_err();
        assert!(matches!(err, DomainError::MalformedCapture { .. }));
    }

    #[test]
    fn validate_accepts_well_formed_auto_event() {
        auto_event().validate().expect("valid");
    }

    #[test]
    fn validate_rejects_payload_family_mismatch() {
        let mut ev = auto_event();
        ev.source_family = SourceFamily::Voice;
        let err = ev.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedCapture { .. }));
    }

    #[test]
    fn validate_rejects_non_sensor_sensor_id() {
        let mut ev = auto_event();
        ev.sensor_id = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        // Author also needs to match for fairness; but the sensor_id check
        // fails first.
        let err = ev.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedCapture { .. }));
    }

    #[test]
    fn validate_rejects_empty_payload_ref() {
        let mut ev = auto_event();
        ev.payload_ref.clear();
        let err = ev.validate().unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { .. }));
    }

    #[test]
    fn json_round_trip() {
        let ev = auto_event();
        let s = serde_json::to_string(&ev).expect("ser");
        let back: CaptureEvent = serde_json::from_str(&s).expect("de");
        assert_eq!(back, ev);
    }

    #[test]
    fn deny_unknown_fields_at_envelope_level() {
        let ev = auto_event();
        let mut v = serde_json::to_value(&ev).expect("to_value");
        v.as_object_mut()
            .expect("obj")
            .insert("rogue".into(), serde_json::json!("nope"));
        let s = serde_json::to_string(&v).expect("ser");
        let res: Result<CaptureEvent, _> = serde_json::from_str(&s);
        assert!(res.is_err(), "unknown fields must be rejected");
    }
}
