//! Codegen entry point (P0 scaffold). IDL load + emit land in #34, #35.
//!
//! Fails closed — exits with a not-implemented status so any build script,
//! CI step, or release automation that shells out to `cairn-codegen` cannot
//! silently treat schema generation as complete.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "cairn-codegen: not yet implemented. IDL source and generation land \
         in issues #34 and #35; no files were loaded or emitted."
    );
    ExitCode::from(2)
}
