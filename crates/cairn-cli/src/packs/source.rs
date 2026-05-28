//! Pack file sources for embedded and filesystem-backed cairn-pack/v1 directories.

use std::path::{Component, Path, PathBuf};

use include_dir::Dir;

use crate::packs::manifest::PackError;

struct ResolvedPackFile {
    path: PathBuf,
}

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
///
/// This source rejects lexical path escapes and static symlinks in
/// pack-relative paths. It assumes the pack directory is not concurrently
/// mutated while verification or install is reading it; it is not a race-free
/// operating-system sandbox for hostile concurrent filesystem changes.
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

    fn resolve_regular_file(&self, path: &str) -> Result<ResolvedPackFile, PackError> {
        let resolved = self.resolve(path)?;
        let mut current = self.root.clone();
        let components: Vec<_> = Path::new(path).components().collect();

        for (idx, component) in components.iter().enumerate() {
            let Component::Normal(name) = component else {
                unreachable!("reject_unsafe_pack_path rejects non-normal components");
            };
            current.push(name);
            let metadata = std::fs::symlink_metadata(&current).map_err(PackError::Io)?;
            if metadata.file_type().is_symlink() {
                return Err(PackError::ManifestInvalid {
                    reason: format!("path `{path}` contains symlink"),
                });
            }
            if idx + 1 == components.len() {
                if !metadata.is_file() {
                    return Err(PackError::ManifestInvalid {
                        reason: format!("path `{path}` is not a regular file"),
                    });
                }
            } else if !metadata.is_dir() {
                return Err(PackError::ManifestInvalid {
                    reason: format!("path `{path}` parent is not a directory"),
                });
            }
        }

        Ok(ResolvedPackFile { path: resolved })
    }
}

impl PackSource for FsPackSource {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn has_file(&self, path: &str) -> bool {
        self.resolve_regular_file(path).is_ok()
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, PackError> {
        let resolved = self.resolve_regular_file(path)?;
        std::fs::read(&resolved.path).map_err(PackError::Io)
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

    #[cfg(unix)]
    #[test]
    fn fs_source_rejects_symlinked_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::create_dir(tmp.path().join("agents")).expect("create agents dir");
        std::fs::write(outside.path().join("foo.md"), b"outside").expect("write outside file");
        std::os::unix::fs::symlink(
            outside.path().join("foo.md"),
            tmp.path().join("agents/foo.md"),
        )
        .expect("create symlink");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        assert!(!source.has_file("agents/foo.md"));
        let err = source
            .read_file("agents/foo.md")
            .expect_err("symlink rejected");
        assert!(
            err.to_string().contains("symlink"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fs_source_rejects_symlinked_parent_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::write(outside.path().join("foo.md"), b"outside").expect("write outside file");
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("agents"))
            .expect("create symlinked parent");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        assert!(!source.has_file("agents/foo.md"));
        let err = source
            .read_file("agents/foo.md")
            .expect_err("symlinked parent rejected");
        assert!(
            err.to_string().contains("symlink"),
            "unexpected error: {err}"
        );
    }
}
