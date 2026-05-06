//! Content-addressed extraction cache helpers.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Extraction cache schema version.
pub const EXTRACTION_CACHE_SCHEMA_VERSION: u32 = 1;

/// Directory under a vault root that stores extraction cache entries.
pub const EXTRACTION_CACHE_DIR: &str = ".cairn/cache";

/// Extracted nodes and edges before they are applied to the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionResult {
    /// Extracted entity or record nodes.
    pub nodes: Vec<serde_json::Value>,
    /// Extracted graph edges.
    pub edges: Vec<serde_json::Value>,
}

/// One content-addressed cache entry persisted as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionCacheEntry {
    /// Cache key for this entry.
    pub key: String,
    /// Entry schema version used for invalidation.
    pub schema_version: u32,
    /// Source path relative to the vault root, using `/` separators.
    pub source_path: String,
    /// Unix timestamp in milliseconds.
    pub extracted_at: u64,
    /// Cached extraction nodes.
    pub nodes: Vec<serde_json::Value>,
    /// Cached extraction edges.
    pub edges: Vec<serde_json::Value>,
    /// Cached node count for quick diagnostics.
    pub entity_count: usize,
    /// Cached edge count for quick diagnostics.
    pub edge_count: usize,
}

impl ExtractionCacheEntry {
    /// Build a cache entry and derive counts from the result payload.
    #[must_use]
    pub fn new(
        key: String,
        source_path: String,
        extracted_at: u64,
        result: ExtractionResult,
    ) -> Self {
        let entity_count = result.nodes.len();
        let edge_count = result.edges.len();
        Self {
            key,
            schema_version: EXTRACTION_CACHE_SCHEMA_VERSION,
            source_path,
            extracted_at,
            nodes: result.nodes,
            edges: result.edges,
            entity_count,
            edge_count,
        }
    }

    /// Convert this entry back into the extraction payload used by callers.
    #[must_use]
    pub fn result(&self) -> ExtractionResult {
        ExtractionResult {
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
        }
    }

    /// Whether this entry can satisfy a lookup for `key`.
    #[must_use]
    pub fn matches_key_and_schema(&self, key: &str) -> bool {
        self.key == key && self.schema_version == EXTRACTION_CACHE_SCHEMA_VERSION
    }
}

/// Errors returned while constructing portable cache keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheKeyError {
    /// An absolute path was not under the provided vault root.
    PathOutsideVault {
        /// Source path supplied for hashing.
        path: PathBuf,
        /// Vault root that the source path must be under.
        vault_root: PathBuf,
    },
    /// The source path had no usable relative components.
    EmptyRelativePath,
    /// Parent-directory components would make the key non-portable.
    ParentComponent {
        /// Source path supplied for hashing.
        path: PathBuf,
    },
    /// Non-Unicode paths are rejected so JSON cache entries stay portable.
    NonUtf8Path {
        /// Source path supplied for hashing.
        path: PathBuf,
    },
}

