//! Integration tests for backup registry domain helpers.

#![allow(missing_docs)]

use cairn_core::domain::{
    BackupRegistryEntry, DomainError, RewritePlan, Rfc3339Timestamp, ShreddedBackupEntry,
    TargetId,
};

#[test]
fn backup_registry_entry_rejects_empty_artifact_path() {
    let entry = BackupRegistryEntry {
        backup_id: "bkp_01J00000000000000000000000".to_owned(),
        created_at: Rfc3339Timestamp::parse("2026-05-12T12:00:00Z").expect("valid timestamp"),
        artifact_path: "   ".to_owned(),
        target_ids_included: vec![TargetId::parse("01HQZX9F5N0000000000000000").expect("valid")],
    };

    let err = entry.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField { field: "artifact_path" });
}

#[test]
fn backup_registry_entry_rejects_empty_backup_id() {
    let entry = BackupRegistryEntry {
        backup_id: "  ".to_owned(),
        created_at: Rfc3339Timestamp::parse("2026-05-12T12:00:00Z").expect("valid timestamp"),
        artifact_path: ".cairn/backups/backup-01.tar.zst".to_owned(),
        target_ids_included: vec![TargetId::parse("01HQZX9F5N0000000000000000").expect("valid")],
    };

    let err = entry.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField { field: "backup_id" });
}

#[test]
fn rewrite_plan_is_deterministic_for_multiple_targets() {
    let plan = RewritePlan::for_targets(
        "bkp_01J00000000000000000000000",
        vec![
            TargetId::parse("01HQZX9F5N0000000000000002").expect("valid"),
            TargetId::parse("01HQZX9F5N0000000000000001").expect("valid"),
            TargetId::parse("01HQZX9F5N0000000000000002").expect("valid"),
            TargetId::parse("01HQZX9F5N0000000000000000").expect("valid"),
        ],
    );

    let ordered: Vec<&str> = plan.target_ids.iter().map(TargetId::as_str).collect();
    assert_eq!(
        ordered,
        vec![
            "01HQZX9F5N0000000000000000",
            "01HQZX9F5N0000000000000001",
            "01HQZX9F5N0000000000000002",
        ]
    );
}

#[test]
fn rewrite_plan_rejects_empty_backup_id() {
    let plan = RewritePlan::for_targets(
        "   ",
        vec![TargetId::parse("01HQZX9F5N0000000000000000").expect("valid")],
    );

    let err = plan.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField { field: "backup_id" });
}

#[test]
fn rewrite_plan_rejects_empty_target_ids() {
    let plan = RewritePlan::for_targets("bkp_01J00000000000000000000000", Vec::new());

    let err = plan.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField { field: "target_ids" });
}

#[test]
fn shredded_entry_round_trips_json() {
    let entry = ShreddedBackupEntry::new(
        "bkp_01J00000000000000000000000",
        ".cairn/backups/backup-01.tar.zst",
        "op_01J11111111111111111111111",
        Rfc3339Timestamp::parse("2026-05-12T13:00:00Z").expect("valid timestamp"),
    );

    let json = serde_json::to_string(&entry).expect("serialize");
    let back: ShreddedBackupEntry = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, entry);
}

#[test]
fn shredded_entry_rejects_empty_backup_id() {
    let entry = ShreddedBackupEntry::new(
        "   ",
        ".cairn/backups/backup-01.tar.zst",
        "op_01J11111111111111111111111",
        Rfc3339Timestamp::parse("2026-05-12T13:00:00Z").expect("valid timestamp"),
    );

    let err = entry.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField { field: "backup_id" });
}

#[test]
fn shredded_entry_rejects_empty_artifact_path() {
    let entry = ShreddedBackupEntry::new(
        "bkp_01J00000000000000000000000",
        "  ",
        "op_01J11111111111111111111111",
        Rfc3339Timestamp::parse("2026-05-12T13:00:00Z").expect("valid timestamp"),
    );

    let err = entry.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField { field: "artifact_path" });
}

#[test]
fn shredded_entry_rejects_empty_forget_operation_id() {
    let entry = ShreddedBackupEntry::new(
        "bkp_01J00000000000000000000000",
        ".cairn/backups/backup-01.tar.zst",
        "",
        Rfc3339Timestamp::parse("2026-05-12T13:00:00Z").expect("valid timestamp"),
    );

    let err = entry.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField {
        field: "forget_operation_id"
    });
}
