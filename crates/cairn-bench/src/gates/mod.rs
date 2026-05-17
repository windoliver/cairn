//! Shared infrastructure for the release-gate subcommands.

pub mod baseline;
pub mod report;
pub mod thresholds;

/// Returns the runner profile name derived from `$RUNNER_OS` (CI) or `cfg!(target_os)` (local).
#[must_use]
pub fn runner_profile() -> &'static str {
    match std::env::var("RUNNER_OS").as_deref() {
        Ok("Linux") => "linux",
        Ok("macOS") => "macos",
        Ok("Windows") => "windows",
        _ if cfg!(target_os = "linux") => "linux",
        _ if cfg!(target_os = "macos") => "macos",
        _ => "linux",
    }
}
