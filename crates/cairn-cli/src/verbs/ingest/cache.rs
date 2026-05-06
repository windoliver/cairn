use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub version: u8,
    pub relative_path: String,
    pub cache_key: String,
    pub entities_new: u64,
    pub edges_new: u64,
}

pub fn body_for_cache(path: &Path, body: &str) -> String {
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        return body.to_owned();
    }
    let Some(rest) = body.strip_prefix("---\n") else {
        return body.to_owned();
    };
    let Some(end) = rest.find("\n---\n") else {
        return body.to_owned();
    };
    rest[end + "\n---\n".len()..].to_owned()
}

pub fn cache_key(relative_path: &Path, body_for_hash: &str) -> String {
    let normalized_path = relative_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let mut hasher = Sha256::new();
    hasher.update(body_for_hash.as_bytes());
    hasher.update([0]);
    hasher.update(normalized_path.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn cache_path(cache_root: &Path, key: &str) -> PathBuf {
    cache_root.join(format!("{key}.json"))
}

pub fn read_cache_entry(cache_root: &Path, key: &str) -> std::io::Result<Option<CacheEntry>> {
    let path = cache_path(cache_root, key);
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    serde_json::from_str(&body)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

pub fn write_cache_entry(cache_root: &Path, entry: &CacheEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_root)?;
    let final_path = cache_path(cache_root, &entry.cache_key);
    let tmp_path = cache_root.join(format!("{}.{}.tmp", entry.cache_key, std::process::id()));
    let body = serde_json::to_vec(entry)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(&tmp_path, body)?;
    std::fs::rename(tmp_path, final_path)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn markdown_frontmatter_changes_do_not_affect_body_for_cache() {
        let first = "---\ntitle: First\n---\n# Same\n\nBody\n";
        let second = "---\ntitle: Second\n---\n# Same\n\nBody\n";

        assert_eq!(
            body_for_cache(Path::new("notes/page.md"), first),
            body_for_cache(Path::new("notes/page.md"), second)
        );
    }

    #[test]
    fn markdown_body_changes_cache_key() {
        let path = Path::new("notes/page.md");
        let first = body_for_cache(path, "---\ntitle: Same\n---\n# One\n");
        let second = body_for_cache(path, "---\ntitle: Same\n---\n# Two\n");

        assert_ne!(cache_key(path, &first), cache_key(path, &second));
    }

    #[test]
    fn relative_path_participates_in_cache_key() {
        let body = "same body";

        assert_ne!(
            cache_key(Path::new("one/page.md"), body),
            cache_key(Path::new("two/page.md"), body)
        );
    }

    #[test]
    fn cache_entry_round_trips_through_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let entry = CacheEntry {
            version: 1,
            relative_path: "docs/page.md".to_owned(),
            cache_key: cache_key(Path::new("docs/page.md"), "# Body\n"),
            entities_new: 2,
            edges_new: 3,
        };

        write_cache_entry(dir.path(), &entry).unwrap();
        let read = read_cache_entry(dir.path(), &entry.cache_key).unwrap();

        assert_eq!(read, Some(entry));
    }
}
