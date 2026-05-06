use std::path::Path;

use regex::Regex;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionCounts {
    pub entities_new: u64,
    pub edges_new: u64,
}

pub fn extract_keyword_counts(relative_path: &Path, body: &str) -> ExtractionCounts {
    match relative_path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => code_counts(
            body,
            &[
                r"(?m)^(?:async[ \t]+)?fn[ \t]+[A-Za-z_][A-Za-z0-9_]*",
                r"(?m)^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?fn[ \t]+[A-Za-z_][A-Za-z0-9_]*",
                r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:struct|enum|trait|mod)[ \t]+[A-Za-z_][A-Za-z0-9_]*",
                r"(?m)^[ \t]*impl(?:[ \t]*<[^>]+>)?[ \t]+(?:[A-Za-z_][A-Za-z0-9_:<>]*(?:[ \t]+for[ \t]+)?)+",
            ],
        ),
        Some("py") => code_counts(
            body,
            &[
                r"(?m)^[ \t]*def[ \t]+[A-Za-z_][A-Za-z0-9_]*[ \t]*\(",
                r"(?m)^[ \t]*class[ \t]+[A-Za-z_][A-Za-z0-9_]*",
            ],
        ),
        Some("ts" | "js") => code_counts(
            body,
            &[
                r"(?m)^[ \t]*(?:export[ \t]+)?(?:async[ \t]+)?function[ \t]+[A-Za-z_$][A-Za-z0-9_$]*[ \t]*\(",
                r"(?m)^[ \t]*(?:export[ \t]+)?(?:class|interface|type)[ \t]+[A-Za-z_$][A-Za-z0-9_$]*",
                r"(?m)^[ \t]*(?:export[ \t]+)?const[ \t]+[A-Za-z_$][A-Za-z0-9_$]*[ \t]*=",
            ],
        ),
        Some("go") => code_counts(
            body,
            &[
                r"(?m)^[ \t]*func[ \t]+(?:\([^)]*\)[ \t]*)?[A-Za-z_][A-Za-z0-9_]*[ \t]*\(",
                r"(?m)^[ \t]*type[ \t]+[A-Za-z_][A-Za-z0-9_]*",
                r"(?m)^[ \t]*package[ \t]+[A-Za-z_][A-Za-z0-9_]*",
            ],
        ),
        Some("md" | "txt" | "rst") => text_counts(body),
        _ => ExtractionCounts::default(),
    }
}

fn code_counts(body: &str, patterns: &[&str]) -> ExtractionCounts {
    ExtractionCounts {
        entities_new: patterns
            .iter()
            .map(|pattern| {
                Regex::new(pattern)
                    .expect("valid extraction regex")
                    .find_iter(body)
                    .count() as u64
            })
            .sum(),
        edges_new: 0,
    }
}

fn text_counts(body: &str) -> ExtractionCounts {
    let heading_count = count_matches(r"(?m)^[ \t]{0,3}#{1,6}[ \t]+[^ \t\r\n].*$", body);
    let wiki_link_count = count_matches(r"\[\[[^\]\n]+\]\]", body);
    let title_case_phrase_count =
        count_matches(r"(?:^|[^A-Za-z])[A-Z][a-z]+(?:[ \t]+[A-Z][a-z]+)+", body);
    let marker_count = count_matches(r"(?:^|[^A-Z])(?:TODO|FIXME)(?:[^A-Z]|$)", body);

    ExtractionCounts {
        entities_new: heading_count + wiki_link_count + title_case_phrase_count + marker_count,
        edges_new: wiki_link_count,
    }
}

fn count_matches(pattern: &str, body: &str) -> u64 {
    Regex::new(pattern)
        .expect("valid extraction regex")
        .find_iter(body)
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_extracts_headings_wiki_links_phrases_and_markers() {
        let body =
            "# Cairn Guide\nSee [[Memory Store]] for San Francisco context.\nTODO: wire this.\n";

        let counts = extract_keyword_counts(Path::new("guide.md"), body);

        assert!(counts.entities_new >= 4);
        assert_eq!(counts.edges_new, 1);
    }

    #[test]
    fn rust_extracts_structural_declarations() {
        let body = r"
pub struct Folder;
pub enum Entry {
    File,
}
pub fn ingest_folder() {}
impl Folder {
    fn path(&self) {}
}
";

        let counts = extract_keyword_counts(Path::new("src/folder.rs"), body);

        assert_eq!(counts.entities_new, 4);
        assert_eq!(counts.edges_new, 0);
    }

    #[test]
    fn rust_extracts_indented_structural_declarations() {
        let body = r"
mod outer {
    pub struct Nested;
    pub fn build_nested() {}
    impl Nested {}
}
";

        let counts = extract_keyword_counts(Path::new("src/nested.rs"), body);

        assert_eq!(counts.entities_new, 4);
        assert_eq!(counts.edges_new, 0);
    }

    #[test]
    fn python_extracts_functions_and_classes() {
        let body = r"
class Ingestor:
    def ingest(self):
        pass
";

        let counts = extract_keyword_counts(Path::new("ingest.py"), body);

        assert_eq!(counts.entities_new, 2);
        assert_eq!(counts.edges_new, 0);
    }

    #[test]
    fn typescript_and_javascript_extract_declarations() {
        let ts_body = r"
export function route() {}
interface RouteConfig {}
type RouteId = string;
const activeRoute = route();
";
        let js_body = r"
function route() {}
class Router {}
const activeRoute = route();
";

        assert_eq!(
            extract_keyword_counts(Path::new("route.ts"), ts_body).entities_new,
            4
        );
        assert_eq!(
            extract_keyword_counts(Path::new("route.js"), js_body).entities_new,
            3
        );
    }

    #[test]
    fn go_extracts_package_types_and_functions() {
        let body = r"
package ingest

type Scanner struct {}

func NewScanner() {}
";

        let counts = extract_keyword_counts(Path::new("scanner.go"), body);

        assert_eq!(counts.entities_new, 3);
        assert_eq!(counts.edges_new, 0);
    }

    #[test]
    fn txt_extracts_text_signals() {
        let body = "See [[Memory Store]] for San Francisco context.\nFIXME: wire this.\n";

        let counts = extract_keyword_counts(Path::new("notes.txt"), body);

        assert!(counts.entities_new >= 3);
        assert_eq!(counts.edges_new, 1);
    }

    #[test]
    fn rst_extracts_text_signals() {
        let body = "Cairn Guide\nSee [[Memory Store]] for New York context.\nTODO: wire this.\n";

        let counts = extract_keyword_counts(Path::new("guide.rst"), body);

        assert!(counts.entities_new >= 3);
        assert_eq!(counts.edges_new, 1);
    }

    #[test]
    fn unknown_extensions_return_zero_counts() {
        let body = "TODO: [[Memory Store]]\npub struct Folder;\n";

        let counts = extract_keyword_counts(Path::new("archive.bin"), body);

        assert_eq!(counts, ExtractionCounts::default());
    }
}
