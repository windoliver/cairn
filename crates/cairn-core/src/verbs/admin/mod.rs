//! `cairn.admin.v1` verb implementations.

pub mod capability;
pub mod connector;
pub mod guard;
pub mod manifest;
pub mod replay_wal;
pub mod restore;
pub mod snapshot;

pub use capability::{
    ADMIN_CAPABILITIES, CAP_CONNECTOR_BACKFILL, CAP_CONNECTOR_DISABLE, CAP_CONNECTOR_ENABLE,
    CAP_REPLAY_WAL, CAP_RESTORE, CAP_SNAPSHOT, ensure_admin_capability,
};
