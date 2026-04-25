//! Contract versioning. Each contract module exports `CONTRACT_VERSION` of
//! type [`ContractVersion`]. Plugins declare a [`VersionRange`] they accept;
//! the registry rejects on mismatch (brief §4.1, "Contracts are versioned").

use serde::{Deserialize, Serialize};

/// Semver-shaped contract version. `0.x` is pre-stable; bumping `minor`
/// is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Half-open range `[min, max_exclusive)` over `ContractVersion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn accepts(&self, host: ContractVersion) -> bool {
        let key = (host.major, host.minor, host.patch);
        let lo = (self.min.major, self.min.minor, self.min.patch);
        let hi = (
            self.max_exclusive.major,
            self.max_exclusive.minor,
            self.max_exclusive.patch,
        );
        key >= lo && key < hi
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
