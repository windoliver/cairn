//! Memory gate sizer smoke test — feeds a synthetic resolved profile with
//! a real fake-binary file, verifies pass/fail thresholds.

use std::io::Write;

use cairn_bench::memory::manifest::ResolvedProfile;
use cairn_bench::memory::sizer::measure;

fn fake_binary(dir: &std::path::Path, size_mb: usize) -> std::path::PathBuf {
    let path = dir.join("fake-bin");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(&vec![0u8; size_mb * 1024 * 1024]).unwrap();
    path
}

#[test]
fn passes_when_total_under_budget() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_binary(dir.path(), 10);

    let resolved = ResolvedProfile {
        name: "synthetic".into(),
        binary: bin.display().to_string(),
        features: vec![],
        assets: vec![],
        budget_mb: 50.0,
    };
    let r = measure(&resolved, dir.path()).unwrap();
    assert!(r.ok, "expected ok; failures: {:?}", r.failures);
    assert!(
        r.total_mb > 9.0 && r.total_mb < 11.0,
        "total_mb={}",
        r.total_mb
    );
}

#[test]
fn fails_when_total_over_budget() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_binary(dir.path(), 10);

    let resolved = ResolvedProfile {
        name: "synthetic".into(),
        binary: bin.display().to_string(),
        features: vec![],
        assets: vec![],
        budget_mb: 5.0,
    };
    let r = measure(&resolved, dir.path()).unwrap();
    assert!(!r.ok);
    assert!(r.failures.iter().any(|x| x.contains("budget")));
}
