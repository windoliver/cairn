//! WAL helpers for `cairn-store-sqlite`.
//!
//! Each submodule corresponds to one `wal_ops.kind` and provides async
//! functions that drive the §5.6 FSM (ISSUED → PREPARED → COMMITTED / ABORTED)
//! against the `wal_ops` table.

pub mod lint_repair;
