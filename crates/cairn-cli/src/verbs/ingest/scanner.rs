use std::path::{Path, PathBuf};

use super::patterns::{matches_any, GlobPattern};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    pub absolute_path: PathBuf,
    pub relative_path: PathBuf,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanWarnings {
    pub broken_symlinks: u64,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub entries: Vec<ScanEntry>,
    pub skipped: u64,
    pub warnings: ScanWarnings,
}

pub fn scan_folder(
    root: &Path,
    recursive: bool,
    include: &[GlobPattern],
    exclude: &[GlobPattern],
) -> std::io::Result<ScanResult> {
    let mut result = ScanResult::default();
    scan_dir(
        root,
        Path::new(""),
        recursive,
        include,
        exclude,
        &mut result,
    )?;
    result
        .entries
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(result)
}

fn scan_dir(
    root: &Path,
    relative_dir: &Path,
    recursive: bool,
    include: &[GlobPattern],
    exclude: &[GlobPattern],
    result: &mut ScanResult,
) -> std::io::Result<()> {
    let absolute_dir = root.join(relative_dir);
    for entry in std::fs::read_dir(&absolute_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let relative_path = relative_dir.join(file_name);
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                result.warnings.broken_symlinks += 1;
                continue;
            }
            Err(err) => return Err(err),
        };
        let file_type = metadata.file_type();
        let is_dir = file_type.is_dir();
        if matches_any(exclude, &relative_path, is_dir) {
            result.skipped += 1;
            continue;
        }
        if file_type.is_symlink() {
            let target_metadata = match std::fs::metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    result.warnings.broken_symlinks += 1;
                    continue;
                }
                Err(err) => return Err(err),
            };
            if target_metadata.is_file() && matches_any(include, &relative_path, false) {
                result.entries.push(ScanEntry {
                    absolute_path: entry.path(),
                    relative_path,
                });
            } else {
                result.skipped += 1;
            }
            continue;
        }
        if is_dir {
            if recursive {
                scan_dir(root, &relative_path, recursive, include, exclude, result)?;
            } else {
                result.skipped += 1;
            }
        } else if metadata.is_file() && matches_any(include, &relative_path, false) {
            result.entries.push(ScanEntry {
                absolute_path: entry.path(),
                relative_path,
            });
        } else {
            result.skipped += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::ingest::patterns::parse_pattern_list;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("fixture path has parent")).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn rels(result: &ScanResult) -> Vec<String> {
        result
            .entries
            .iter()
            .map(|entry| entry.relative_path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn scans_recursively_with_default_includes_and_excludes() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("docs/a.md"), "# A");
        write(&dir.path().join("src/lib.rs"), "fn main() {}");
        write(&dir.path().join("target/generated.rs"), "fn generated() {}");
        write(&dir.path().join("image.png"), "not text");
        let include = parse_pattern_list(None, &["*.md", "*.rs"]).unwrap();
        let exclude = parse_pattern_list(None, &["target"]).unwrap();

        let result = scan_folder(dir.path(), true, &include, &exclude).unwrap();

        assert_eq!(rels(&result), vec!["docs/a.md", "src/lib.rs"]);
        assert!(result.skipped >= 2);
    }

    #[test]
    fn non_recursive_scan_ignores_child_files() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("root.md"), "# Root");
        write(&dir.path().join("nested/child.md"), "# Child");
        let include = parse_pattern_list(None, &["*.md"]).unwrap();
        let exclude = parse_pattern_list(None, &[]).unwrap();

        let result = scan_folder(dir.path(), false, &include, &exclude).unwrap();

        assert_eq!(rels(&result), vec!["root.md"]);
    }

    #[test]
    fn includes_hidden_files_when_pattern_matches() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".agent.md"), "# Agent");
        let include = parse_pattern_list(None, &["*.md"]).unwrap();
        let exclude = parse_pattern_list(None, &[]).unwrap();

        let result = scan_folder(dir.path(), true, &include, &exclude).unwrap();

        assert_eq!(rels(&result), vec![".agent.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn processes_symlinked_files_but_not_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("actual.md"), "# Actual");
        write(&dir.path().join("linked_dir/child.md"), "# Child");
        symlink(dir.path().join("actual.md"), dir.path().join("alias.md")).unwrap();
        symlink(dir.path().join("linked_dir"), dir.path().join("alias_dir")).unwrap();
        let include = parse_pattern_list(None, &["*.md"]).unwrap();
        let exclude = parse_pattern_list(None, &[]).unwrap();

        let result = scan_folder(dir.path(), true, &include, &exclude).unwrap();

        assert_eq!(
            rels(&result),
            vec!["actual.md", "alias.md", "linked_dir/child.md"]
        );
    }
}
