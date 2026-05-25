//! `ConnectorManifest` — adapter-shipped TOML describing labels,
//! scopes, OAuth requirements, budgets, and payload limits. Hashed
//! into the consent journal on enable; drift triggers `ConsentRevoked`.
//!
//! Issue #130, brief §9.1 source sensors, §19 v0.3.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ConnectorError;

/// Full connector manifest deserialized from a `connector.toml` file.
///
/// Every block is required; `#[serde(deny_unknown_fields)]` is applied to
/// every struct so typos in field names surface as parse errors rather than
/// being silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorManifest {
    /// Identity + contract declaration.
    pub connector: ConnectorMeta,
    /// Advertised capability flags.
    pub capabilities: CapabilitiesBlock,
    /// OAuth credential requirements.
    pub oauth: OauthBlock,
    /// Per-connector emission budgets.
    pub budget: BudgetBlock,
    /// Label allow-list (gates every emit).
    pub labels: LabelsBlock,
    /// Declared scope patterns (consent journal scope matching).
    pub scopes: ScopesBlock,
    /// Webhook signature requirements.
    pub webhook: WebhookBlock,
    /// Poll scheduling parameters.
    pub poll: PollBlock,
    /// Payload size + depth limits.
    pub payload: PayloadBlock,
}

/// Connector identity + contract declaration block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorMeta {
    /// Unique connector name — short ASCII slug.
    pub name: String,
    /// Must equal `"Connector"` (the `ContractKind` string).
    pub contract: String,
    /// Semver string for the version of the connector implementation.
    pub contract_version: String,
    /// Sensor identity token in the canonical wire form `snr:local:connector:<name>:v<n>`.
    ///
    /// The `local:connector:` root is used because external connectors run
    /// inside the same process as `cairn-core` (loaded via `ConnectorPlugin`)
    /// and therefore occupy the `local:` family in `cairn-core`'s
    /// `P0_SENSOR_LABEL_PREFIXES` (brief §4.2 + §19 v0.3).  Example:
    /// `"snr:local:connector:github:v1"`.
    ///
    /// Note: the `local:` root is provisional pending a brief-level decision
    /// on whether external connectors should warrant their own `connector:`
    /// root family — see issue #130 spec §9.
    pub sensor_identity: String,
}

/// Advertised capability flags for this connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesBlock {
    /// Connector supports periodic polling.
    pub poll: bool,
    /// Connector can receive webhook deliveries.
    pub webhook: bool,
    /// Connector supports backfill from a cursor.
    pub backfill: bool,
}

/// OAuth credential requirements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OauthBlock {
    /// Scopes to request from the OAuth provider.
    pub required_scopes: Vec<String>,
    /// Human-readable token lifetime hint (e.g. `"1h"`); parsed by adapters.
    pub token_lifetime: String,
    /// Whether the connector supports token refresh.
    pub refresh: bool,
}

/// Per-connector emission budgets to prevent runaway ingestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetBlock {
    /// Maximum number of items the connector may emit per hour.
    pub max_items_per_hour: u32,
    /// Maximum byte volume per day as a human-readable string (e.g. `"50MiB"`).
    pub max_bytes_per_day: String,
}

/// Label allow-list — every emitted event must carry only declared labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelsBlock {
    /// Allowed label names. Must be non-empty and duplicate-free.
    pub allowed: Vec<String>,
}

/// Declared scope patterns the connector is permitted to emit into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopesBlock {
    /// Array of scope pattern entries (`[[scopes.declared]]` in TOML).
    pub declared: Vec<ScopePattern>,
}

/// One declared scope pattern (`kind` + `pattern` glob).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopePattern {
    /// Pattern family name (e.g. `"project"`, `"channel"`).
    pub kind: String,
    /// Glob pattern matched against the scope value (e.g. `"*"`, `"owner/*"`).
    pub pattern: String,
}

/// Webhook signature requirements.
///
/// The TOML keys use dotted names (`"signature.algorithm"`, `"signature.header"`)
/// which are preserved verbatim in the TOML file via `#[serde(rename = …)]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookBlock {
    /// HMAC algorithm identifier (e.g. `"hmac-sha256"`).
    #[serde(rename = "signature.algorithm")]
    pub signature_algorithm: String,
    /// HTTP header carrying the signature (e.g. `"X-Fixture-Signature"`).
    #[serde(rename = "signature.header")]
    pub signature_header: String,
    /// MIME types accepted by the webhook endpoint.
    pub allowed_mimes: Vec<String>,
}

