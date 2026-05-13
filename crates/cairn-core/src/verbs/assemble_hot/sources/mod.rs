//! Source-specific `select` functions for each `HotRecipeStep`. Each
//! one returns a [`super::inclusion::LoadedSegment`] — never errors —
//! so the assembler can compose them deterministically.

pub mod index;
pub mod pinned;
pub mod playbook;
pub mod project;
pub mod purpose;
pub mod render;
pub mod user_signal;
