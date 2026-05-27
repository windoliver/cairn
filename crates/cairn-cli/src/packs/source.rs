//! Pack file sources for embedded and filesystem-backed cairn-pack/v1 directories.

use std::path::{Component, Path, PathBuf};

use include_dir::Dir;

use crate::packs::manifest::PackError;

/// Read-only source of pack files addressed by pack-relative paths.
pub trait PackSource {
    /// Human-readable source label for diagnostics.
    fn label(&self) -> String;

    /// Return true if `path` exists as a regular file in this source.
    fn has_file(&self, path: &str) -> bool;

    /// Read a pack-relative file into memory.
    fn read_file(&self, path: &str) -> Result<Vec<u8>, PackError>;
}

/// Embedded pack source backed by `include_dir`.
pub struct EmbeddedPackSource {
    label: &'static str,
    dir: &'static Dir<'static>,
}

impl EmbeddedPackSource {
    /// Build an embedded source from a bundled pack directory.
    #[must_use]
    pub const fn new(label: &'static str, dir: &'static Dir<'static>) -> Self {
        Self { label, dir }
    }
}

impl PackSource for EmbeddedPackSource {
    fn label(&self) -> String {
        self.label.to_owned()
    }

    fn has_file(&self, path: &str) -> bool {
        self.dir.get_file(path).is_some()
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, PackError> {
        self.dir
            .get_file(path)
            .map(|file| file.contents().to_vec())
            .ok_or_else(|| PackError::ManifestInvalid {
                reason: format!("pack file `{path}` missing from {}", self.label),
            })
    }
}

/// Filesystem pack source rooted at an author-provided directory.
pub struct FsPackSource {
    root: PathBuf,
}

impl FsPackSource {
    /// Build a filesystem source rooted at `root`.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, PackError> {
        reject_unsafe_pack_path(path)?;
        Ok(self.root.join(path))
    }
}

impl PackSource for FsPackSource {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn has_file(&self, path: &str) -> bool {
        self.resolve(path).is_ok_and(|p| p.is_file())
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, PackError> {
        let resolved = self.resolve(path)?;
        std::fs::read(&resolved).map_err(PackError::Io)
    }
}

fn reject_unsafe_pack_path(path: &str) -> Result<(), PackError> {
    let p = Path::new(path);
    if path.is_empty() || p.is_absolute() {
        return Err(PackError::ManifestInvalid {
            reason: format!("path `{path}` escapes pack root"),
        });
    }
    for component in p.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(PackError::ManifestInvalid {
                    reason: format!("path `{path}` escapes pack root"),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_source_reads_pack_relative_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("pack.json"), br#"{"ok":true}"#).expect("write pack");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        assert!(source.has_file("pack.json"));
        assert_eq!(
            source.read_file("pack.json").expect("read"),
            br#"{"ok":true}"#
        );
        assert_eq!(source.label(), tmp.path().display().to_string());
    }

    #[test]
    fn fs_source_rejects_escaping_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        let err = source
            .read_file("../pack.json")
            .expect_err("escape rejected");
        assert!(
            err.to_string().contains("escapes pack root"),
            "unexpected error: {err}"
        );
    }
}
