//! Contract versioning. Each contract module exports `CONTRACT_VERSION` of
//! type [`ContractVersion`]. Plugins declare a [`VersionRange`] they accept;
//! the registry rejects on mismatch (brief §4.1, "Contracts are versioned").

use serde::{Deserialize, Serialize};

/// Semver-shaped contract version. `0.x` is pre-stable; bumping `minor`
/// is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVersion {
    /// Major version component.
    pub major: u16,
    /// Minor version component.
    pub minor: u16,
    /// Patch version component.
    pub patch: u16,
}

impl ContractVersion {
    /// Construct a [`ContractVersion`] from its three components.
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl std::fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Major.minor schema-version stamp persisted alongside each
/// [`crate::contract::memory_store::StoredRecord`].
///
/// Distinct from [`ContractVersion`] because patch is irrelevant for
/// schema-lag detection (brief §6.4 / lint spec §6.4): a record written
/// at `0.3.7` and the current host at `0.3.12` carry the same schema
/// shape, so the patch component would only generate noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaVersion {
    /// Major version component.
    pub major: u32,
    /// Minor version component.
    pub minor: u32,
}

/// On-disk record-shape version. Bumps **only** when a change to the
/// canonical record bytes / hot-column projection forces older readers
/// to migrate. Decoupled from
/// [`crate::contract::memory_store::CONTRACT_VERSION`] so trait-surface
/// evolution (new methods, new public fields that don't reshape persisted
/// rows) does not manufacture spurious `stale_schema` findings on releases
/// that never touched record bytes (Issue #258 review round 5).
pub const RECORD_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(0, 1);

impl SchemaVersion {
    /// Construct a [`SchemaVersion`] from major / minor components.
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Project a [`ContractVersion`] onto its `(major, minor)` schema
    /// stamp, dropping `patch`.
    #[must_use]
    pub fn from_contract(version: ContractVersion) -> Self {
        Self {
            major: u32::from(version.major),
            minor: u32::from(version.minor),
        }
    }

    /// Stamp matching the host's current persisted-row schema — what
    /// fresh writes carry. Reads [`RECORD_SCHEMA_VERSION`], which is
    /// **not** the same as [`crate::contract::memory_store::CONTRACT_VERSION`]:
    /// the trait surface can evolve (new methods, new public fields)
    /// without changing how rows are interpreted on disk, and stamping
    /// rows from a trait-only bump would manufacture spurious
    /// `stale_schema` findings on releases that never touched record
    /// bytes (Issue #258 review round 5).
    #[must_use]
    pub const fn current() -> Self {
        RECORD_SCHEMA_VERSION
    }

    /// Compare a per-record stamp against `self` (the current host
    /// version). Drives the §6.4 `stale_schema` lint, which assigns
    /// severity per variant.
    ///
    /// Forward skew (`AheadBy`) is surfaced rather than collapsed into
    /// `Same`: if a vault rolled back from a newer binary still carries
    /// rows stamped at the newer contract, the older binary may
    /// misinterpret their shape and silently truncating the comparison
    /// would hide the incompatibility.
    #[must_use]
    pub const fn compare(self, record: SchemaVersion) -> SchemaSkew {
        if record.major != self.major {
            return SchemaSkew::MajorMismatch;
        }
        if self.minor == record.minor {
            SchemaSkew::Same
        } else if self.minor > record.minor {
            SchemaSkew::BehindBy(self.minor - record.minor)
        } else {
            SchemaSkew::AheadBy(record.minor - self.minor)
        }
    }
}

/// Result of comparing a per-record [`SchemaVersion`] against the
/// host's current contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaSkew {
    /// Record stamp matches host major.minor.
    Same,
    /// Record minor is `n` behind the host (record older).
    BehindBy(u32),
    /// Record minor is `n` ahead of the host (rollback / forward skew).
    AheadBy(u32),
    /// Record and host disagree on major; magnitude not meaningful.
    MajorMismatch,
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Half-open range `[min, max_exclusive)` over `ContractVersion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionRange {
    /// Lower bound (inclusive).
    pub min: ContractVersion,
    /// Upper bound (exclusive).
    pub max_exclusive: ContractVersion,
}

impl VersionRange {
    /// Construct a [`VersionRange`] with the given inclusive lower and exclusive upper bounds.
    #[must_use]
    pub const fn new(min: ContractVersion, max_exclusive: ContractVersion) -> Self {
        Self { min, max_exclusive }
    }

    /// `true` iff `host` is in `[min, max_exclusive)`.
    #[must_use]
    pub const fn accepts(&self, host: ContractVersion) -> bool {
        // Hand-unfolded lexicographic compare: tuple `PartialOrd` is not yet
        // stable in const context, so we cannot use `(a, b, c) >= (..)` here.
        let ge_min = host.major > self.min.major
            || (host.major == self.min.major
                && (host.minor > self.min.minor
                    || (host.minor == self.min.minor && host.patch >= self.min.patch)));
        let lt_max = host.major < self.max_exclusive.major
            || (host.major == self.max_exclusive.major
                && (host.minor < self.max_exclusive.minor
                    || (host.minor == self.max_exclusive.minor
                        && host.patch < self.max_exclusive.patch)));
        ge_min && lt_max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_min_inclusive() {
        let range = VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
        assert!(range.accepts(ContractVersion::new(0, 1, 0)));
    }

    #[test]
    fn rejects_max_exclusive() {
        let range = VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
        assert!(!range.accepts(ContractVersion::new(0, 2, 0)));
    }

    #[test]
    fn accepts_within_range() {
        let range = VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
        assert!(range.accepts(ContractVersion::new(0, 1, 5)));
    }

    #[test]
    fn rejects_below_min() {
        let range = VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
        assert!(!range.accepts(ContractVersion::new(0, 0, 9)));
    }

    #[test]
    fn version_display() {
        assert_eq!(ContractVersion::new(1, 2, 3).to_string(), "1.2.3");
    }
}
