//! Trigger-keyword prefilter and phrase-window builder. Spec §6.2.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use super::super::TextSpan;

/// Fixed trigger keyword set the prefilter scans for.
const TRIGGER_KEYWORDS: &[&str] = &[
    "remember",
    "forget",
    "correction",
    "skillify",
    "this is how",
];

/// Pre-built trigger prefilter. One instance per `RegexExtractor`.
pub struct TriggerPrefilter {
    ac: AhoCorasick,
}

impl Default for TriggerPrefilter {
    fn default() -> Self {
        Self::new()
    }
}

impl TriggerPrefilter {
    /// Build the (case-insensitive) prefilter.
    #[must_use]
    pub fn new() -> Self {
        let ac = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostFirst)
            .build(TRIGGER_KEYWORDS)
            .unwrap_or_else(|e| {
                // SAFETY: TRIGGER_KEYWORDS are static ASCII literals; build never fails.
                panic!("invariant: static patterns build: {e}")
            });
        Self { ac }
    }

    /// Find every keyword occurrence whose start position is at a
    /// sentence-start (per [`is_sentence_start`]) and not inside a
    /// quoted span. Returns `(start, end)` byte offsets in the original
    /// body. Capped at `MAX_PHRASE_WINDOWS` hits.
    #[must_use]
    pub fn scan(&self, body: &str) -> PrefilterScan {
        let quote_spans = collect_quote_spans(body);
        let mut hits: Vec<TextSpan> = Vec::new();
        let mut first_omitted: Option<u32> = None;
        let bytes = body.as_bytes();
        for m in self.ac.find_iter(body) {
            let start = m.start();
            let end = m.end();
            if quote_spans
                .iter()
                .any(|(qs, qe)| *qs <= start && end <= *qe)
            {
                continue;
            }
            if !is_sentence_start(bytes, start) {
                continue;
            }
            if hits.len() >= super::super::MAX_PHRASE_WINDOWS {
                first_omitted = Some(u32::try_from(start).unwrap_or(u32::MAX));
                tracing::warn!(
                    body_len = body.len(),
                    cap = super::super::MAX_PHRASE_WINDOWS,
                    "regex extractor: phrase-window cap reached"
                );
                break;
            }
            hits.push(TextSpan::new(
                u32::try_from(start).unwrap_or(u32::MAX),
                u32::try_from(end).unwrap_or(u32::MAX),
            ));
        }
        PrefilterScan {
            hits,
            first_omitted_start: first_omitted,
        }
    }
}

/// Result of a single prefilter scan.
#[derive(Debug, PartialEq, Eq)]
pub struct PrefilterScan {
    /// Byte spans of accepted trigger hits in the body.
    pub hits: Vec<TextSpan>,
    /// Byte offset where the first omitted hit began, when the scan
    /// stopped at `MAX_PHRASE_WINDOWS`. `None` when no truncation
    /// occurred.
    pub first_omitted_start: Option<u32>,
}

impl PrefilterScan {
    /// True when the scan stopped at `MAX_PHRASE_WINDOWS`.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.first_omitted_start.is_some()
    }
}

