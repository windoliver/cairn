//! Pure pipeline functions (brief §5.2).
//!
//! Stages between sensor capture and store upsert that operate as
//! pure transformations: no I/O, no shared state. Squash is the
//! tool-output compactor (issue #72); future siblings include
//! filter, classify, and rank as those issues land.

pub mod squash;
