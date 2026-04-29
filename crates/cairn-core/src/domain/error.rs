//! Domain validation errors. Returned by [`crate::domain::MemoryRecord::validate`]
//! and the constructors of supporting types when invariants from the brief
//! (§4.2, §6, §6.5) are violated.

use thiserror::Error;

/// Validation failures rejected before any [`crate::contract::MemoryStore`] write.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DomainError {
    /// `provenance` is missing one of `source_sensor`, `created_at`,
    /// `originating_agent_id`, `source_hash`, or `consent_ref`. §6.5 makes
    /// every component mandatory; only `llm_id_if_any` is optional.
    #[error("provenance: missing required field `{field}`")]
    MissingProvenance {
        /// Name of the missing provenance field.
        field: &'static str,
    },

    /// An [`crate::domain::Identity`] string failed prefix or character-class
    /// validation. Identities must match `^(agt|usr|snr):[A-Za-z0-9._:-]+$`
    /// per `crates/cairn-idl/schema/common/primitives.json`.
    #[error("identity: {message}")]
    InvalidIdentity {
        /// Specific reason the identity string was rejected.
        message: String,
    },

    /// The [`crate::domain::ScopeTuple`] had no narrowing dimension set or
    /// contained an empty string component. §6 mandates a non-empty scope
    /// on every record.
    #[error("scope: {message}")]
    MalformedScope {
        /// Specific reason the scope tuple was rejected.
        message: String,
    },

    /// A [`crate::domain::MemoryVisibility`] value did not parse to one of the
    /// six recognized tiers (§6.3).
    #[error("visibility: unsupported tier `{value}`")]
    UnsupportedVisibility {
        /// Tier string that failed to parse.
        value: String,
    },

    /// The record carries no `signature`, an empty `actor_chain`, or no
    /// chain entry with `role: author`. §4.2 requires every write to be
    /// signed by an author at minimum.
    #[error("signature: {message}")]
    MissingSignature {
        /// Specific reason signature metadata was rejected.
        message: String,
    },

    /// A timestamp string was not a valid RFC3339 / ISO-8601 instant.
    #[error("timestamp: {message}")]
    InvalidTimestamp {
        /// Specific reason the timestamp was rejected.
        message: String,
    },

    /// A scalar (confidence, salience, evidence component) fell outside its
    /// declared range — for example `confidence` outside `[0.0, 1.0]`
    /// (§6.4).
    #[error("scalar: {field} out of range: {message}")]
    OutOfRange {
        /// Field name (e.g., `"confidence"`).
        field: &'static str,
        /// Specific reason the value was rejected.
        message: String,
    },

    /// A [`crate::domain::MemoryKind`] string did not match any of the 19
    /// recognized kinds (§6.1). Classifiers may not invent new kinds.
    #[error("kind: unsupported memory kind `{value}`")]
    UnsupportedKind {
        /// Kind string that failed to parse.
        value: String,
    },

    /// A [`crate::domain::MemoryClass`] string did not match any of the 4
    /// recognized classes (§6.2).
    #[error("class: unsupported memory class `{value}`")]
    UnsupportedClass {
        /// Class string that failed to parse.
        value: String,
    },

    /// A [`crate::domain::ConfidenceBand`] string did not match `high`,
    /// `normal`, or `uncertain` (§6.4).
    #[error("confidence_band: unsupported value `{value}`")]
    UnsupportedConfidenceBand {
        /// Band string that failed to parse.
        value: String,
    },

    /// A required string field was empty (e.g., `body`, `record_id`).
    #[error("required field `{field}` must not be empty")]
    EmptyField {
        /// Name of the empty field.
        field: &'static str,
    },

    /// A `CaptureEvent` (§5.0.a / §9) was malformed — missing fields, bad
    /// shape for its [`crate::domain::SourceFamily`] variant, or violated
    /// an internal invariant unrelated to identity/scope/timestamps.
    #[error("capture: {message}")]
    MalformedCapture {
        /// Specific reason the capture event was rejected.
        message: String,
    },

    /// A [`crate::domain::SourceFamily`] string did not parse to one of the
    /// declared families (§9.1).
    #[error("source_family: unsupported family `{value}`")]
    UnsupportedSourceFamily {
        /// Family string that failed to parse.
        value: String,
    },

    /// A [`crate::domain::CaptureMode`] string did not parse to `auto`,
    /// `explicit`, or `proactive` (§5.0.a).
    #[error("capture_mode: unsupported mode `{value}`")]
    UnsupportedCaptureMode {
        /// Mode string that failed to parse.
        value: String,
    },

    /// A `SensorLabel` (the `body` portion of a `snr:` identity, §9.1) was
    /// not present in the declared P0 manifest. Sensors may not emit
    /// `CaptureEvent`s under labels they have not registered.
    #[error("sensor_label: undeclared label `{label}`")]
    UndeclaredSensor {
        /// Label string that failed manifest validation.
        label: String,
    },

    /// The `actor_chain` author for a [`crate::domain::CaptureMode`] did
    /// not match the §5.0.a attribution rule
    /// (Mode A → sensor, Mode B → human, Mode C → agent).
    #[error("attribution: {message}")]
    AttributionMismatch {
        /// Specific reason the mode/author pairing was rejected.
        message: String,
    },

    /// A payload hash string did not match `sha256:<64 lowercase hex>` —
    /// the same shape `CanonicalRecordHash` and `target_hash` use.
    #[error("payload_hash: {message}")]
    InvalidPayloadHash {
        /// Specific reason the hash string was rejected.
        message: String,
    },

    /// A `SessionId` (§8.1) was empty or contained characters outside
    /// `[A-Za-z0-9._:-]`.
    #[error("session_id: {message}")]
    InvalidSessionId {
        /// Specific reason the session identifier was rejected.
        message: String,
    },
}