/// Whether the byte offset `pos` is at a "sentence-start" position for
/// trigger eligibility. See spec §6.2.
#[must_use]
pub fn is_sentence_start(body: &[u8], pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    let mut i = pos;
    while i > 0 && body[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i == 0 {
        return true;
    }
    let prev = body[i - 1];
    match prev {
        b'\n' | b';' | b'?' | b'!' | b',' => true,
        b'.' => is_period_sentence_boundary(body, i - 1),
        _ => preceded_by_conjunction(body, i),
    }
}

fn preceded_by_conjunction(body: &[u8], end: usize) -> bool {
    let mut j = end;
    while j > 0 && !body[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    let word = &body[j..end];
    ascii_eq_ignore_case(word, b"and")
        || ascii_eq_ignore_case(word, b"but")
        || ascii_eq_ignore_case(word, b"then")
}

fn ascii_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

fn is_period_sentence_boundary(body: &[u8], period_pos: usize) -> bool {
    let start = period_pos.saturating_sub(6);
    let window = &body[start..period_pos];
    !looks_like_abbreviation(window)
}

fn looks_like_abbreviation(window: &[u8]) -> bool {
    let mut i = 0;
    while i + 1 < window.len() {
        if window[i] == b'.' && window[i + 1].is_ascii_alphabetic() {
            return true;
        }
        i += 1;
    }
    false
}

fn collect_quote_spans(body: &str) -> Vec<(usize, usize)> {
    let bytes = body.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'"' | b'\'' | b'`') {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b {
                j += 1;
            }
            if j < bytes.len() {
                spans.push((i, j + 1));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    spans
}

/// Phrase window: a slice of the body anchored at a prefilter hit, ending
/// at the next stop (sentence boundary, semicolon, newline, next hit, or EOB).
#[derive(Debug, PartialEq, Eq)]
pub struct PhraseWindow {
    /// Byte span of the window in the body.
    pub span: TextSpan,
}

/// Build phrase windows from a prefilter scan. Each window starts at a
/// hit and runs to the next stop. Zero-length windows are dropped.
///
/// When `scan.first_omitted_start` is `Some`, the last materialised
/// window's hard stop is bounded at the first-omitted hit so the window
/// never absorbs trigger occurrences past the cap (those occurrences are
/// represented in the LLM eligible-span tail by the dispatcher instead).
#[must_use]
pub fn build_phrase_windows(body: &str, scan: &PrefilterScan) -> Vec<PhraseWindow> {
    let bytes = body.as_bytes();
    let hits = &scan.hits;
    let truncation_hard_stop = scan.first_omitted_start.map(|s| s as usize);
    let mut windows = Vec::with_capacity(hits.len());
    for (i, hit) in hits.iter().enumerate() {
        let start = hit.start as usize;
        let next_hit_start = hits
            .get(i + 1)
            .map(|h| h.start as usize)
            .or(truncation_hard_stop)
            .unwrap_or(bytes.len());
        let stop = find_window_stop(bytes, start, next_hit_start);
        if stop > start {
            windows.push(PhraseWindow {
                span: TextSpan::new(
                    u32::try_from(start).unwrap_or(u32::MAX),
                    u32::try_from(stop).unwrap_or(u32::MAX),
                ),
            });
        }
    }
    windows
}

fn find_window_stop(bytes: &[u8], start: usize, hard_stop: usize) -> usize {
    let mut i = start;
    while i < hard_stop {
        let b = bytes[i];
        match b {
            b';' | b'\n' => return i,
            b'.' => {
                let after_is_ws_or_eob = i + 1 == bytes.len() || bytes[i + 1].is_ascii_whitespace();
                if after_is_ws_or_eob && is_period_sentence_boundary(bytes, i) {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    hard_stop.min(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(body: &str) -> Vec<&str> {
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        scan.hits
            .iter()
            .map(|s| &body[s.start as usize..s.end as usize])
            .collect()
    }

    #[test]
    fn finds_remember_at_start() {
        let hits = scan("remember that I prefer dark mode");
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn finds_remember_after_period_with_space() {
        let hits = scan("FYI old. remember that I prefer cash");
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn ignores_abbreviation_period() {
        let hits = scan("remember that I live in the U.S. and prefer cash");
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn skips_keyword_inside_quotes() {
        let hits = scan(r#"the user said "remember that" by mistake"#);
        assert!(hits.is_empty());
    }

    #[test]
    fn finds_after_conjunction_with_trigger() {
        let hits = scan("forget X and remember Y");
        assert_eq!(hits, vec!["forget", "remember"]);
    }

    #[test]
    fn does_not_split_normal_and() {
        let hits = scan("remember that Alice and Bob are on call");
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn caps_at_max_phrase_windows() {
        let body = "remember a; ".repeat(super::super::super::MAX_PHRASE_WINDOWS + 5);
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(&body);
        assert!(scan.truncated());
        assert!(scan.first_omitted_start.is_some());
        assert_eq!(scan.hits.len(), super::super::super::MAX_PHRASE_WINDOWS);
    }

    #[test]
    fn finds_trigger_in_oversized_body() {
        let mut body = "lorem ipsum. ".repeat(20_000);
        body.push_str("forget my old address");
        let hits = scan(&body);
        assert_eq!(hits, vec!["forget"]);
    }

    #[test]
    fn quote_aware_finds_unquoted_after_quoted() {
        let body = r#"the user said "remember that" then forget my password"#;
        let hits = scan(body);
        assert_eq!(hits, vec!["forget"]);
    }

    #[test]
    fn truncated_scan_caps_last_window_at_first_omitted_hit() {
        // Single sentence, no semicolons/newlines/periods, with many
        // `remember X` clauses joined by `and` (sentence-start eligible
        // when followed by a trigger). The cap at MAX_PHRASE_WINDOWS
        // means hit #(MAX+1) and beyond must NOT bleed into the last
        // window; the last window must stop before them.
        let cap = super::super::super::MAX_PHRASE_WINDOWS;
        let mut body = String::from("remember a");
        for _ in 1..(cap + 5) {
            body.push_str(" and remember b");
        }
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(&body);
        assert!(scan.truncated());
        let first_omitted = scan.first_omitted_start.expect("truncated") as usize;
        let windows = build_phrase_windows(&body, &scan);
        let last = windows.last().expect("at least one window");
        assert!(
            (last.span.end as usize) <= first_omitted,
            "last window end {} must not extend past first-omitted hit start {}",
            last.span.end,
            first_omitted,
        );
    }

    #[test]
    fn build_windows_single_hit_runs_to_eob() {
        let body = "remember that I prefer dark mode";
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        let windows = build_phrase_windows(body, &scan);
        assert_eq!(windows.len(), 1);
        assert_eq!(
            windows[0].span,
            TextSpan::new(0, u32::try_from(body.len()).unwrap())
        );
    }

    #[test]
    fn build_windows_stops_at_semicolon() {
        let body = "remember that thing; nothing else";
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        let windows = build_phrase_windows(body, &scan);
        assert_eq!(windows.len(), 1);
        let s = &body[windows[0].span.start as usize..windows[0].span.end as usize];
        assert_eq!(s, "remember that thing");
    }

    #[test]
    fn build_windows_two_triggers() {
        let body = "forget X and remember Y";
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        let windows = build_phrase_windows(body, &scan);
        assert_eq!(windows.len(), 2);
        let w0 = &body[windows[0].span.start as usize..windows[0].span.end as usize];
        let w1 = &body[windows[1].span.start as usize..windows[1].span.end as usize];
        assert_eq!(w0, "forget X and ");
        assert_eq!(w1, "remember Y");
    }
}
