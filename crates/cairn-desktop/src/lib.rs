//! Desktop GUI backend for Cairn.

pub mod error;
pub mod fixture;
pub mod model;
pub mod repository;
pub mod server;

/// Minimal exported backend marker used while the alpha backend is built out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopBackend;
