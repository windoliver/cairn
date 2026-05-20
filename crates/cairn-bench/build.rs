//! build.rs — emit `CAIRN_BIN_PATH` so bench targets can find the `cairn` binary
// at runtime. Cargo's CARGO_BIN_EXE_* is only set for integration tests, not
// bench targets. This build script derives the expected binary path from the
// cargo target directory and profile.
//
// Strategy:
// 1. Use CARGO_BIN_EXE_cairn if Cargo happens to set it (future-proofing).
// 2. Otherwise derive: <OUT_DIR>/../../.. is the profile dir; cairn binary is
//    profile_dir/cairn (Unix) or profile_dir/cairn.exe (Windows).
//
// This works because `cargo bench` builds all dev-dep binaries into the same
// profile directory before running the bench binary.

fn main() {
    // Cargo sets CARGO_BIN_EXE_cairn in build-script env when the binary is in
    // the package's own [[bin]] targets (not for dev-deps). Use it if present.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_cairn") {
        println!("cargo:rustc-env=CAIRN_BIN_PATH={path}");
        println!("cargo:rerun-if-env-changed=CARGO_BIN_EXE_cairn");
        return;
    }

    // Derive the path from OUT_DIR. Cargo sets OUT_DIR to
    // <workspace_target>/<profile>/build/<pkg>-<hash>/out .
    // Walking three parents up gives the profile directory.
    let out_dir = std::env::var("OUT_DIR").expect("Cargo sets OUT_DIR");
    // OUT_DIR = <workspace>/target/<profile>/build/<pkg>-<hash>/out
    // Walk up 3 parents to reach the profile directory.
    let profile_dir = std::path::Path::new(&out_dir)
        .parent() // <pkg>-<hash>
        .and_then(|p| p.parent()) // build
        .and_then(|p| p.parent()) // <profile>  (e.g. "release" or "debug")
        .expect("could not navigate from OUT_DIR to profile dir");

    let bin_name = if cfg!(windows) { "cairn.exe" } else { "cairn" };
    let cairn_path = profile_dir.join(bin_name);

    // Emit the path even if the binary doesn't exist yet — it will be built
    // alongside the bench binary by `cargo bench`.
    println!("cargo:rustc-env=CAIRN_BIN_PATH={}", cairn_path.display());
    println!("cargo:rerun-if-env-changed=CARGO_BIN_EXE_cairn");
    println!("cargo:rerun-if-changed=build.rs");
}
