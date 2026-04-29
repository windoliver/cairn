//! `identity_wal` insert helper for [`super::SqliteIdentityRegistry`] (issue #50).
//!
//! Every `IdentityRegistry` mutation must call [`wal_insert`] inside its
//! `SQLite` transaction so that a crash before commit rolls both the WAL row
//! and the record mutation back atomically (spec §3.5 / §5.6).
//!
//! This module is a placeholder; the full implementation lands in C4.