/// Poll scheduling parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollBlock {
    /// Cursor representation kind (`"opaque-string"`, `"timestamp-ms"`, …).
    pub cursor_kind: String,
    /// Minimum polling interval as a human-readable duration (e.g. `"30s"`).
    pub min_interval: String,
    /// Default polling interval as a human-readable duration (e.g. `"5m"`).
    pub default_interval: String,
}

/// Payload size and depth limits applied before redaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadBlock {
    /// Maximum payload size as a human-readable string (e.g. `"256KiB"`).
    pub max_bytes: String,
    /// Parsed `max_bytes` value in bytes.
    ///
    /// Set by [`ConnectorManifest::parse_toml`] from the `max_bytes` string;
    /// avoids re-parsing on every emitted event (brief §130 Fix 4).
    #[serde(skip)]
    pub max_bytes_parsed: u64,
    /// Maximum JSON nesting depth before the payload is rejected.
    pub max_depth: u32,
}

/// Parse a human-readable byte-size string into a `u64` byte count.
///
/// Accepts only the binary suffixes used in connector manifests:
/// `KiB` (1 024), `MiB` (1 048 576), `GiB` (1 073 741 824) and the
/// suffix-free form (plain bytes). Fails closed on any unrecognised suffix.
///
/// # Errors
///
/// Returns `Err` with a descriptive message on unrecognised format.
pub fn parse_byte_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("GiB") {
        n.trim()
            .parse::<u64>()
            .map(|v| v * 1_073_741_824)
            .map_err(|e| format!("invalid GiB value \"{s}\": {e}"))
    } else if let Some(n) = s.strip_suffix("MiB") {
        n.trim()
            .parse::<u64>()
            .map(|v| v * 1_048_576)
            .map_err(|e| format!("invalid MiB value \"{s}\": {e}"))
    } else if let Some(n) = s.strip_suffix("KiB") {
        n.trim()
            .parse::<u64>()
            .map(|v| v * 1_024)
            .map_err(|e| format!("invalid KiB value \"{s}\": {e}"))
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("invalid byte size \"{s}\": {e}"))
    }
}

impl ConnectorManifest {
    /// Parse a TOML string into a `ConnectorManifest` and validate invariants.
    ///
    /// Returns `Err(ConnectorError::MalformedPayload(_))` on any parse or
    /// validation failure. This is a configuration-time concern — no transient
    /// or fatal paths are used.
    pub fn parse_toml(src: &str) -> Result<Self, ConnectorError> {
        let mut parsed: Self = toml::from_str(src)
            .map_err(|e| ConnectorError::MalformedPayload(format!("manifest: {e}")))?;
        // Parse max_bytes once at manifest-load time so event processing does
        // not need to re-parse the string on every emitted event.
        parsed.payload.max_bytes_parsed =
            parse_byte_size(&parsed.payload.max_bytes).map_err(|e| {
                ConnectorError::MalformedPayload(format!("payload.max_bytes: {e}"))
            })?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Validate invariants that cannot be expressed as TOML schema constraints.
    fn validate(&self) -> Result<(), ConnectorError> {
        // Contract name must be exactly "Connector".
        if self.connector.contract != "Connector" {
            return Err(ConnectorError::MalformedPayload(format!(
                "contract must be \"Connector\", got \"{}\"",
                self.connector.contract
            )));
        }
        // Label allow-list must be non-empty.
        if self.labels.allowed.is_empty() {
            return Err(ConnectorError::MalformedPayload(
                "labels.allowed must contain at least one entry".into(),
            ));
        }
        // Label allow-list must not contain duplicates.
        let mut seen = BTreeSet::new();
        for label in &self.labels.allowed {
            if !seen.insert(label.as_str()) {
                return Err(ConnectorError::MalformedPayload(format!(
                    "duplicate label \"{label}\""
                )));
            }
        }
        // At least one declared scope is required.
        if self.scopes.declared.is_empty() {
            return Err(ConnectorError::MalformedPayload(
                "scopes.declared must contain at least one entry".into(),
            ));
        }
        Ok(())
    }

    /// Return the connector's name (short slug from `connector.name`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.connector.name
    }

