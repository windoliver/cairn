//! Shared test helpers for Cairn crates.
//!
//! Only ever pulled in as a `dev-dependency`. `cairn-core` does not depend on
//! this crate — core tests stay pure so the boundary check remains trivially
//! sound.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod store_conformance;

/// Absolute path to the workspace-level `fixtures/` directory.
///
/// Resolves at runtime from `CARGO_MANIFEST_DIR` (this crate's dir) and walks
/// up to the workspace root. Cached after first call.
#[must_use]
// `expect` is appropriate here: a broken project layout (crate not two levels
// below the workspace root) is a programmer error that should panic loudly.
#[allow(clippy::expect_used)]
pub fn fixtures_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // CARGO_MANIFEST_DIR is this crate: <workspace>/crates/cairn-test-fixtures
        // Walk up two levels to the workspace root, then into `fixtures/`.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .ancestors()
            .nth(2)
            .expect("crates/cairn-test-fixtures must be two levels below the workspace root");
        workspace_root.join("fixtures")
    })
    .as_path()
}

/// Absolute path to the versioned P0 fixture directory (`fixtures/v0/`).
#[must_use]
pub fn fixture_v0_dir() -> std::path::PathBuf {
    fixtures_dir().join("v0")
}
