//! sqlite-vec extension registration (brief §3.0, §5.1).
//!
//! The `sqlite-vec` crate statically links the vec0 C extension into our
//! binary (via its own `build.rs -DSQLITE_CORE`). We must register it with
//! every `SQLite` connection before migrations run so the `CREATE VIRTUAL TABLE
//! … USING vec0(…)` DDL in migration 0020 can succeed.
//!
//! `rusqlite::ffi::sqlite3_auto_extension` registers a C init callback
//! globally for the process — every connection opened **after** the call
//! automatically runs it. We call it exactly once per process via
//! `std::sync::OnceLock`.
//!
//! # Why `#[allow(unsafe_code)]` in `register_vec0`?
//! The FFI call to `sqlite3_auto_extension` and the transmute needed to
//! coerce `sqlite_vec::sqlite3_vec_init` into the expected extension-init
//! C signature are inherently unsafe operations. There is no safe Rust
//! wrapper in the current sqlite-vec or rusqlite APIs. The unsafety is fully
//! contained in `register_vec0`; all other callers see a function that is
//! safe to call from any context after the first call is a no-op.

use std::sync::OnceLock;

// Process-wide guard: register sqlite-vec exactly once.
static REGISTERED: OnceLock<()> = OnceLock::new();

/// Register the sqlite-vec `vec0` module globally for all future
/// `SQLite` connections in this process.
///
/// Safe to call multiple times — only the first call has any effect.
///
/// # Panics
/// Panics if `sqlite3_auto_extension` returns a non-`SQLITE_OK` error
/// code, which would indicate a bug in the sqlite-vec build linkage.
#[allow(unsafe_code)]
pub fn register_vec0() {
    REGISTERED.get_or_init(|| {
        // SAFETY: sqlite_vec::sqlite3_vec_init is the correct init
        // entrypoint exported by the statically-linked sqlite-vec C
        // library.  Its true C signature is:
        //   int (*)(sqlite3*, char**, const sqlite3_api_routines*)
        // which matches the RawAutoExtension type in rusqlite.  The
        // transmute is required because the Rust crate declares it as
        // `fn()` (no parameters) to satisfy the `extern "C"` link
        // constraint without exposing sqlite internals.  This pattern
        // is identical to the example in the sqlite-vec crate's own
        // test suite.
        let rc = unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(
                #[allow(clippy::transmute_ptr_to_ptr)]
                std::mem::transmute::<
                    *const (),
                    unsafe extern "C" fn(
                        *mut rusqlite::ffi::sqlite3,
                        *mut *mut std::os::raw::c_char,
                        *const rusqlite::ffi::sqlite3_api_routines,
                    ) -> std::os::raw::c_int,
                >(sqlite_vec::sqlite3_vec_init as *const ()),
            ))
        };
        assert_eq!(
            rc,
            rusqlite::ffi::SQLITE_OK,
            "sqlite3_auto_extension(sqlite_vec) failed: rc={rc}"
        );
    });
}