    /// Return `true` if `label` appears in the `labels.allowed` list.
    #[must_use]
    pub fn allowed_label(&self, label: &str) -> bool {
        self.labels.allowed.iter().any(|l| l == label)
    }

    /// Return `true` if the `(scope_kind, scope_value)` pair matches at least
    /// one entry in `scopes.declared`.
    ///
    /// Pattern matching is intentionally simple: a pattern of `"*"` matches any
    /// value within the same `kind`; any other pattern requires an exact match.
    /// This matches the design spec's "keep it simple" guidance for P0.
    #[must_use]
    pub fn scope_matches(&self, scope_kind: &str, scope_value: &str) -> bool {
        self.scopes.declared.iter().any(|p| {
            p.kind == scope_kind && (p.pattern == "*" || p.pattern == scope_value)
        })
    }

    /// Return `true` if `mime` appears in the webhook's `allowed_mimes` list.
    #[must_use]
    pub fn allowed_mime(&self, mime: &str) -> bool {
        self.webhook.allowed_mimes.iter().any(|m| m == mime)
    }

    /// Compute a stable SHA-256 hash of this manifest for use in the consent
    /// journal. The hash is derived from a canonical TOML serialization of the
    /// manifest, prefixed with `"sha256:"`.
    ///
    /// # Determinism
    /// Identical `ConnectorManifest` values always produce the same hash because
    /// `toml::to_string` is deterministic for structs with derived `Serialize`
    /// (field order follows declaration order; `BTreeSet`/`Vec` are ordered).
    #[must_use]
    pub fn hash(&self) -> String {
        // SAFETY (invariant): a fully-typed struct with derived Serialize
        // cannot produce a serialization error — all fields are basic types.
        let canonical = toml::to_string(self)
            .expect("infallible: derived Serialize on fully-typed ConnectorManifest");
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[connector]
name = "fixture"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:fixture:v1"

[capabilities]
poll = true
webhook = true
backfill = true

[oauth]
required_scopes = ["read:fixture"]
token_lifetime = "1h"
refresh = true

[budget]
max_items_per_hour = 600
max_bytes_per_day = "50MiB"

[labels]
allowed = ["note", "comment"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Fixture-Signature"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "256KiB"
max_depth = 12
"#;

    #[test]
    fn parses_valid_manifest() {
        let m = ConnectorManifest::parse_toml(VALID).expect("parse");
        assert_eq!(m.name(), "fixture");
        assert!(m.capabilities.poll);
        assert!(m.allowed_label("note"));
        assert!(!m.allowed_label("unknown"));
    }

    #[test]
    fn manifest_hash_stable() {
        let a = ConnectorManifest::parse_toml(VALID).unwrap().hash();
        let b = ConnectorManifest::parse_toml(VALID).unwrap().hash();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_dup_labels() {
        let bad = VALID.replace(r#"["note", "comment"]"#, r#"["note", "note"]"#);
        assert!(ConnectorManifest::parse_toml(&bad).is_err());
    }

    #[test]
    fn parses_payload_block() {
        let m = ConnectorManifest::parse_toml(VALID).expect("parse");
        assert_eq!(m.payload.max_bytes, "256KiB");
        assert_eq!(m.payload.max_depth, 12);
    }

    #[test]
    fn rejects_wrong_contract_kind() {
        let bad = VALID.replace(r#"contract = "Connector""#, r#"contract = "MemoryStore""#);
        assert!(ConnectorManifest::parse_toml(&bad).is_err());
    }

    /// Round-trip a manifest whose `sensor_identity` uses the canonical
    /// `snr:local:connector:<name>:v<n>` wire form (brief §4.2 + §19 v0.3,
    /// aligned with `cairn-core::domain::capture_manifest::P0_SENSOR_LABEL_PREFIXES`).
    #[test]
    fn parses_manifest_with_local_connector_identity() {
        let m = ConnectorManifest::parse_toml(VALID).expect("parse");
        assert_eq!(
            m.connector.sensor_identity,
            "snr:local:connector:fixture:v1",
        );
        // Verify the value survives a TOML serialize → deserialize round-trip.
        let serialized = toml::to_string(&m).expect("serialize");
        let reparsed = ConnectorManifest::parse_toml(&serialized).expect("reparse");
        assert_eq!(
            reparsed.connector.sensor_identity,
            m.connector.sensor_identity
        );
    }
}
