//! Body-bearing WAL apply for record upsert and expire operations.

#![allow(
    dead_code,
    missing_docs,
    unused_imports,
    reason = "Task 3 intentionally lands record WAL scaffolding before later tasks wire callers"
)]

pub mod forget;
pub mod payload;

pub(crate) mod expire;
pub(crate) mod locks;
pub(crate) mod ops;
pub(crate) mod recovery;
pub(crate) mod steps;
pub(crate) mod upsert;

pub(crate) use expire::apply_expire;
pub use recovery::RecordWalRegistry;
pub(crate) use upsert::apply_upsert;
