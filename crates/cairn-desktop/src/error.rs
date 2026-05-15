//! Error types for the desktop GUI backend.

/// Result alias for desktop backend operations.
pub type DesktopResult<T> = Result<T, DesktopError>;

/// Errors produced by the desktop GUI backend.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// Fixture data could not be loaded or parsed.
    #[error("desktop fixture error: {message}")]
    Fixture {
        /// Human-readable fixture failure.
        message: String,
    },
}
