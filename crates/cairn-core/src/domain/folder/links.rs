//! Markdown link extraction and reverse-map materialization.

use std::path::{Path, PathBuf};

/// A link extracted from a record body, target resolved against the source folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLink {
    /// Vault-relative target path.
    pub target_path: PathBuf,
    /// Optional fragment after `#`, if present.
    pub anchor: Option<String>,
}

/// A reverse link: source file pointing at a target file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    /// File containing the link (vault-relative).
    pub source_path: PathBuf,
    /// File being linked to (vault-relative).
    pub target_path: PathBuf,
    /// Optional fragment after `#`, if present.
    pub anchor: Option<String>,
}

/// Pure markdown link extractor.
///
/// Recognises:
/// - `[label](path)` — markdown link; label discarded.
/// - `[[target]]` and `[[target#anchor]]` — wiki link.
///
/// Drops:
/// - external URLs (`http://`, `https://`, `mailto:`);
/// - links inside fenced code blocks;
/// - escaped link syntax (`\[`).
///
/// Wiki-style targets without an extension get `.md` appended. Relative
/// paths are resolved against the source file's parent directory.
#[must_use]
pub fn extract_links(source_path: &Path, body: &str) -> Vec<RawLink> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            // Toggle fence on the leading-``` line; skip the fence content.
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        scan_line(source_path, line, &mut out);
    }
    out
}

