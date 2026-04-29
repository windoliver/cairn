//! Folder aggregation + `_index.md` projection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::contract::memory_store::StoredRecord;
use crate::domain::Rfc3339Timestamp;
use crate::domain::folder::links::Backlink;
use crate::domain::folder::policy::{EffectivePolicy, FolderPolicy, resolve_policy};
use crate::domain::record::RecordId;

/// Per-subfolder aggregate row.
#[derive(Debug, Clone, PartialEq)]
pub struct SubfolderEntry {
    /// Subfolder name (basename, no trailing slash).
    pub name: String,
    /// Number of records inside the subtree.
    pub record_count: u32,
    /// Latest `updated_at` across the subtree.
    pub last_updated: Option<Rfc3339Timestamp>,
}

/// One direct record under a folder, paired with its caller-supplied
/// vault-relative path.  Carrying the path explicitly avoids reconstructing
/// `<kind>_<id>.md` inside [`project_index`], which would silently emit
/// broken links if the projector adopts a different naming scheme (e.g.
/// `raw/a/x.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordEntry {
    /// Vault-relative path of the projected markdown file for this record.
    pub path: PathBuf,
    /// The full stored record.
    pub record: StoredRecord,
}

/// Aggregated state for one folder, ready to project as `_index.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderState {
    /// Vault-relative folder path.
    pub path: PathBuf,
    /// Records living directly in this folder, sorted by kind then id.
    pub records: Vec<RecordEntry>,
    /// Subfolders, sorted by name.
    pub subfolders: Vec<SubfolderEntry>,
    /// Backlinks targeting any record in this folder, sorted by source path.
    pub backlinks: Vec<Backlink>,
    /// Resolved effective policy at this folder.
    pub effective_policy: EffectivePolicy,
}

