//! Generic cairn-pack/v1 runtime: embed, validate, install harness packs.
//!
//! Pack content lives under `packs/<harness>/` (markdown + JSON, no Rust).
//! This module owns the loader, validator, installer, and verify hooks.

pub mod embed;
pub mod install;
pub mod manifest;
pub mod merge;
pub mod source;
pub mod template;
pub mod verify;

use include_dir::Dir;

use self::manifest::Harness;

/// Return the embedded pack content for the given harness.
#[must_use]
pub fn bundled_pack_for(harness: Harness) -> Option<&'static Dir<'static>> {
    match harness {
        Harness::ClaudeCode => Some(&embed::CAIRN_CLAUDE_CODE_PACK),
        Harness::Codex | Harness::Gemini => None,
    }
}