impl fmt::Display for CacheKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathOutsideVault { path, vault_root } => write!(
                f,
                "path '{}' is outside vault root '{}'",
                path.display(),
                vault_root.display()
            ),
            Self::EmptyRelativePath => f.write_str("cache source path must not be empty"),
            Self::ParentComponent { path } => write!(
                f,
                "cache source path '{}' must not contain parent components",
                path.display()
            ),
            Self::NonUtf8Path { path } => {
                write!(
                    f,
                    "cache source path '{}' must be valid UTF-8",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for CacheKeyError {}

/// Return the markdown body used for cache hashing.
///
/// YAML frontmatter changes do not invalidate markdown extraction because the
/// extraction cache is keyed to the body content.
#[must_use]
pub fn body_for_hashing(content: &str) -> &str {
    if content.starts_with("---\n") {
        content
            .find("\n---\n")
            .map_or(content, |i| &content[i + 5..])
    } else if content.starts_with("---\r\n") {
        content
            .find("\r\n---\r\n")
            .map_or(content, |i| &content[i + 7..])
    } else {
        content
    }
}

/// Return the markdown body bytes used for cache hashing.
#[must_use]
pub fn body_bytes_for_hashing(content: &[u8]) -> &[u8] {
    if content.starts_with(b"---\n") {
        find_bytes(content, b"\n---\n").map_or(content, |i| &content[i + 5..])
    } else if content.starts_with(b"---\r\n") {
        find_bytes(content, b"\r\n---\r\n").map_or(content, |i| &content[i + 7..])
    } else {
        content
    }
}

/// Compute the SHA-256 cache key for file content and a vault-relative path.
///
/// The hash input is `body + NUL + relative_path`, where markdown files hash
/// only their body below YAML frontmatter.
///
/// # Errors
///
/// Returns [`CacheKeyError`] if `path` cannot be made portable relative to
/// `vault_root`.
pub fn cache_key_for_content(
    content: &str,
    path: &Path,
    vault_root: &Path,
) -> Result<String, CacheKeyError> {
    cache_key_for_bytes(content.as_bytes(), path, vault_root)
}

/// Compute the SHA-256 cache key for file bytes and a vault-relative path.
///
/// The hash input is `body + NUL + relative_path`, where markdown files hash
/// only their body below YAML frontmatter. Non-markdown bytes are hashed
/// without UTF-8 decoding so mixed vault folders can include binary sidecars.
///
/// # Errors
///
/// Returns [`CacheKeyError`] if `path` cannot be made portable relative to
/// `vault_root`.
pub fn cache_key_for_bytes(
    content: &[u8],
    path: &Path,
    vault_root: &Path,
) -> Result<String, CacheKeyError> {
    let relative = relative_path_for_cache(path, vault_root)?;
    let body = if is_markdown_path(path) {
        body_bytes_for_hashing(content)
    } else {
        content
    };

    let mut hasher = Sha256::new();
    hasher.update(body);
    hasher.update([0]);
    hasher.update(relative.as_bytes());
    Ok(hex_lower(&hasher.finalize()))
}

/// Return the portable relative path stored in cache entries.
///
/// # Errors
///
/// Returns [`CacheKeyError`] if `path` is outside the vault root or contains
/// non-portable components.
pub fn relative_path_for_cache(path: &Path, vault_root: &Path) -> Result<String, CacheKeyError> {
    let rel = if path.is_absolute() {
        path.strip_prefix(vault_root)
            .map_err(|_| CacheKeyError::PathOutsideVault {
                path: path.to_path_buf(),
                vault_root: vault_root.to_path_buf(),
            })?
    } else {
        path
    };

    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| CacheKeyError::NonUtf8Path {
                    path: path.to_path_buf(),
                })?;
                if !part.is_empty() {
                    parts.push(part);
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(CacheKeyError::ParentComponent {
                    path: path.to_path_buf(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(CacheKeyError::PathOutsideVault {
                    path: path.to_path_buf(),
                    vault_root: vault_root.to_path_buf(),
                });
            }
        }
    }

    if parts.is_empty() {
        return Err(CacheKeyError::EmptyRelativePath);
    }

    Ok(parts.join("/"))
}

/// Return the cache entry path for a key under the vault root.
#[must_use]
pub fn cache_entry_path(vault_root: &Path, key: &str) -> PathBuf {
    vault_root
        .join(EXTRACTION_CACHE_DIR)
        .join(format!("{key}.json"))
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::{Component, Path};

    #[test]
    fn frontmatter_is_excluded_from_markdown_cache_keys() {
        let root = Path::new("/tmp/vault");
        let path = root.join("docs/design.md");

        let first = cache_key_for_content("---\ntitle: X\n---\nbody", &path, root)
            .expect("cache key for first frontmatter variant");
        let second = cache_key_for_content("---\ntitle: Y\n---\nbody", &path, root)
            .expect("cache key for second frontmatter variant");

        assert_eq!(first, second);
    }

    #[test]
    fn crlf_frontmatter_is_excluded_from_markdown_cache_keys() {
        let root = Path::new("/tmp/vault");
        let path = root.join("docs/design.md");

        let first = cache_key_for_content("---\r\ntitle: X\r\n---\r\nbody", &path, root)
            .expect("cache key for first CRLF frontmatter variant");
        let second = cache_key_for_content("---\r\ntitle: Y\r\n---\r\nbody", &path, root)
            .expect("cache key for second CRLF frontmatter variant");

        assert_eq!(first, second);
    }

    #[test]
    fn non_markdown_cache_keys_include_the_whole_body() {
        let root = Path::new("/tmp/vault");
        let path = root.join("notes.txt");

        let first = cache_key_for_content("---\ntitle: X\n---\nbody", &path, root)
            .expect("cache key for first text variant");
        let second = cache_key_for_content("---\ntitle: Y\n---\nbody", &path, root)
            .expect("cache key for second text variant");

        assert_ne!(first, second);
    }

    #[test]
    fn absolute_prefix_changes_do_not_change_cache_keys() {
        let first_root = Path::new("/Users/alice/vault");
        let second_root = Path::new("/home/ci/work/vault");

        let first = cache_key_for_content(
            "body",
            &first_root.join("docs/design/design-brief.md"),
            first_root,
        )
        .expect("cache key under first absolute root");
        let second = cache_key_for_content(
            "body",
            &second_root.join("docs/design/design-brief.md"),
            second_root,
        )
        .expect("cache key under second absolute root");

        assert_eq!(first, second);
    }

    #[test]
    fn cache_entry_counts_match_nodes_and_edges() {
        let entry = ExtractionCacheEntry::new(
            "0".repeat(64),
            "docs/design.md".to_owned(),
            1_714_123_456_789,
            ExtractionResult {
                nodes: vec![
                    serde_json::json!({"id": "n1"}),
                    serde_json::json!({"id": "n2"}),
                ],
                edges: vec![serde_json::json!({"from": "n1", "to": "n2"})],
            },
        );

        assert_eq!(entry.entity_count, 2);
        assert_eq!(entry.edge_count, 1);
        assert_eq!(entry.schema_version, EXTRACTION_CACHE_SCHEMA_VERSION);
    }

    proptest! {
        #[test]
        fn cache_key_is_deterministic(body in ".*", rel in "[a-zA-Z0-9_/.-]{1,80}") {
            let rel = rel.trim_start_matches('/').trim_matches('.');
            let rel_path = Path::new(rel);
            prop_assume!(!rel.is_empty());
            prop_assume!(!rel_path.is_absolute());
            prop_assume!(rel_path.components().any(|c| matches!(c, Component::Normal(_))));
            prop_assume!(rel_path
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)));

            let root = Path::new("/tmp/vault");
            let path = root.join(rel_path);

            let first = cache_key_for_content(&body, &path, root)
                .expect("first deterministic cache key");
            let second = cache_key_for_content(&body, &path, root)
                .expect("second deterministic cache key");

            prop_assert_eq!(first, second);
        }

        #[test]
        fn cache_key_changes_when_relative_path_changes(body in ".*", rel in "[a-zA-Z0-9_-]{1,40}") {
            let root = Path::new("/tmp/vault");
            let first = root.join(format!("a/{rel}.md"));
            let second = root.join(format!("b/{rel}.md"));

            let first_key = cache_key_for_content(&body, &first, root)
                .expect("cache key for first relative path");
            let second_key = cache_key_for_content(&body, &second, root)
                .expect("cache key for second relative path");

            prop_assert_ne!(first_key, second_key);
        }
    }
}
