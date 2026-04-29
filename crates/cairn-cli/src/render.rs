//! TTY-safe rendering helpers for byte buffers that may contain bare
//! carriage returns.
//!
//! `cairn-core::pipeline::squash::SquashOutput::compacted_bytes` is documented
//! as an audit artifact, not a render-ready string: the squash pipeline
//! preserves bare `\r` bytes inside lines so the audit hash stays stable.
//! Writing those bytes naively to a TTY causes the cursor to jump back to
//! column 0 and overwrite earlier characters, producing garbled output.
//!
//! This module owns the human-display escaping. See issue #220.

use std::borrow::Cow;

/// Visualization marker for a bare `\r` byte (Unicode `SYMBOL FOR CARRIAGE
/// RETURN`, U+240D).
pub const BARE_CR_MARKER: char = '\u{240D}';

/// Wrap an audit byte buffer for human display on a TTY.
///
/// The output preserves `\r\n` and `\n` line terminators verbatim and replaces
/// every *bare* `\r` (not followed by `\n`) with [`BARE_CR_MARKER`]. Invalid
/// UTF-8 is decoded with `U+FFFD` replacement characters via
/// [`String::from_utf8_lossy`].
///
/// Returns a borrowed `Cow` when the input is already TTY-safe (valid UTF-8,
/// no bare `\r`), and an owned `Cow` only when escaping or lossy decoding
/// allocates.
#[must_use]
pub fn render_for_tty(bytes: &[u8]) -> Cow<'_, str> {
    let decoded = String::from_utf8_lossy(bytes);
    if !contains_bare_cr(decoded.as_bytes()) {
        return decoded;
    }
    Cow::Owned(escape_bare_cr(&decoded))
}

fn contains_bare_cr(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) != Some(&b'\n') {
            return true;
        }
        i += 1;
    }
    false
}

fn escape_bare_cr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() != Some(&'\n') {
            out.push(BARE_CR_MARKER);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_borrowed() {
        let out = render_for_tty(b"hello world\n");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "hello world\n");
    }

    #[test]
    fn crlf_is_preserved_and_borrowed() {
        let out = render_for_tty(b"a\r\nb\r\nc\r\n");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn bare_cr_is_escaped() {
        let out = render_for_tty(b"first\rsecond");
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out, "first\u{240D}second");
    }

    #[test]
    fn trailing_bare_cr_is_escaped() {
        let out = render_for_tty(b"line\r");
        assert_eq!(out, "line\u{240D}");
    }

    #[test]
    fn mixed_crlf_and_bare_cr() {
        // Common shape: progress overwrites within a line, then real newline.
        let out = render_for_tty(b"build\r  10%\r  50%\r 100%\r\ndone\n");
        assert_eq!(
            out,
            "build\u{240D}  10%\u{240D}  50%\u{240D} 100%\r\ndone\n"
        );
    }

    #[test]
    fn lone_lf_passes_through() {
        let out = render_for_tty(b"a\nb\n");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn empty_input_is_borrowed_empty() {
        let out = render_for_tty(b"");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "");
    }

    #[test]
    fn invalid_utf8_uses_replacement_char() {
        // 0xFF is not valid UTF-8; expect U+FFFD in output. Owned because lossy
        // decoding allocated.
        let out = render_for_tty(b"a\xFFb");
        assert_eq!(out, "a\u{FFFD}b");
    }

    #[test]
    fn invalid_utf8_with_bare_cr() {
        let out = render_for_tty(b"a\xFFb\rc");
        assert_eq!(out, "a\u{FFFD}b\u{240D}c");
    }

    #[test]
    fn snapshot_progress_bar() {
        insta::assert_snapshot!(render_for_tty(
            b"Compiling foo v0.1.0\r    Finished in 1.2s\r\nbuild ok\n"
        ));
    }

    #[test]
    fn snapshot_only_bare_cr() {
        insta::assert_snapshot!(render_for_tty(b"\r\r\r"));
    }

    #[test]
    fn snapshot_crlf_preserved() {
        insta::assert_snapshot!(render_for_tty(b"alpha\r\nbeta\r\ngamma\r\n"));
    }
}
