//! Running codegen 5 times in fresh tempdirs yields byte-equal output trees.
//! Catches accidental hash-iteration leaks (HashMap, HashSet, etc.).

use std::collections::BTreeMap;
use std::path::PathBuf;
use cairn_idl::codegen::{run, RunMode, RunOpts};

fn snapshot_tree(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            let bytes = std::fs::read(&path).unwrap();
            out.insert(rel, bytes);
        }
    }
}

#[test]
fn five_runs_produce_byte_equal_trees() {
    let mut snapshots: Vec<BTreeMap<PathBuf, Vec<u8>>> = Vec::with_capacity(5);
    for _ in 0..5 {
        let tmp = tempfile::tempdir().unwrap();
        run(&RunOpts {
            workspace_root: tmp.path().to_path_buf(),
            mode: RunMode::Write,
        })
        .unwrap();
        snapshots.push(snapshot_tree(tmp.path()));
    }
    let first = &snapshots[0];
    for (i, snap) in snapshots.iter().enumerate().skip(1) {
        assert_eq!(snap, first, "run #{i} differs from run #0");
    }
}
