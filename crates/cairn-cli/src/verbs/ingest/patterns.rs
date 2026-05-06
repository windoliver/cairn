use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobPattern {
    raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    Empty,
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty folder ingest pattern"),
        }
    }
}

impl std::error::Error for PatternError {}

pub fn parse_pattern_list(
    values: Option<Vec<String>>,
    defaults: &[&str],
) -> Result<Vec<GlobPattern>, PatternError> {
    let source: Vec<String> =
        values.unwrap_or_else(|| defaults.iter().map(|s| (*s).to_owned()).collect());
    source
        .into_iter()
        .flat_map(|value| value.split(',').map(str::to_owned).collect::<Vec<_>>())
        .map(|value| {
            let normalized = normalize_pattern(&value);
            if normalized.is_empty() {
                Err(PatternError::Empty)
            } else {
                Ok(GlobPattern { raw: normalized })
            }
        })
        .collect()
}

pub fn matches_any(patterns: &[GlobPattern], relative: &Path, is_dir: bool) -> bool {
    let normalized = normalize_relative(relative);
    patterns
        .iter()
        .any(|pattern| pattern.matches(&normalized, is_dir))
}

fn normalize_relative(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_pattern(value: &str) -> String {
    let mut normalized = value.trim().replace('\\', "/");
    while normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

impl GlobPattern {
    fn matches(&self, normalized: &str, is_dir: bool) -> bool {
        let pattern = self.raw.as_str();
        if let Some(ext) = pattern.strip_prefix("*.") {
            return !is_dir
                && normalized
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.ends_with(&format!(".{ext}")));
        }
        if pattern.contains('/') {
            return normalized == pattern || normalized.starts_with(&format!("{pattern}/"));
        }
        normalized.split('/').any(|segment| segment == pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repeated_and_comma_separated_patterns() {
        let patterns = parse_pattern_list(
            Some(vec!["*.md, *.rs".to_owned(), "docs/*.txt".to_owned()]),
            &[],
        )
        .expect("patterns parse");

        assert_eq!(
            patterns,
            vec![
                GlobPattern {
                    raw: "*.md".to_owned()
                },
                GlobPattern {
                    raw: "*.rs".to_owned()
                },
                GlobPattern {
                    raw: "docs/*.txt".to_owned()
                },
            ]
        );
    }

    #[test]
    fn basename_extension_pattern_matches_files_only() {
        let patterns = parse_pattern_list(Some(vec!["*.md".to_owned()]), &[]).unwrap();

        assert!(matches_any(&patterns, Path::new("notes/readme.md"), false));
        assert!(!matches_any(
            &patterns,
            Path::new("notes/readme.txt"),
            false
        ));
        assert!(!matches_any(&patterns, Path::new("notes.md"), true));
    }

    #[test]
    fn bare_segment_pattern_matches_any_path_segment() {
        let patterns = parse_pattern_list(Some(vec!["target".to_owned()]), &[]).unwrap();

        assert!(matches_any(&patterns, Path::new("target"), true));
        assert!(matches_any(
            &patterns,
            Path::new("crates/core/target/debug"),
            true
        ));
        assert!(!matches_any(&patterns, Path::new("targets/file.rs"), false));
    }

    #[test]
    fn slash_pattern_matches_exact_path() {
        let patterns = parse_pattern_list(Some(vec!["docs/file.md".to_owned()]), &[]).unwrap();

        assert!(matches_any(&patterns, Path::new("docs/file.md"), false));
        assert!(!matches_any(&patterns, Path::new("docs/other.md"), false));
    }

    #[test]
    fn trailing_slash_pattern_matches_directory_prefix() {
        let patterns = parse_pattern_list(Some(vec!["target/".to_owned()]), &[]).unwrap();

        assert!(matches_any(
            &patterns,
            Path::new("target/generated.rs"),
            false
        ));
        assert!(!matches_any(
            &patterns,
            Path::new("targets/generated.rs"),
            false
        ));
    }

    #[test]
    fn windows_style_pattern_matches_normalized_path() {
        let patterns = parse_pattern_list(Some(vec!["foo\\bar".to_owned()]), &[]).unwrap();

        assert!(matches_any(&patterns, Path::new("foo/bar"), false));
    }
}
