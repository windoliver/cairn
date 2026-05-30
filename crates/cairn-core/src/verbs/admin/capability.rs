//! Capability-availability gate used by `cairn.admin.v1` callers (SDK,
//! CLI, MCP). Verbs themselves do not call this — they trust the caller
//! to have negotiated the capability — but the boundary surfaces use it
//! to return `AdminError::CapabilityUnavailable` when admin is dark.

use crate::domain::admin::AdminError;
use crate::status::remediation_for;
use crate::status::wiring::admin_extension_ready;

/// Per-verb capability strings, mirrored from spec §7.4.
pub const CAP_SNAPSHOT: &str = "cairn.mcp.v1.extension.admin.snapshot";
/// Per-verb capability string for restore.
pub const CAP_RESTORE: &str = "cairn.mcp.v1.extension.admin.restore";
/// Per-verb capability string for WAL replay.
pub const CAP_REPLAY_WAL: &str = "cairn.mcp.v1.extension.admin.replay_wal";
/// Per-verb capability string for connector enable.
pub const CAP_CONNECTOR_ENABLE: &str = "cairn.mcp.v1.extension.admin.connector.enable";
/// Per-verb capability string for connector disable.
pub const CAP_CONNECTOR_DISABLE: &str = "cairn.mcp.v1.extension.admin.connector.disable";
/// Per-verb capability string for connector backfill.
pub const CAP_CONNECTOR_BACKFILL: &str = "cairn.mcp.v1.extension.admin.connector.backfill";

/// All six `cairn.admin.v1` capability strings in spec §7.4 order.
///
/// Used by AC#1 tests and by surfaces that need to iterate the full set
/// (e.g., to pre-flight every admin capability before dispatch).
pub const ADMIN_CAPABILITIES: &[&str] = &[
    CAP_SNAPSHOT,
    CAP_RESTORE,
    CAP_REPLAY_WAL,
    CAP_CONNECTOR_ENABLE,
    CAP_CONNECTOR_DISABLE,
    CAP_CONNECTOR_BACKFILL,
];