fn scan_line(source_path: &Path, line: &str, out: &mut Vec<RawLink>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Escaped open-bracket — skip.
        if b == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            continue;
        }
        if b == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Wiki link: [[target]] or [[target#anchor]]
            if let Some(end) = find_close(&bytes[i + 2..], b"]]") {
                let inner = &line[i + 2..i + 2 + end];
                if let Some(link) = parse_wiki(source_path, inner) {
                    out.push(link);
                }
                i += 2 + end + 2;
                continue;
            }
        }
        if b == b'[' {
            // Markdown link: [label](target)
            if let Some(label_end) = find_close(&bytes[i + 1..], b"]") {
                let after = i + 1 + label_end + 1;
                if after < bytes.len()
                    && bytes[after] == b'('
                    && let Some(target_end) = find_close(&bytes[after + 1..], b")")
                {
                    let target = &line[after + 1..after + 1 + target_end];
                    if let Some(link) = parse_markdown(source_path, target) {
                        out.push(link);
                    }
                    i = after + 1 + target_end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
}

fn find_close(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_external(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:")
}

fn parse_wiki(source_path: &Path, raw: &str) -> Option<RawLink> {
    let raw = raw.trim();
    if raw.is_empty() || is_external(raw) {
        return None;
    }
    let (target_str, anchor) = match raw.split_once('#') {
        Some((t, a)) => (t, Some(a.to_owned())),
        None => (raw, None),
    };
    let with_ext: PathBuf = if Path::new(target_str).extension().is_some() {
        PathBuf::from(target_str)
    } else {
        PathBuf::from(format!("{target_str}.md"))
    };
    // Wiki links are always resolved relative to the source file's parent
    // directory (bare names like `[[alice]]` resolve to the same folder).
    Some(RawLink {
        target_path: resolve_source_relative(source_path, &with_ext),
        anchor,
    })
}

fn parse_markdown(source_path: &Path, raw: &str) -> Option<RawLink> {
    let raw = raw.trim();
    if raw.is_empty() || is_external(raw) {
        return None;
    }
    let (target_str, anchor) = match raw.split_once('#') {
        Some((t, a)) => (t, Some(a.to_owned())),
        None => (raw, None),
    };
    // Markdown links: paths starting with `./` or `../` are source-relative;
    // all others are treated as vault-relative already.
    let target = Path::new(target_str);
    let target_path = {
        use std::path::Component;
        let first = target.components().next();
        let is_nav = matches!(first, Some(Component::CurDir | Component::ParentDir));
        if is_nav {
            resolve_source_relative(source_path, target)
        } else {
            normalize(target)
        }
    };
    Some(RawLink {
        target_path,
        anchor,
    })
}

/// Resolve `target` against the parent directory of `source_path`.
fn resolve_source_relative(source_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        return target.components().skip(1).collect();
    }
    let parent = source_path.parent().unwrap_or_else(|| Path::new(""));
    let joined = parent.join(target);
    normalize(&joined)
}

fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in p.components() {
        use std::path::Component;
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(s) => out.push(s.to_os_string()),
        }
    }
    out.iter().collect()
}

use std::collections::BTreeMap;

use crate::contract::memory_store::StoredRecord;
use crate::domain::record::RecordId;

/// Build the reverse map: `target_path` → backlinks pointing at it. Sorted by
/// `source_path` for deterministic output.
#[must_use]
pub fn materialize_backlinks(
    records: &[StoredRecord],
    record_paths: &BTreeMap<RecordId, PathBuf>,
) -> BTreeMap<PathBuf, Vec<Backlink>> {
    let mut by_target: BTreeMap<PathBuf, Vec<Backlink>> = BTreeMap::new();
    for stored in records {
        let Some(source_path) = record_paths.get(&stored.record.id) else {
            continue;
        };
        for raw in extract_links(source_path, &stored.record.body) {
            by_target
                .entry(raw.target_path.clone())
                .or_default()
                .push(Backlink {
                    source_path: source_path.clone(),
                    target_path: raw.target_path,
                    anchor: raw.anchor,
                });
        }
    }
    for entries in by_target.values_mut() {
        entries.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    }
    by_target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_link_label_discarded() {
        let src = Path::new("raw/a.md");
        let links = extract_links(src, "see [Alice](raw/alice.md) for details");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_path, PathBuf::from("raw/alice.md"));
        assert!(links[0].anchor.is_none());
    }

    #[test]
    fn wiki_link_appends_md_extension() {
        let src = Path::new("raw/a.md");
        let links = extract_links(src, "see [[alice]] for details");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_path, PathBuf::from("raw/alice.md"));
    }

    #[test]
    fn wiki_anchor_is_populated() {
        let src = Path::new("raw/a.md");
        let links = extract_links(src, "see [[alice#bio]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].anchor.as_deref(), Some("bio"));
    }

    #[test]
    fn relative_path_resolves_against_source_folder() {
        let src = Path::new("wiki/entities/people/bob.md");
        let links = extract_links(src, "[ref](../alice.md)");
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].target_path,
            PathBuf::from("wiki/entities/alice.md"),
        );
    }

    #[test]
    fn external_urls_are_dropped() {
        let src = Path::new("raw/a.md");
        let body = "see [docs](https://example.com), [home](http://x), [mail](mailto:x@y)";
        let links = extract_links(src, body);
        assert!(links.is_empty());
    }

    #[test]
    fn code_fenced_links_are_ignored() {
        let src = Path::new("raw/a.md");
        let body = "before\n```\n[Alice](raw/alice.md)\n```\nafter [Bob](raw/bob.md)";
        let links = extract_links(src, body);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_path, PathBuf::from("raw/bob.md"));
    }

    #[test]
    fn escaped_open_bracket_is_ignored() {
        let src = Path::new("raw/a.md");
        let body = r"\[not a link\](nope)";
        let links = extract_links(src, body);
        assert!(links.is_empty());
    }

    use crate::contract::memory_store::StoredRecord;
    use crate::domain::record::RecordId;
    use crate::domain::record::tests::sample_stored_record;
    use std::collections::BTreeMap;

    fn record_with_body(version: u32, body: &str) -> StoredRecord {
        let mut s = sample_stored_record(version);
        s.record.body = body.to_owned();
        s
    }

    #[test]
    fn materialize_empty_records_returns_empty_map() {
        let records: Vec<StoredRecord> = Vec::new();
        let paths: BTreeMap<RecordId, PathBuf> = BTreeMap::new();
        let map = materialize_backlinks(&records, &paths);
        assert!(map.is_empty());
    }

    #[test]
    fn materialize_emits_backlink_for_existing_target() {
        let r1 = record_with_body(1, "see [Alice](raw/alice.md)");
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/r1.md"));
        let map = materialize_backlinks(&[r1], &paths);
        let entries = map.get(Path::new("raw/alice.md")).expect("entry");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_path, PathBuf::from("raw/r1.md"));
    }

    #[test]
    fn materialize_includes_dangling_links() {
        let r1 = record_with_body(1, "[ghost](raw/missing.md)");
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/r1.md"));
        let map = materialize_backlinks(&[r1], &paths);
        assert!(map.contains_key(Path::new("raw/missing.md")));
    }

    #[test]
    fn materialize_two_sources_to_same_target_sorted() {
        let r1 = record_with_body(1, "[a](raw/alice.md)");
        let mut r2 = record_with_body(1, "[a](raw/alice.md)");
        // Mutate id so paths differ.
        r2.record.id = RecordId::parse("01HQZX9F5N0000000000000ZZZ".to_owned()).expect("valid");
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/zzz.md"));
        paths.insert(r2.record.id.clone(), PathBuf::from("raw/aaa.md"));
        let map = materialize_backlinks(&[r1, r2], &paths);
        let entries = map.get(Path::new("raw/alice.md")).expect("entry");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source_path, PathBuf::from("raw/aaa.md"));
        assert_eq!(entries[1].source_path, PathBuf::from("raw/zzz.md"));
    }
}
