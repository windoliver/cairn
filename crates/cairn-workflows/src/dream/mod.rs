//! Minimum-path `DreamWorkflow` (issue #91, brief §10.1, §10.2).
//!
//! See [`handler::DreamHandler`] for the entry point and scope notes.

pub mod handler;
pub mod payload;

pub use handler::{DREAM_KIND, DreamHandler, render_dream_prompt};
pub use payload::DreamPayload;
