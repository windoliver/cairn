//! Folder aggregation + `_index.md` projection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::contract::memory_store::StoredRecord;
use crate::domain::folder::links::Backlink;
use crate::domain::folder::policy::{EffectivePolicy, FolderPolicy, resolve_policy};
use crate::domain::record::RecordId;
use crate::domain::{MemoryKind, Rfc3339Timestamp};

/// Per-record summary line for a folder index.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordEntry {
    /// Vault-relative path of the record file.
    pub path: PathBuf,
    /// Record id.
    pub id: RecordId,
    /// Memory kind.
    pub kind: MemoryKind,
    /// Last-update timestamp from the stored record.
    pub updated_at: Rfc3339Timestamp,
    /// Backlinks pointing at this record.
    pub backlink_count: u32,
}

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

/// Aggregated state for one folder, ready to project as `_index.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderState {
    /// Vault-relative folder path.
    pub path: PathBuf,
    /// Records living directly in this folder, sorted by kind then id.
    pub records: Vec<StoredRecord>,
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
    let mut direct: BTreeMap<PathBuf, Vec<&StoredRecord>> = BTreeMap::new();

    for (path, stored) in &paired {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from(""), Path::to_path_buf);
        direct.entry(parent.clone()).or_default().push(*stored);

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
            // Lexical comparison is correct for UTC timestamps (all `Z`-form);
            // callers that ingest mixed-offset timestamps must normalise to UTC
            // before calling this function.
            if stored.record.updated_at.as_str() > entry.as_str() {
                *entry = stored.record.updated_at.clone();
            }
            cur = d.parent();
        }
    }

    // 3. For every folder in `subtree_count`, build a FolderState.
    let mut states: Vec<FolderState> = Vec::new();
    for folder in subtree_count.keys() {
        let direct_records: Vec<StoredRecord> = direct
            .get(folder)
            .map(|v| v.iter().map(|r| (*r).clone()).collect())
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
    let updated_at = state
        .records
        .iter()
        .map(|r| r.record.updated_at.as_str())
        .chain(
            state
                .subfolders
                .iter()
                .filter_map(|s| s.last_updated.as_ref().map(Rfc3339Timestamp::as_str)),
        )
        .max()
        .unwrap_or("1970-01-01T00:00:00Z")
        .to_owned();

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
        for s in &state.records {
            // path = folder / "<kind>_<id>.md" — same as MarkdownProjector.
            let leaf = format!("{}_{}.md", s.record.kind.as_str(), s.record.id.as_str());
            let _ = writeln!(
                body,
                "- [{leaf}]({leaf}) — {kind} · updated {upd}",
                kind = s.record.kind.as_str(),
                upd = s.record.updated_at.as_str(),
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
            let p = bl.source_path.to_string_lossy();
            let _ = writeln!(body, "- [{p}]({p})");
        }
    }

    ProjectedFile {
        path: state.path.join("_index.md"),
        content: format!("{frontmatter}{body}"),
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

    use crate::domain::record::tests::sample_stored_record;

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
}