/// Group records by their parent folder, walk up to compute subfolder
/// aggregates, attach backlinks targeting records in each folder, and
/// resolve the effective policy at each folder. Returns one
/// [`FolderState`] per folder that is non-empty (has at least one record
/// in its subtree).
#[must_use]
pub fn aggregate_folders(
    records: &[StoredRecord],
    record_paths: &BTreeMap<RecordId, PathBuf>,
    policies_by_dir: &BTreeMap<PathBuf, FolderPolicy>,
    backlinks_by_target: &BTreeMap<PathBuf, Vec<Backlink>>,
) -> Vec<FolderState> {
    // 1. Pair records with their resolved paths.
    let mut paired: Vec<(PathBuf, &StoredRecord)> = Vec::new();
    for stored in records {
        let Some(p) = record_paths.get(&stored.record.id) else {
            continue;
        };
        paired.push((p.clone(), stored));
    }
    paired.sort_by(|a, b| {
        a.1.record
            .kind
            .as_str()
            .cmp(b.1.record.kind.as_str())
            .then_with(|| a.1.record.id.as_str().cmp(b.1.record.id.as_str()))
    });

    // 2. Collect every folder path that has either a direct record OR a
    //    descendant with a record. Record per-subtree counts.
    let mut subtree_count: BTreeMap<PathBuf, u32> = BTreeMap::new();
    let mut subtree_last_update: BTreeMap<PathBuf, Rfc3339Timestamp> = BTreeMap::new();
    let mut direct: BTreeMap<PathBuf, Vec<(PathBuf, &StoredRecord)>> = BTreeMap::new();

    for (path, stored) in &paired {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from(""), Path::to_path_buf);
        direct
            .entry(parent.clone())
            .or_default()
            .push((path.clone(), *stored));

        // Walk every ancestor (parent inclusive) and bump counts.
        let mut cur: Option<&Path> = Some(&parent);
        while let Some(d) = cur {
            if d.as_os_str().is_empty() {
                break;
            }
            *subtree_count.entry(d.to_path_buf()).or_insert(0) += 1;
            let entry = subtree_last_update
                .entry(d.to_path_buf())
                .or_insert_with(|| stored.record.updated_at.clone());
            // Chronological comparison: lexical comparison of the raw string
            // disagrees with chronological order once offsets are involved
            // (e.g. `+02:00` happens before `Z` for the same wall-clock
            // hour).  `Rfc3339Timestamp::cmp_chronological` parses both
            // sides and applies the offset.
            if stored.record.updated_at.cmp_chronological(entry).is_gt() {
                *entry = stored.record.updated_at.clone();
            }
            cur = d.parent();
        }
    }

    // 3. For every folder in `subtree_count`, build a FolderState.
    let mut states: Vec<FolderState> = Vec::new();
    for folder in subtree_count.keys() {
        let direct_records: Vec<RecordEntry> = direct
            .get(folder)
            .map(|v| {
                v.iter()
                    .map(|(p, r)| RecordEntry {
                        path: p.clone(),
                        record: (*r).clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Subfolders: any path in `subtree_count` whose parent equals `folder`.
        let mut subfolders: Vec<SubfolderEntry> = subtree_count
            .iter()
            .filter_map(|(p, c)| {
                if p.parent() == Some(folder) {
                    Some(SubfolderEntry {
                        name: p
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        record_count: *c,
                        last_updated: subtree_last_update.get(p).cloned(),
                    })
                } else {
                    None
                }
            })
            .collect();
        subfolders.sort_by(|a, b| a.name.cmp(&b.name));

        // Backlinks: any entry in backlinks_by_target whose target lives in this folder.
        let mut blinks: Vec<Backlink> = backlinks_by_target
            .iter()
            .filter(|(target, _)| target.parent() == Some(folder))
            .flat_map(|(_, v)| v.iter().cloned())
            .collect();
        blinks.sort_by(|a, b| a.source_path.cmp(&b.source_path));

        // Effective policy: walk up from a synthetic file inside this folder.
        let synthetic = folder.join("_dummy.md");
        let effective_policy = resolve_policy(&synthetic, policies_by_dir);

        states.push(FolderState {
            path: folder.clone(),
            records: direct_records,
            subfolders,
            backlinks: blinks,
            effective_policy,
        });
    }
    states.sort_by(|a, b| a.path.cmp(&b.path));
    states
}

use crate::domain::projection::ProjectedFile;

/// Project a [`FolderState`] to a `_index.md` file. Caller is responsible
/// for ensuring deterministic sort order on `state.records`,
/// `state.subfolders`, and `state.backlinks`.
#[must_use]
pub fn project_index(state: &FolderState) -> ProjectedFile {
    use std::fmt::Write as _;

    let folder_str = state.path.to_string_lossy();
    // Chronological max: `Rfc3339Timestamp` deliberately doesn't implement
    // `Ord` because lexical order disagrees with wall-clock order across
    // timezone offsets.  Reduce by `cmp_chronological` instead.
    let updated_at = state
        .records
        .iter()
        .map(|r| &r.record.record.updated_at)
        .chain(
            state
                .subfolders
                .iter()
                .filter_map(|s| s.last_updated.as_ref()),
        )
        .max_by(|a, b| a.cmp_chronological(b))
        .map_or_else(
            || "1970-01-01T00:00:00Z".to_owned(),
            |t| t.as_str().to_owned(),
        );

    let mut frontmatter = String::new();
    frontmatter.push_str("---\n");
    // Writing to a String is infallible; discard the Ok(()) result.
    let _ = writeln!(frontmatter, "folder: {folder_str}");
    frontmatter.push_str("kind: folder_index\n");
    let _ = writeln!(frontmatter, "updated_at: {updated_at}");
    let _ = writeln!(frontmatter, "record_count: {}", state.records.len());
    let _ = writeln!(frontmatter, "subfolder_count: {}", state.subfolders.len());
    if let Some(purpose) = &state.effective_policy.purpose {
        // Quote with serde_yaml to avoid breaking on `:` / leading whitespace.
        let yaml_val = serde_yaml::Value::String(purpose.clone());
        let s = serde_yaml::to_string(&yaml_val)
            .ok()
            .and_then(|s| s.strip_prefix("---\n").map(str::to_owned).or(Some(s)))
            .unwrap_or_else(|| purpose.clone());
        let s = s.trim_end_matches('\n');
        let _ = writeln!(frontmatter, "purpose: {s}");
    }
    frontmatter.push_str("---\n\n");

    let mut body = String::new();
    let _ = writeln!(body, "# {folder_str}");

    if !state.records.is_empty() {
        let _ = write!(body, "\n## Records ({})\n", state.records.len());
        for entry in &state.records {
            // Render the leaf relative to the folder containing the index,
            // and resolve backlink counts against the record's actual
            // vault-relative path.  Any reconstruction would diverge if the
            // projector chooses a different naming scheme.
            let leaf = relativize(&state.path, &entry.path);
            let leaf_str = leaf.to_string_lossy();
            let backlink_count = state
                .backlinks
                .iter()
                .filter(|bl| bl.target_path == entry.path)
                .count();
            let _ = writeln!(
                body,
                "- [{leaf_str}]({leaf_str}) — {kind} · updated {upd} · {backlink_count} backlinks",
                kind = entry.record.record.kind.as_str(),
                upd = entry.record.record.updated_at.as_str(),
            );
        }
    }

    if !state.subfolders.is_empty() {
        let _ = write!(body, "\n## Subfolders ({})\n", state.subfolders.len());
        for sf in &state.subfolders {
            let upd = sf
                .last_updated
                .as_ref()
                .map(|t| format!(" · last updated {}", t.as_str()))
                .unwrap_or_default();
            let _ = writeln!(
                body,
                "- [{name}/]({name}/) — {n} records{upd}",
                name = sf.name,
                n = sf.record_count,
            );
        }
    }

    if !state.backlinks.is_empty() {
        let _ = write!(
            body,
            "\n## Backlinks into this folder ({})\n",
            state.backlinks.len(),
        );
        for bl in &state.backlinks {
            let label = bl.source_path.to_string_lossy();
            // Render the href relative to the folder containing this _index.md
            // so a Markdown viewer resolves it correctly. Vault-relative paths
            // would double-prefix (e.g. `raw/_index.md` → `raw/raw/foo.md`).
            let href_rel = relativize(&state.path, &bl.source_path);
            let href = href_rel.to_string_lossy();
            let _ = writeln!(body, "- [{label}]({href})");
        }
    }

    ProjectedFile {
        path: state.path.join("_index.md"),
        content: format!("{frontmatter}{body}"),
    }
}

/// Render `target` relative to `base`. Walks the common-prefix split,
/// emitting `..` for each base segment past the common ancestor and the
/// remaining target segments. Both inputs are expected to be vault-relative
/// (no leading `/`). If `target` is fully inside `base`, the result is the
/// suffix; otherwise we emit `..` segments.
fn relativize(base: &Path, target: &Path) -> PathBuf {
    use std::path::Component;
    let base_segs: Vec<_> = base
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            _ => None,
        })
        .collect();
    let target_segs: Vec<_> = target
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            _ => None,
        })
        .collect();
    let common = base_segs
        .iter()
        .zip(target_segs.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for _ in common..base_segs.len() {
        out.push(std::ffi::OsString::from(".."));
    }
    for s in &target_segs[common..] {
        out.push(s.clone());
    }
    if out.is_empty() {
        PathBuf::from(".")
    } else {
        out.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_state_compiles_with_default_policy() {
        let _ = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
    }

    use crate::domain::record::tests_export::sample_stored_record;

    fn fixture_record(suffix_id: &str) -> StoredRecord {
        // RecordId is constructed via `parse` — the suffix must be valid
        // Crockford base32 chars and total length must be exactly 26.
        // Base: "01HQZX9F5N" (10 chars) + "0000000000" (10 zeros) + 6-char suffix = 26 chars.
        let mut s = sample_stored_record(1);
        s.record.id = RecordId::parse(format!("01HQZX9F5N0000000000{suffix_id:0>6}"))
            .expect("valid record id");
        s
    }

    #[test]
    fn aggregate_single_record_yields_one_folder() {
        let r = fixture_record("000A");
        let mut paths = BTreeMap::new();
        paths.insert(r.record.id.clone(), PathBuf::from("raw/x.md"));
        let states = aggregate_folders(&[r], &paths, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].path, PathBuf::from("raw"));
        assert_eq!(states[0].records.len(), 1);
        assert!(states[0].subfolders.is_empty());
    }

    #[test]
    fn aggregate_nested_propagates_subfolder_counts() {
        let r1 = fixture_record("000001");
        let r2 = fixture_record("000002");
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/a/x.md"));
        paths.insert(r2.record.id.clone(), PathBuf::from("raw/a/b/y.md"));
        let states = aggregate_folders(&[r1, r2], &paths, &BTreeMap::new(), &BTreeMap::new());
        let by_path: BTreeMap<PathBuf, &FolderState> =
            states.iter().map(|s| (s.path.clone(), s)).collect();
        let raw = by_path.get(Path::new("raw")).expect("raw");
        let raw_a = by_path.get(Path::new("raw/a")).expect("raw/a");
        let raw_a_b = by_path.get(Path::new("raw/a/b")).expect("raw/a/b");
        // raw has one subfolder (a), no direct records.
        assert!(raw.records.is_empty());
        assert_eq!(raw.subfolders.len(), 1);
        assert_eq!(raw.subfolders[0].name, "a");
        assert_eq!(raw.subfolders[0].record_count, 2);
        // raw/a has one direct record + one subfolder (b).
        assert_eq!(raw_a.records.len(), 1);
        assert_eq!(raw_a.subfolders.len(), 1);
        assert_eq!(raw_a.subfolders[0].record_count, 1);
        // raw/a/b has one direct record, no subfolders.
        assert_eq!(raw_a_b.records.len(), 1);
        assert!(raw_a_b.subfolders.is_empty());
    }

    #[test]
    fn aggregate_skips_empty_folders() {
        let r = fixture_record("000001");
        let mut paths = BTreeMap::new();
        paths.insert(r.record.id.clone(), PathBuf::from("raw/x.md"));
        let states = aggregate_folders(&[r], &paths, &BTreeMap::new(), &BTreeMap::new());
        assert!(states.iter().all(|s| s.path != Path::new("wiki")));
    }

    #[test]
    fn aggregate_attaches_backlinks_to_target_folder() {
        let r = fixture_record("000001");
        let mut paths = BTreeMap::new();
        paths.insert(r.record.id.clone(), PathBuf::from("raw/x.md"));
        let mut backlinks = BTreeMap::new();
        backlinks.insert(
            PathBuf::from("raw/x.md"),
            vec![Backlink {
                source_path: PathBuf::from("raw/y.md"),
                target_path: PathBuf::from("raw/x.md"),
                anchor: None,
            }],
        );
        let states = aggregate_folders(&[r], &paths, &BTreeMap::new(), &backlinks);
        let raw = states
            .iter()
            .find(|s| s.path == Path::new("raw"))
            .expect("raw state");
        assert_eq!(raw.backlinks.len(), 1);
    }

    use crate::domain::projection::ProjectedFile;

    #[test]
    fn project_emits_required_frontmatter_fields() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let pf: ProjectedFile = project_index(&state);
        assert_eq!(pf.path, PathBuf::from("raw/_index.md"));
        assert!(pf.content.contains("folder: raw"));
        assert!(pf.content.contains("kind: folder_index"));
        assert!(pf.content.contains("record_count: 0"));
        assert!(pf.content.contains("subfolder_count: 0"));
    }

    #[test]
    fn project_omits_purpose_when_unset() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(!pf.content.contains("purpose:"));
    }

    #[test]
    fn project_includes_purpose_when_set() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy {
                purpose: Some("things".into()),
                ..EffectivePolicy::default()
            },
        };
        let pf = project_index(&state);
        assert!(pf.content.contains("purpose: things"));
    }

    #[test]
    fn project_is_deterministic() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: vec![
                SubfolderEntry {
                    name: "b".into(),
                    record_count: 1,
                    last_updated: None,
                },
                SubfolderEntry {
                    name: "a".into(),
                    record_count: 1,
                    last_updated: None,
                },
            ],
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let mut state = state;
        state.subfolders.sort_by(|a, b| a.name.cmp(&b.name));
        let a = project_index(&state);
        let b = project_index(&state);
        assert_eq!(a.content, b.content);
    }

    #[test]
    fn project_omits_empty_sections() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(!pf.content.contains("## Records"));
        assert!(!pf.content.contains("## Subfolders"));
        assert!(!pf.content.contains("## Backlinks"));
    }

    #[test]
    fn project_records_section_renders_leaf_link_and_backlink_count() {
        use crate::domain::record::tests_export::sample_stored_record;
        let stored = sample_stored_record(1);
        let leaf = format!(
            "{}_{}.md",
            stored.record.kind.as_str(),
            stored.record.id.as_str()
        );
        let target_path = PathBuf::from(format!("raw/{leaf}"));
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: vec![RecordEntry {
                path: target_path.clone(),
                record: stored.clone(),
            }],
            subfolders: Vec::new(),
            backlinks: vec![Backlink {
                source_path: PathBuf::from("raw/other.md"),
                target_path: target_path.clone(),
                anchor: None,
            }],
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        let row = format!("- [{leaf}]({leaf})");
        assert!(
            pf.content.contains(&row),
            "expected leaf-only row link per brief §3.4, got:\n{}",
            pf.content
        );
        assert!(
            pf.content.contains("· 1 backlinks"),
            "expected backlink count, got:\n{}",
            pf.content
        );
    }

    #[test]
    fn project_records_section_uses_actual_record_path_for_nested_leaf() {
        // A record at `raw/a/x.md` (not the `<kind>_<id>.md` reconstructed
        // shape) must render its leaf as `a/x.md` from `raw/_index.md`, and
        // its backlink count must be matched against the actual path.
        use crate::domain::record::tests_export::sample_stored_record;
        let stored = sample_stored_record(1);
        let actual_path = PathBuf::from("raw/a/x.md");
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: vec![RecordEntry {
                path: actual_path.clone(),
                record: stored,
            }],
            subfolders: Vec::new(),
            backlinks: vec![Backlink {
                source_path: PathBuf::from("raw/other.md"),
                target_path: actual_path,
                anchor: None,
            }],
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(
            pf.content.contains("- [a/x.md](a/x.md)"),
            "expected nested leaf 'a/x.md', got:\n{}",
            pf.content
        );
        assert!(
            pf.content.contains("· 1 backlinks"),
            "expected backlink count to match actual record path, got:\n{}",
            pf.content
        );
    }

    #[test]
    fn project_renders_backlink_href_relative_to_folder() {
        // `raw/_index.md` listing a backlink whose source is `raw/other.md`
        // must render the href as `other.md` — NOT `raw/other.md`, which
        // would resolve to `raw/raw/other.md` in a Markdown viewer.
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: vec![Backlink {
                source_path: PathBuf::from("raw/other.md"),
                target_path: PathBuf::from("raw/x.md"),
                anchor: None,
            }],
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(
            pf.content.contains("- [raw/other.md](other.md)"),
            "expected folder-relative href, got:\n{}",
            pf.content
        );
        assert!(
            !pf.content.contains("(raw/other.md)"),
            "vault-relative href should not appear, got:\n{}",
            pf.content
        );
    }

    #[test]
    fn project_renders_backlink_href_for_sibling_subfolder() {
        // `raw/a/_index.md` listing a backlink from `raw/b/foo.md` must
        // render `../b/foo.md`.
        let state = FolderState {
            path: PathBuf::from("raw/a"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: vec![Backlink {
                source_path: PathBuf::from("raw/b/foo.md"),
                target_path: PathBuf::from("raw/a/x.md"),
                anchor: None,
            }],
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(
            pf.content.contains("(../b/foo.md)"),
            "expected sibling-relative href '../b/foo.md', got:\n{}",
            pf.content
        );
    }

    #[test]
    fn project_renders_backlink_href_for_nested_descendant() {
        // `raw/_index.md` listing a backlink from `raw/sub/foo.md` must
        // render `sub/foo.md`.
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: vec![Backlink {
                source_path: PathBuf::from("raw/sub/foo.md"),
                target_path: PathBuf::from("raw/x.md"),
                anchor: None,
            }],
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(
            pf.content.contains("(sub/foo.md)"),
            "expected descendant-relative href 'sub/foo.md', got:\n{}",
            pf.content
        );
    }

    #[test]
    fn project_renders_backlink_href_for_parent_source() {
        // `raw/sub/_index.md` listing a backlink from `raw/parent.md` must
        // render `../parent.md`.
        let state = FolderState {
            path: PathBuf::from("raw/sub"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: vec![Backlink {
                source_path: PathBuf::from("raw/parent.md"),
                target_path: PathBuf::from("raw/sub/x.md"),
                anchor: None,
            }],
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(
            pf.content.contains("(../parent.md)"),
            "expected parent-relative href '../parent.md', got:\n{}",
            pf.content
        );
    }

    #[test]
    fn project_records_section_renders_zero_backlinks_when_state_empty() {
        use crate::domain::record::tests_export::sample_stored_record;
        let stored = sample_stored_record(1);
        let leaf = format!(
            "{}_{}.md",
            stored.record.kind.as_str(),
            stored.record.id.as_str()
        );
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: vec![RecordEntry {
                path: PathBuf::from(format!("raw/{leaf}")),
                record: stored,
            }],
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(
            pf.content.contains("· 0 backlinks"),
            "expected '· 0 backlinks' when state.backlinks is empty, got:\n{}",
            pf.content
        );
    }
}
