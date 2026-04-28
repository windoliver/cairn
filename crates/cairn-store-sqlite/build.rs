//! Compute SHA-256 checksums for each migration at build time. Emits
//! `migration_checksums.rs` in `OUT_DIR` with `&[(name, checksum)]` for
//! `schema/mod.rs` to include via `include!`.

use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let migrations_dir = manifest_dir.join("migrations");
    println!("cargo:rerun-if-changed={}", migrations_dir.display());

    let mut entries: Vec<_> = fs::read_dir(&migrations_dir)
        .expect("migrations dir must exist")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("sql"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut out = String::from("&[\n");
    for entry in &entries {
        println!("cargo:rerun-if-changed={}", entry.path().display());
        let bytes = fs::read(entry.path()).expect("read migration");
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hex = format!("{:x}", hasher.finalize());
        let name = entry.file_name().to_string_lossy().into_owned();
        let line = format!("    (\"{name}\", \"{hex}\"),\n");
        out.push_str(&line);
    }
    out.push(']');

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out_dir.join("migration_checksums.rs"), out).expect("write checksums");
}
