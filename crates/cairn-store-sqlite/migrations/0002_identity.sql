-- crates/cairn-store-sqlite/migrations/0002_identity.sql
-- Issue #50: identity provisioning. See
--   docs/superpowers/specs/2026-04-27-issue-50-identity-provisioning-design.md §4.4

CREATE TABLE identities (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('human', 'agent', 'sensor')),
    current_key_version INTEGER NOT NULL,
    provisioning_state TEXT NOT NULL CHECK (provisioning_state IN ('pending', 'active', 'revoke_pending', 'revoked', 'purge_pending', 'purged')),
    created_at TEXT NOT NULL,
    activated_at TEXT,
    revoked_at TEXT,
    revocation_signature BLOB,
    purge_requested_at TEXT,
    purged_at TEXT,
    purge_reason TEXT
);

CREATE TABLE identity_keys (
    identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    key_version INTEGER NOT NULL,
    public_key BLOB NOT NULL,
    signed_predecessor BLOB,
    created_at TEXT NOT NULL,
    superseded_at TEXT,
    PRIMARY KEY (identity_id, key_version)
);
CREATE INDEX idx_identity_keys_identity ON identity_keys(identity_id);

CREATE TABLE vault_meta (
    rowid INTEGER PRIMARY KEY CHECK (rowid = 1),
    vault_id TEXT NOT NULL,
    witness_sha256 BLOB NOT NULL,
    binding_path TEXT NOT NULL,
    witness_created_at TEXT NOT NULL
);

-- vault_meta is immutable post-insert: no UPDATE / DELETE allowed.
CREATE TRIGGER vault_meta_no_update
BEFORE UPDATE OF vault_id, witness_sha256, binding_path ON vault_meta
BEGIN
    SELECT RAISE(FAIL, 'vault_meta is immutable');
END;

CREATE TRIGGER vault_meta_no_delete
BEFORE DELETE ON vault_meta
BEGIN
    SELECT RAISE(FAIL, 'vault_meta is immutable');
END;

CREATE TABLE identity_receipts (
    rowid INTEGER PRIMARY KEY,
    op_kind TEXT NOT NULL CHECK (op_kind IN ('rotation', 'revocation')),
    target_identity TEXT NOT NULL,
    signer_identity TEXT NOT NULL,
    signer_key_version INTEGER NOT NULL,
    old_key_version INTEGER,
    new_key_version INTEGER,
    issued_at TEXT NOT NULL,
    signed_payload BLOB NOT NULL,
    signature BLOB NOT NULL,
    pending_eviction INTEGER NOT NULL DEFAULT 0 CHECK (pending_eviction IN (0, 1)),
    pending_key_disable INTEGER NOT NULL DEFAULT 0 CHECK (pending_key_disable IN (0, 1)),
    FOREIGN KEY (signer_identity, signer_key_version)
        REFERENCES identity_keys(identity_id, key_version),
    FOREIGN KEY (target_identity, old_key_version)
        REFERENCES identity_keys(identity_id, key_version),
    FOREIGN KEY (target_identity, new_key_version)
        REFERENCES identity_keys(identity_id, key_version)
);
CREATE INDEX idx_identity_receipts_target ON identity_receipts(target_identity);
CREATE INDEX idx_identity_receipts_signer ON identity_receipts(signer_identity);
CREATE INDEX idx_identity_receipts_pending_eviction ON identity_receipts(pending_eviction) WHERE pending_eviction = 1;
CREATE INDEX idx_identity_receipts_pending_key_disable ON identity_receipts(pending_key_disable) WHERE pending_key_disable = 1;

CREATE TABLE pending_rotations (
    rowid INTEGER PRIMARY KEY,
    identity_id TEXT NOT NULL,
    planned_version INTEGER NOT NULL,
    planned_handle TEXT NOT NULL,
    intended_at TEXT NOT NULL,
    UNIQUE (identity_id, planned_version)
);
CREATE INDEX idx_pending_rotations_identity ON pending_rotations(identity_id);

-- identity_wal records every mutating operation atomically with the
-- record mutation it drives (spec §3.5 / §5.6).
--
-- op_kind includes insert_pending_rotation / delete_pending_rotation to
-- cover the pending_rotations lifecycle (closes round-10 finding #1).
CREATE TABLE identity_wal (
    rowid INTEGER PRIMARY KEY,
    op_id BLOB NOT NULL UNIQUE,
    op_kind TEXT NOT NULL CHECK (op_kind IN (
        'reserve_first_identity', 'reserve_identity', 'activate_identity',
        'delete_pending', 'apply_rotation', 'begin_revocation',
        'finalise_revocation', 'mark_purge_pending', 'finalise_purge',
        'clear_pending_eviction', 'clear_pending_key_disable',
        'insert_pending_rotation', 'delete_pending_rotation')),
    target_identity TEXT NOT NULL,
    request_payload BLOB NOT NULL,
    applied_at TEXT NOT NULL
);
CREATE INDEX idx_identity_wal_target ON identity_wal(target_identity);