/// Returns `Ok(())` when the extension is negotiated and the per-verb
/// capability may be honored. Returns `AdminError::CapabilityUnavailable`
/// with a remediation hint sourced from the central `REMEDIATION` table
/// otherwise.
///
/// `config_enabled` and `has_operator` come from the caller's runtime
/// context (`config.admin.enabled` and `AdminStateStore::has_any_operator()`).
///
/// # Errors
///
/// Returns `AdminError::CapabilityUnavailable { capability, remediation }`
/// when the extension is not ready.
pub fn ensure_admin_capability(
    capability: &'static str,
    config_enabled: bool,
    has_operator: bool,
) -> Result<(), AdminError> {
    if admin_extension_ready(config_enabled, has_operator) {
        return Ok(());
    }
    let remediation = remediation_for(capability)
        .unwrap_or("enable admin extension and grant operator role")
        .to_string();
    Err(AdminError::CapabilityUnavailable {
        capability: capability.into(),
        remediation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_capability_unavailable_when_runtime_gate_fails() {
        // ADMIN_EXTENSION_WIRED is true (phase 6); runtime gates still
        // enforce: with `has_operator = false`, the gate refuses.
        let err = ensure_admin_capability(CAP_SNAPSHOT, true, false).unwrap_err();
        match err {
            AdminError::CapabilityUnavailable {
                capability,
                remediation,
            } => {
                assert_eq!(capability, CAP_SNAPSHOT);
                assert!(!remediation.is_empty());
            }
            other => panic!("expected CapabilityUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn returns_capability_unavailable_when_config_disabled() {
        // Even if wired flips in phase 6.2, config_enabled=false still gates.
        let err = ensure_admin_capability(CAP_RESTORE, false, true).unwrap_err();
        assert!(matches!(err, AdminError::CapabilityUnavailable { .. }));
    }

    #[test]
    fn returns_capability_unavailable_when_no_operator() {
        let err = ensure_admin_capability(CAP_REPLAY_WAL, true, false).unwrap_err();
        assert!(matches!(err, AdminError::CapabilityUnavailable { .. }));
    }

    #[test]
    fn remediation_text_matches_table_when_present() {
        // Force the unnegotiated path via `has_operator = false` (the
        // helper only emits a remediation string when it's denying).
        let err = ensure_admin_capability(CAP_CONNECTOR_BACKFILL, true, false).unwrap_err();
        if let AdminError::CapabilityUnavailable { remediation, .. } = err {
            let table = remediation_for(CAP_CONNECTOR_BACKFILL).expect("present in REMEDIATION");
            assert_eq!(remediation, table);
        } else {
            panic!("expected CapabilityUnavailable");
        }
    }

    // AC#1 — the canonical list of admin capabilities is fixed.
    #[test]
    fn six_admin_capabilities_listed_in_order() {
        assert_eq!(ADMIN_CAPABILITIES.len(), 6);
        assert!(
            ADMIN_CAPABILITIES
                .iter()
                .all(|c| c.starts_with("cairn.mcp.v1.extension.admin.")),
            "every capability must be in the admin extension namespace"
        );
        // Pin the exact order mandated by spec §7.4.
        assert_eq!(ADMIN_CAPABILITIES[0], CAP_SNAPSHOT);
        assert_eq!(ADMIN_CAPABILITIES[1], CAP_RESTORE);
        assert_eq!(ADMIN_CAPABILITIES[2], CAP_REPLAY_WAL);
        assert_eq!(ADMIN_CAPABILITIES[3], CAP_CONNECTOR_ENABLE);
        assert_eq!(ADMIN_CAPABILITIES[4], CAP_CONNECTOR_DISABLE);
        assert_eq!(ADMIN_CAPABILITIES[5], CAP_CONNECTOR_BACKFILL);
    }

    // AC#4 — when either runtime gate is unsatisfied, every admin
    // capability returns CapabilityUnavailable with a remediation hint.
    #[test]
    fn unnegotiated_returns_capability_unavailable_for_all_caps() {
        // Two unnegotiated scenarios: config off, and no operator. Either
        // alone is enough to deny the capability.
        let unnegotiated: &[(bool, bool, &str)] = &[
            (false, true, "config_enabled=false"),
            (true, false, "has_operator=false"),
            (false, false, "both off"),
        ];
        for &(config_enabled, has_operator, label) in unnegotiated {
            for &cap in ADMIN_CAPABILITIES {
                let err = ensure_admin_capability(cap, config_enabled, has_operator)
                    .expect_err("unnegotiated case must refuse");
                match err {
                    AdminError::CapabilityUnavailable {
                        capability,
                        remediation,
                    } => {
                        assert_eq!(capability, cap, "{label}: capability string round-trip");
                        assert!(
                            !remediation.is_empty(),
                            "{label}: remediation must be non-empty for {cap}"
                        );
                    }
                    other => {
                        panic!("{label}: expected CapabilityUnavailable for {cap}, got {other:?}")
                    }
                }
            }
        }
    }

    // Positive case: with all runtime gates true, the helper returns
    // Ok(()) iff every build-time constant is also true. The test reads
    // the constants directly so it stays valid through the phased flip.
    #[test]
    fn negotiated_when_all_gates_true_tracks_wired_constants() {
        use crate::status::wiring;
        let all_wired = wiring::ADMIN_EXTENSION_WIRED
            && wiring::ADMIN_CLI_DISPATCH_WIRED
            && wiring::ADMIN_MCP_DISPATCH_WIRED;
        for &cap in ADMIN_CAPABILITIES {
            let result = ensure_admin_capability(
                cap, /*config_enabled=*/ true, /*has_operator=*/ true,
            );
            assert_eq!(
                result.is_ok(),
                all_wired,
                "{cap}: expected Ok iff all build-time wired constants are true"
            );
        }
    }

    // Lockstep guard: ADMIN_CAPABILITIES and the REMEDIATION table must
    // have matching entries so no capability ships without an operator hint.
    #[test]
    fn every_admin_capability_has_a_remediation_row() {
        for &cap in ADMIN_CAPABILITIES {
            let hint = remediation_for(cap);
            assert!(hint.is_some(), "missing REMEDIATION row for {cap}");
            assert!(
                !hint.unwrap().is_empty(),
                "empty remediation string for {cap}"
            );
        }
    }
}
