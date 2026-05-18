//! Tiered `DreamWorkflow` support (brief §10.1, §10.2).
//!
//! See [`handler::DreamHandler`] for the entry point and scope notes.

pub mod handler;
pub mod payload;
pub mod trigger;

pub use handler::{DREAM_KIND, DreamHandler, render_dream_prompt};
pub use payload::DreamPayload;
pub use trigger::{DreamEnqueueDecision, enqueue_tier, enqueue_tier_with_dedupe_token};
