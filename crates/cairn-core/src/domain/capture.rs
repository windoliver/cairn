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
    /// Parse a ULID. Returns [`DomainError::MalformedCapture`] if the input
    /// is not exactly 26 Crockford base32 characters
    /// (`0-9`, `A-H`, `J`, `K`, `M`, `N`, `P-T`, `V-Z`).
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.len() != 26 {
            return Err(DomainError::MalformedCapture {
                message: format!("event_id `{raw}`: ULID must be exactly 26 chars"),
            });
        }
        if !raw.bytes().all(is_crockford_base32) {
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
/// validation enforces this. Bodies are deliberately metadata-only: raw
/// content (utterances, frames, clipboard text) lives behind `payload_ref`
/// and is hashed by `payload_hash` so the envelope can be logged at
/// `info` without leaking source bytes (CLAUDE.md §6.6, brief §14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// URI pointing into the `sources/` vault layer (brief §3) where the
    /// raw bytes live. P0 stores everything as a `file://` URI.
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

impl CaptureEvent {
    /// Run every invariant on the event:
    ///
    /// 1. `payload_ref` is a non-empty `file://` URI rooted under
    ///    `/sources/` and free of `..` segments. P0 vault layout (§3)
    ///    keeps raw bytes under that subtree; rejecting other shapes
    ///    closes off SSRF / off-vault dereferences in any downstream
    ///    stage that resolves the URI.
    /// 2. `sensor_id` is a `snr:` identity (Mode B / C still flow through
    ///    a sensor surface).
    /// 3. `payload.source_family() == self.source_family`.
    /// 4. `capture_mode` and `source_family` are a declared §5.0.a pair
    ///    (Auto = sensor families, Explicit = `cli` / `mcp`,
    ///    Proactive = `proactive`).
    /// 5. `source_family` matches the family expected from the sensor
    ///    label — a `local:cli:` sensor cannot emit a `screen` payload.
    /// 6. `actor_chain` validates per §4.2
    ///    ([`super::actor_chain::validate_chain`]).
    /// 7. `actor_chain` author matches `capture_mode` per §5.0.a
    ///    ([`super::capture_attribution::attribute`]).
    /// 8. For Mode A captures, the `Author` entry's identity equals
    ///    `sensor_id` (the sensor authors its own raw events). For any
    ///    mode, if a `Sensor` role entry is present in the chain, it
    ///    must equal `sensor_id`.
    /// 9. Sensor label is in the declared P0 manifest
    ///    ([`super::capture_manifest::validate_label`]).
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

        if self.capture_mode == CaptureMode::Auto {
            let author = self
                .actor_chain
                .iter()
                .find(|e| e.role == ChainRole::Author)
                .ok_or_else(|| DomainError::MissingSignature {
                    message: "actor_chain has no `author` entry".to_owned(),
                })?;
            if author.identity != self.sensor_id {
                return Err(DomainError::AttributionMismatch {
                    message: format!(
                        "mode `auto` requires author identity `{}` to equal sensor_id `{}`",
                        author.identity.as_str(),
                        self.sensor_id.as_str()
                    ),
                });
            }
        }
        for entry in &self.actor_chain {
            if entry.role == ChainRole::Sensor && entry.identity != self.sensor_id {
                return Err(DomainError::AttributionMismatch {
                    message: format!(
                        "actor_chain `sensor` entry `{}` does not match sensor_id `{}`",
                        entry.identity.as_str(),
                        self.sensor_id.as_str()
                    ),
                });
            }
        }

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
/// Accepted shape: `file:///<abs-path>/sources/<rest>` — strictly empty
/// authority (three slashes after `file:`), no query, no fragment, no
/// `..` traversal. The path is percent-decoded before the traversal
/// check so encoded escapes (`%2e%2e`) cannot bypass the rule. Pure
/// syntactic check — no filesystem access at this layer.
fn validate_payload_ref(raw: &str) -> Result<(), DomainError> {
    if raw.is_empty() {
        return Err(DomainError::EmptyField {
            field: "payload_ref",
        });
    }
    // `file:///` rather than `file://`: require an empty authority so
    // `file://attacker-host/sources/x` is rejected. The third `/` is the
    // start of the absolute path.
    let path = raw
        .strip_prefix("file:///")
        .ok_or_else(|| DomainError::MalformedCapture {
            message: format!("payload_ref `{raw}`: must be a `file:///` URI with empty authority"),
        })?;
    if path.contains('?') || path.contains('#') {
        return Err(DomainError::MalformedCapture {
            message: format!("payload_ref `{raw}`: query/fragment not permitted"),
        });
    }
    let decoded = percent_decode(path).ok_or_else(|| DomainError::MalformedCapture {
        message: format!("payload_ref `{raw}`: malformed percent-encoding"),
    })?;
    // The decoded path must contain a `/sources/` segment so off-vault
    // paths like `/etc/passwd` are rejected, and must have no `..`
    // segments after decoding so percent-encoded traversals
    // (`%2e%2e`) cannot escape the vault root.
    if !decoded.contains("/sources/") && !decoded.starts_with("sources/") {
        return Err(DomainError::MalformedCapture {
            message: format!("payload_ref `{raw}`: path must contain `/sources/`"),
        });
    }
    for segment in decoded.split('/') {
        if segment == ".." {
            return Err(DomainError::MalformedCapture {
                message: format!("payload_ref `{raw}`: `..` traversal not permitted"),
            });
        }
    }
    Ok(())
}

/// Decode `%XX` escapes in a URI path. Returns `None` if a `%` is not
/// followed by two hex digits. Stays in `cairn-core` to avoid pulling
/// `percent-encoding` for one decoder.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = u8::try_from((bytes[i + 1] as char).to_digit(16)?).ok()?;
            let lo = u8::try_from((bytes[i + 2] as char).to_digit(16)?).ok()?;
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
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
            payload_ref: "file:///vault/sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
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
