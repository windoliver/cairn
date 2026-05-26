//! Generic cairn-pack/v1 runtime: embed, validate, install harness packs.
//!
//! Pack content lives under `packs/<harness>/` (markdown + JSON, no Rust).
//! This module owns the loader, validator, installer, and verify hooks.

pub mod embed;
pub mod manifest;
pub mod merge;
