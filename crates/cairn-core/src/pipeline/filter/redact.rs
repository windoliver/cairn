//! Pre-persist PII / secret redactor (brief §5.2 + §14).
//!
//! Pure-Rust regex pipeline — no Python, no network, no Presidio
//! dependency at P0. Detectors live in a static table inside `redact`
//! and are applied in registration order; overlapping matches resolve
//! to the earliest start (and longest span on ties) so the output is
//! deterministic.
//!
//! The function returns a [`RedactedPayload`] containing the masked
//! text and a body-free vector of [`RedactionSpan`]s. The masked text
//! is safe to log at `debug`; the spans carry only `(start, end, tag)`
//! and are safe to log at `info` per CLAUDE.md §6.6.
//!
//! P0 covers nine detectors: `Email`, `Phone`, `Ipv4`, `AwsAccessKeyId`,
//! `GithubToken`, `SlackToken`, `Jwt`, `Ssn`, `HexSecret`. The detector
//! list is intentionally over-eager — false positives are acceptable
//! at P0 (an over-redacted body costs less than a leaked secret); a
//! follow-up Presidio binding can lower false-positive rate without
//! changing this module's signature.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Tag for a single redaction span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RedactionTag {
    /// Email address.
    Email,
    /// Phone number (E.164-ish).
    Phone,
    /// IPv4 address.
    Ipv4,
    /// AWS access key ID (`AKIA...` / `ASIA...`).
    AwsAccessKeyId,
    /// GitHub personal-access token (`ghp_` / `gho_` / `ghu_` / `ghs_` / `ghr_`)
    /// or fine-grained PAT (`github_pat_...`).
    GithubToken,
    /// Slack token (`xox[abceoprs]-...`, including `xoxe-` refresh,
    /// `xoxc-` user, `xoxo-` legacy).
    SlackToken,
    /// JSON Web Token (`<header>.<body>.<sig>`).
    Jwt,
    /// US Social Security Number (`NNN-NN-NNNN`).
    Ssn,
    /// Generic high-entropy hex secret (≥32 chars).
    HexSecret,
    /// Opaque vendor API key with a stable prefix
    /// (`sk-`, `sk_live_`, `sk_test_`, `pk_live_`, `pk_test_`,
    /// `rk_live_`, `whsec_`, `AIza...` Google API).
    OpaqueApiKey,
    /// Context-keyed secret in `key=value` form
    /// (`api_key=...`, `secret=...`, `token=...`, `password=...`).
    ContextKeyedSecret,
}

impl RedactionTag {
    /// Wire-format identifier (lower-snake-case, identical to the serde form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Ipv4 => "ipv4",
            Self::AwsAccessKeyId => "aws_access_key_id",
            Self::GithubToken => "github_token",
            Self::SlackToken => "slack_token",
            Self::Jwt => "jwt",
            Self::Ssn => "ssn",
            Self::HexSecret => "hex_secret",
            Self::OpaqueApiKey => "opaque_api_key",
            Self::ContextKeyedSecret => "context_keyed_secret",
        }
    }
}

/// One redacted region — body bytes are deliberately not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionSpan {
    /// Byte offset of the start of the redacted region in the **input** text.
    pub start: usize,
    /// Byte offset (exclusive) of the end in the **input** text.
    pub end: usize,
    /// Detector that fired.
    pub tag: RedactionTag,
}

/// Output of [`redact`] — masked text plus a body-free span list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedPayload {
    /// Text with each PII span replaced by `[REDACTED:<tag>]`.
    pub text: String,
    /// One entry per detector hit. Order = left-to-right over the input.
    pub spans: Vec<RedactionSpan>,
}

/// Redact PII / secret patterns from `input` (brief §5.2 + §14).
///
/// Pure, deterministic, single-pass — no I/O, no network, no Python.
/// Returns the masked text plus a body-free span list. Idempotent:
/// `redact(redact(s).text).spans` is empty for any `s`.
#[must_use]
pub fn redact(input: &str) -> RedactedPayload {
    let mut hits: Vec<RedactionSpan> = detectors()
        .iter()
        .flat_map(|(tag, re)| {
            re.find_iter(input).map(|m| RedactionSpan {
                start: m.start(),
                end: m.end(),
                tag: *tag,
            })
        })
        .collect();

    // Context-keyed secrets are scanned with a hand-rolled
    // delimiter-aware scanner so values are not bounded by an
    // arbitrary regex length cap and so `Authorization: Bearer ...`
    // headers are covered.
    hits.extend(find_context_keyed_spans(input));

    // Sort by start asc, then by length desc so that on overlap the
    // longer match wins — this avoids `email` swallowing the local
    // part of a JWT that happens to contain `@`.
    hits.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then((b.end - b.start).cmp(&(a.end - a.start)))
    });

    // Drop spans that overlap an earlier-accepted span.
    let mut accepted: Vec<RedactionSpan> = Vec::with_capacity(hits.len());
    let mut cursor = 0usize;
    for h in hits {
        if h.start >= cursor {
            cursor = h.end;
            accepted.push(h);
        }
    }

    // Build the masked text in one pass.
    let mut text = String::with_capacity(input.len());
    let mut last = 0usize;
    for span in &accepted {
        text.push_str(&input[last..span.start]);
        text.push_str("[REDACTED:");
        text.push_str(span.tag.as_str());
        text.push(']');
        last = span.end;
    }
    text.push_str(&input[last..]);

    RedactedPayload {
        text,
        spans: accepted,
    }
}

fn detectors() -> &'static [(RedactionTag, Regex)] {
    static CELL: OnceLock<Vec<(RedactionTag, Regex)>> = OnceLock::new();
    CELL.get_or_init(|| {
        // Order matters when two detectors could match overlapping bytes —
        // the sort in `redact` resolves overlap by start-then-length, so
        // ordering here is mostly cosmetic, but kept structural-first.
        vec![
            (
                RedactionTag::Jwt,
                build(r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\b"),
            ),
            (
                RedactionTag::AwsAccessKeyId,
                build(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            ),
            (
                RedactionTag::GithubToken,
                build(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9_]{36,255}\b"),
            ),
            (
                // Fine-grained PATs: `github_pat_<22 base62>_<59 base62>`
                // — bound the underscore-separated tail conservatively.
                RedactionTag::GithubToken,
                build(r"\bgithub_pat_[A-Za-z0-9_]{22,255}\b"),
            ),
            (
                RedactionTag::SlackToken,
                // Cover the full xox-family: bot/user/admin/refresh/
                // configuration/legacy/external — `[abceoprs]`.
                build(r"\bxox[abceoprs]-[A-Za-z0-9-]{10,200}\b"),
            ),
            (
                // Opaque vendor API keys with stable prefixes. Bounded
                // suffix lengths keep regex linear-time. Order before
                // HexSecret so a hex-shaped suffix doesn't win.
                RedactionTag::OpaqueApiKey,
                build(r"\b(?:sk_live|sk_test|pk_live|pk_test|rk_live|whsec)_[A-Za-z0-9]{16,128}\b"),
            ),
            (
                // OpenAI / Anthropic style `sk-...` and `sk-proj-...`.
                RedactionTag::OpaqueApiKey,
                build(r"\bsk-(?:proj-|ant-)?[A-Za-z0-9_-]{20,200}\b"),
            ),
            (
                // Google API keys.
                RedactionTag::OpaqueApiKey,
                build(r"\bAIza[0-9A-Za-z_-]{35}\b"),
            ),
            (
                // HashiCorp Vault tokens. Service tokens prefix
                // `hvs.`, batch tokens `hvb.`, root tokens `hvr.`.
                // Stable namespace, opaque base62 body.
                RedactionTag::OpaqueApiKey,
                build(r"\bhv[srb]\.[A-Za-z0-9_-]{20,255}\b"),
            ),
            (
                RedactionTag::Email,
                build(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,24}\b"),
            ),
            (RedactionTag::Ssn, build(r"\b\d{3}-\d{2}-\d{4}\b")),
            (
                RedactionTag::Phone,
                // E.164-ish: optional +CC then 7–15 digits with dash/space separators.
                build(r"\+?\d{1,3}[- ]\d{3,4}[- ]\d{3,4}(?:[- ]\d{2,4})?"),
            ),
            (RedactionTag::Ipv4, build(r"\b(?:\d{1,3}\.){3}\d{1,3}\b")),
            (RedactionTag::HexSecret, build(r"\b[A-Fa-f0-9]{32,128}\b")),
        ]
    })
}

fn build(pat: &str) -> Regex {
    #[allow(clippy::expect_used)]
    Regex::new(pat).expect("static redactor pattern compiles")
}

// ── Context-keyed secret scanner ──────────────────────────────────────
//
// `password=...`, `api_key: "..."`, `Authorization: Bearer <token>`,
// `secret = ...` — these envelopes wrap values of arbitrary length and
// arbitrary character class. A regex with a fixed length cap leaks the
// suffix when the value runs longer than the cap, and a fixed value
// charset misses values with punctuation. The scanner consumes through
// the matching delimiter (close quote, whitespace, comma, semicolon,
// closing bracket, newline, or end-of-input) so the entire secret is
// captured regardless of length.

fn find_context_keyed_spans(input: &str) -> Vec<RedactionSpan> {
    static KEY_RE: OnceLock<Regex> = OnceLock::new();
    let key_re = KEY_RE.get_or_init(|| {
        // Match the keyword + optional close-key-quote + optional
        // separator + optional whitespace. The optional `['"]?` runs
        // both before and after `[:=]?` so JSON / YAML structured-key
        // shapes (`"password":`, `'api_key' = `) align the post-key
        // cursor on the value's first byte.
        //
        // Compound prefixes (`client_secret`, `access_token`,
        // `db_password`, `oauth_refresh_token`, etc.) are covered by
        // an optional `[a-z][a-z0-9]*[_-]` head before the keyword.
        // Without it the `\b`-bounded keyword would not match inside
        // `client_secret` because the leading `client_` is contiguous
        // word characters. `bearer` and the standalone `Bearer` after
        // `Authorization:` both fall through to the value-consume
        // step below.
        build(
            r#"(?ix)
            \b
            [a-z0-9_-]{0,40}?                # optional prefix chars (lazy, bounded)
            (?: api[_-]?key
              | secret
              | token
              | password
              | passwd
              | pwd
              | auth
              | authorization
              | bearer
              | credential[s]?
              | private[_-]?key              # GCP service-account JSON, PEM PKCS#8
              | private[_-]?key[_-]?id       # GCP private_key_id
              | account[_-]?key              # Azure Storage AccountKey=
              | accountkey                   # bare CamelCase form in Azure conn strings
              | shared[_-]?access[_-]?signature
              | signature                    # Azure SAS sig=, JWT sig fields
              | sig                          # Azure SAS sig=
              | client[_-]?secret            # OAuth client secret (also caught by suffix path)
            )
            [a-z0-9_-]{0,40}                 # optional suffix chars
            \b
            ['"]? \s* [:=]? \s*
            "#,
        )
    });

    let bytes = input.as_bytes();
    let mut spans: Vec<RedactionSpan> = Vec::new();

    for m in key_re.find_iter(input) {
        let span_start = m.start();
        let mut i = m.end();

        // After the key/separator, optionally swallow a `Bearer ` /
        // `Token ` HTTP-scheme prefix so the actual opaque value is
        // what gets consumed below.
        i = skip_scheme_prefix(input, i);
        if i >= bytes.len() {
            continue;
        }

        // Consume the value through its delimiter.
        let (value_start, value_end) = consume_value(input, i);
        if value_end <= value_start || value_end - value_start < 6 {
            continue;
        }

        spans.push(RedactionSpan {
            start: span_start,
            end: value_end,
            tag: RedactionTag::ContextKeyedSecret,
        });
    }
    spans
}

/// Skip over `Bearer ` / `Token ` / `Bearer: ` / `Token: ` scheme
/// prefixes after a context key, so the consume step lands on the
/// actual opaque value.
fn skip_scheme_prefix(input: &str, mut i: usize) -> usize {
    let bytes = input.as_bytes();
    for prefix in ["bearer", "token"] {
        let len = prefix.len();
        let Some(slice) = input.get(i..i.saturating_add(len)) else {
            continue;
        };
        if !slice.eq_ignore_ascii_case(prefix) {
            continue;
        }
        let after = i + len;
        // Optional `:` or `=` immediately after the scheme keyword.
        let after_sep = match bytes.get(after) {
            Some(b':' | b'=') => after + 1,
            _ => after,
        };
        // Require at least one whitespace separator after the scheme.
        if !bytes.get(after_sep).is_some_and(u8::is_ascii_whitespace) {
            continue;
        }
        i = after_sep;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        return i;
    }
    i
}

/// Consume a value starting at `i`. If the first byte is `'` or `"`,
/// consume through the matching close quote **across newlines** (a
/// quoted secret can legally span multiple lines, e.g. a heredoc-shaped
/// PEM body), and fail closed by redacting through EOF if the quote is
/// never closed. Otherwise, consume non-whitespace, non-delimiter
/// bytes — unquoted values still terminate at the first delimiter.
fn consume_value(input: &str, i: usize) -> (usize, usize) {
    let bytes = input.as_bytes();
    if i >= bytes.len() {
        return (i, i);
    }
    let first = bytes[i];
    if first == b'"' || first == b'\'' {
        let q = first;
        let value_start = i;
        let mut j = i + 1;
        // Consume through the matching close quote, regardless of
        // newlines. A multiline quoted secret (e.g. an embedded PEM
        // body or a backslash-escaped multi-line string) must be
        // redacted in full — terminating at `\n` would leak everything
        // after the first line. Treat `\X` as an escape sequence and
        // consume both bytes so a `\"` inside the value does not act
        // as a close quote — that would split a JSON-escaped secret
        // and leak the suffix.
        while j < bytes.len() && bytes[j] != q {
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
                continue;
            }
            j += 1;
        }
        if j < bytes.len() && bytes[j] == q {
            return (value_start, j + 1);
        }
        // Quote never closed — fail closed: redact through EOF.
        return (value_start, bytes.len());
    }
    let value_start = i;
    let mut j = i;
    while j < bytes.len() && !is_value_terminator(bytes[j]) {
        j += 1;
    }
    (value_start, j)
}

const fn is_value_terminator(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\n' | b'\r' | b',' | b';' | b')' | b'}' | b'>' | b'<' | b'"' | b'\''
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_tag(input: &str) -> RedactionTag {
        let r = redact(input);
        assert_eq!(
            r.spans.len(),
            1,
            "expected exactly one span, got {:?}",
            r.spans
        );
        r.spans[0].tag
    }

    // ── Pass-through ─────────────────────────────────────────────────

    #[test]
    fn clean_text_passes_through_unchanged() {
        let input = "the quick brown fox jumps over the lazy dog";
        let out = redact(input);
        assert_eq!(out.text, input);
        assert!(out.spans.is_empty());
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let out = redact("");
        assert_eq!(out.text, "");
        assert!(out.spans.is_empty());
    }

    // ── Per-detector ─────────────────────────────────────────────────

    #[test]
    fn detects_email() {
        let r = redact("contact me at alice@example.com please");
        assert!(r.text.contains("[REDACTED:email]"), "{}", r.text);
        assert!(!r.text.contains("alice@example.com"));
        assert_eq!(r.spans.len(), 1);
        assert_eq!(r.spans[0].tag, RedactionTag::Email);
    }

    #[test]
    fn detects_aws_access_key_id() {
        assert_eq!(
            one_tag("creds AKIAIOSFODNN7EXAMPLE end"),
            RedactionTag::AwsAccessKeyId
        );
    }

    #[test]
    fn detects_github_token() {
        // The token-shaped detector fires; if a `token` context-key
        // scanner span overlaps, either detector is acceptable as
        // long as the raw secret is gone from the output.
        let s = format!("ghp_{} alone", "a".repeat(36));
        let r = redact(&s);
        assert!(!r.spans.is_empty(), "no span fired for {s}");
        assert_eq!(r.spans[0].tag, RedactionTag::GithubToken);
        assert!(!r.text.contains(&"a".repeat(36)));
    }

    #[test]
    fn detects_github_fine_grained_pat() {
        // Fine-grained PAT: `github_pat_<22 base62>_<59 base62>`.
        let pat = format!("github_pat_{}_{}", "A".repeat(22), "B".repeat(59));
        let r = redact(&format!("creds {pat} done"));
        assert_eq!(r.spans.len(), 1, "expected one span, got {:?}", r.spans);
        assert_eq!(r.spans[0].tag, RedactionTag::GithubToken);
        assert!(!r.text.contains(&pat), "raw token leaked: {}", r.text);
    }

    #[test]
    fn detects_slack_token() {
        assert_eq!(
            one_tag("post xoxb-1234567890-abcdef done"),
            RedactionTag::SlackToken
        );
    }

    #[test]
    fn detects_slack_refresh_and_config_tokens() {
        // xoxe = refresh, xoxc = config — both must redact.
        assert_eq!(
            one_tag("refresh xoxe-1234567890-abcdef done"),
            RedactionTag::SlackToken,
        );
        assert_eq!(
            one_tag("config xoxc-1234567890-abcdef done"),
            RedactionTag::SlackToken,
        );
    }

    #[test]
    fn detects_openai_style_sk_key() {
        let r = redact("OPENAI_API_KEY=sk-proj-aB3dEfGhIjKlMnOpQrStUv done");
        // The context-keyed assignment fires first and swallows the
        // value; either tag is acceptable as long as the raw key is
        // not in the masked text.
        assert!(!r.spans.is_empty(), "no span fired");
        assert!(
            !r.text.contains("sk-proj-aB3dEfGhIjKlMnOpQrStUv"),
            "sk- key leaked: {}",
            r.text
        );
    }

    #[test]
    fn detects_stripe_style_sk_live_key() {
        // Bare key (no `token` keyword to compete with the
        // OpaqueApiKey detector).
        assert_eq!(
            one_tag("creds sk_live_aB3dEfGhIjKlMnOpQrStUv done"),
            RedactionTag::OpaqueApiKey
        );
    }

    #[test]
    fn detects_google_api_key() {
        // Bare key (no preceding `token` keyword that would otherwise
        // get matched by the context-keyed scanner).
        let key: String = std::iter::repeat_n('A', 35).collect();
        let r = redact(&format!("creds AIza{key} done"));
        assert!(!r.spans.is_empty(), "no span fired");
        assert_eq!(r.spans[0].tag, RedactionTag::OpaqueApiKey);
        assert!(!r.text.contains(&format!("AIza{key}")), "{}", r.text);
    }

    #[test]
    fn detects_context_keyed_secret() {
        let r = redact("config: password=hunter2horsestaplebattery and rest");
        assert!(!r.spans.is_empty(), "no span fired");
        assert!(
            !r.text.contains("hunter2horsestaplebattery"),
            "context-keyed secret leaked: {}",
            r.text
        );
    }

    #[test]
    fn detects_api_key_assignment_dash_and_underscore() {
        for variant in [
            "api_key=verysecretverylongtokenstring",
            "api-key: verysecretverylongtokenstring",
            "auth=verysecretverylongtokenstring",
        ] {
            let r = redact(variant);
            assert!(!r.spans.is_empty(), "no span fired for {variant}");
            assert!(
                !r.text.contains("verysecretverylongtokenstring"),
                "secret leaked for {variant}: {}",
                r.text
            );
        }
    }

    #[test]
    fn detects_context_keyed_secret_with_punctuation() {
        // Real-world passwords / tokens often contain `!`, `$`, `@`,
        // `%`, `:`, etc. — a narrow value charset would let the leading
        // punctuation escape the regex and ship the secret to disk.
        for variant in [
            r#"password="abc!defghijklmnopqrstuv""#,
            r"password='abc$defghijklmnopqrstuv'",
            "password=abc@defghijklmnopqrstuv",
            "secret=abc%defghijklmnopqrstuv",
            "token=ab:cdefghijklmnopqrstuvwxyz",
            "bearer Token: ab&cdefghijklmnopqrstuv",
        ] {
            let r = redact(variant);
            assert!(!r.spans.is_empty(), "no span fired for {variant}");
            for needle in [
                "abc!defghijklmnopqrstuv",
                "abc$defghijklmnopqrstuv",
                "abc@defghijklmnopqrstuv",
                "abc%defghijklmnopqrstuv",
                "ab:cdefghijklmnopqrstuvwxyz",
                "ab&cdefghijklmnopqrstuv",
            ] {
                assert!(
                    !r.text.contains(needle),
                    "secret `{needle}` leaked for {variant}: {}",
                    r.text
                );
            }
        }
    }

    #[test]
    fn detects_quoted_password_with_punctuation_after_short_prefix() {
        // The exact case from the round-3 review: short accepted
        // prefix, then punctuation, then the rest of the value.
        let r = redact(r#"password="abc!defghijklmnopqrstuv""#);
        assert!(!r.spans.is_empty(), "redactor missed quoted password");
        assert!(
            !r.text.contains("abc!defghijklmnopqrstuv"),
            "secret leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_quoted_password_longer_than_old_regex_cap() {
        // The previous regex capped values at 200 bytes. A quoted
        // password longer than the cap leaked the suffix. The hand-
        // rolled scanner consumes through the matching close quote
        // regardless of length.
        let secret: String = std::iter::repeat_n('a', 400).collect();
        let input = format!(r#"password="{secret}""#);
        let r = redact(&input);
        assert!(!r.spans.is_empty(), "no span for >200B quoted password");
        assert!(
            !r.text.contains(&secret),
            "long password leaked through scanner: {}",
            r.text
        );
    }

    #[test]
    fn redacts_multiline_quoted_secret_through_close_quote() {
        // A quoted context-keyed value can legitimately span multiple
        // lines (e.g. an embedded PEM body or a backslash-escaped
        // multi-line string). The scanner must consume through the
        // matching close quote across newlines — terminating at `\n`
        // would leak every line after the first.
        let secret = "first-line-contents\nsecond-line-secret\nthird-line-tail";
        let input = format!("password=\"{secret}\" trailing");
        let r = redact(&input);
        assert!(!r.spans.is_empty(), "no span for multiline quoted secret");
        for line in [
            "first-line-contents",
            "second-line-secret",
            "third-line-tail",
        ] {
            assert!(
                !r.text.contains(line),
                "line `{line}` leaked through multiline scanner: {}",
                r.text
            );
        }
        // Bytes outside the quoted value are preserved.
        assert!(r.text.contains(" trailing"), "tail dropped: {}", r.text);
    }

    #[test]
    fn redacts_json_quoted_password_field() {
        // Standard JSON shape `{"password":"<value>"}` — round 6 codex
        // flagged that the closing key-quote was not in the regex's
        // separator set, so consume_value started on the `:` and
        // emitted nothing.
        for input in [
            r#"{"password":"hunter2horsestaplebattery"}"#,
            r#"{"api_key": "sk_live_abc123def456"}"#,
            r#"{"secret":  "verysecretverylongtoken"}"#,
            r#"{ "auth" : "bearer-token-xyz123" }"#,
        ] {
            let r = redact(input);
            assert!(!r.spans.is_empty(), "no span fired for {input}");
            for needle in [
                "hunter2horsestaplebattery",
                "sk_live_abc123def456",
                "verysecretverylongtoken",
                "bearer-token-xyz123",
            ] {
                assert!(
                    !r.text.contains(needle),
                    "JSON-quoted secret `{needle}` leaked for {input}: {}",
                    r.text
                );
            }
        }
    }

    #[test]
    fn redacts_compound_keyed_secrets() {
        // Round 7 codex flagged that `client_secret`, `access_token`,
        // `refresh_token`, `db_password`, etc. all shipped through
        // unredacted. The compound-prefix support in KEY_RE makes the
        // keyword match anywhere inside the structured key word.
        for input in [
            r#"{"client_secret":"abcd1234efgh5678"}"#,
            r#"{"access_token":"opaque-bearer-12345"}"#,
            r#"{"refresh_token":"opaque-refresh-12345"}"#,
            "db_password=verysecretdbpassword",
            "client_id=foo client_secret=hunter2horsestaple",
            "oauth_refresh_token=longopaquetokenstring",
            "clientSecret=camelCasedSecretValue",
        ] {
            let r = redact(input);
            assert!(!r.spans.is_empty(), "no span fired for {input}");
            for needle in [
                "abcd1234efgh5678",
                "opaque-bearer-12345",
                "opaque-refresh-12345",
                "verysecretdbpassword",
                "hunter2horsestaple",
                "longopaquetokenstring",
                "camelCasedSecretValue",
            ] {
                assert!(
                    !r.text.contains(needle),
                    "compound-keyed secret `{needle}` leaked for {input}: {}",
                    r.text
                );
            }
        }
    }

    #[test]
    fn redacts_gcp_service_account_private_key_field() {
        // Real-shaped GCP service-account JSON. Round 8 codex flagged
        // that `private_key` and `private_key_id` were not in the
        // keyword set, so the embedded PEM body shipped through.
        let input = r#"{"type":"service_account","private_key_id":"abc1234deadbeef","private_key":"-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDsecretpembody\n-----END PRIVATE KEY-----\n"}"#;
        let r = redact(input);
        assert!(!r.spans.is_empty(), "no span for GCP private_key field");
        for needle in [
            "abc1234deadbeef",
            "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSj",
            "secretpembody",
        ] {
            assert!(
                !r.text.contains(needle),
                "GCP secret `{needle}` leaked: {}",
                r.text
            );
        }
    }

    #[test]
    fn redacts_azure_storage_connection_string_account_key() {
        // Azure Storage connection strings expose `AccountKey=` —
        // case-sensitive and bare (not `account_key`).
        let input = "DefaultEndpointsProtocol=https;AccountName=foo;AccountKey=base64encodedazurekeyvaluehere==;EndpointSuffix=core.windows.net";
        let r = redact(input);
        assert!(!r.spans.is_empty(), "no span for Azure AccountKey");
        assert!(
            !r.text.contains("base64encodedazurekeyvaluehere=="),
            "Azure AccountKey leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_azure_sas_signature_field() {
        // Azure Shared Access Signature URL: `?sig=<base64-encoded>`.
        let input = "https://foo.blob.core.windows.net/container?sv=2021-08-06&sig=abcdef1234567890%2Fbase64sigvalue%3D&se=2024-01-01";
        let r = redact(input);
        assert!(!r.spans.is_empty(), "no span for Azure SAS sig= field");
        assert!(
            !r.text.contains("abcdef1234567890%2Fbase64sigvalue%3D"),
            "Azure SAS signature leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_shared_access_signature_field() {
        let input = r#"{"shared_access_signature":"abcdef1234567890longsigvalue"}"#;
        let r = redact(input);
        assert!(!r.spans.is_empty(), "no span for shared_access_signature");
        assert!(
            !r.text.contains("abcdef1234567890longsigvalue"),
            "SAS leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_hashicorp_vault_service_token() {
        // Round 9 codex: bare `hvs.<opaque>` Vault tokens in logs or
        // pasted output had no matching detector unless a `token=`
        // prefix happened to be there too.
        let token = "hvs.CAESILe6BsXg-fakeVaultBodyABCDEFGHIJklmnopqrstuvwxYZ0123";
        let r = redact(&format!("logged in with {token} successfully"));
        assert!(!r.spans.is_empty(), "no span for hvs. Vault token");
        assert!(!r.text.contains(token), "Vault token leaked: {}", r.text);
    }

    #[test]
    fn redacts_hashicorp_vault_batch_and_root_tokens() {
        for prefix in ["hvb", "hvr"] {
            let token = format!("{prefix}.CAESILe6BsXg-fakeVaultBodyABCDEFGHIJklmnopqrstuvwxYZ");
            let r = redact(&format!("token: {token}"));
            assert!(!r.spans.is_empty(), "no span for {prefix}. token");
            assert!(
                !r.text.contains(&token),
                "{prefix}. token leaked: {}",
                r.text
            );
        }
    }

    #[test]
    fn redacts_credentials_keyword() {
        // The `credentials` (and `credential`) keyword is a common
        // cloud-config field name.
        let r = redact("aws_credentials: my-secret-creds-string-here");
        assert!(!r.spans.is_empty(), "no span for aws_credentials");
        assert!(
            !r.text.contains("my-secret-creds-string-here"),
            "credentials value leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_yaml_single_quoted_password_field() {
        // YAML / Python dict shape with single-quoted values.
        let r = redact(r"config: 'password': 'verysecretpassword42'");
        assert!(!r.spans.is_empty(), "no span for yaml-shaped key");
        assert!(
            !r.text.contains("verysecretpassword42"),
            "yaml secret leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_json_escaped_quote_in_value_through_real_close() {
        // The value contains an escaped `\"` which must not act as a
        // close quote — without backslash-escape handling, the scanner
        // would terminate after `abc` and leak `def-rest-of-secret`.
        let input = r#"{"password":"abc\"def-rest-of-secret"}"#;
        let r = redact(input);
        assert!(!r.spans.is_empty(), "no span for escaped-quote secret");
        for needle in ["abc\\\"def-rest-of-secret", "def-rest-of-secret"] {
            assert!(
                !r.text.contains(needle),
                "escaped-quote suffix leaked for `{needle}`: {}",
                r.text
            );
        }
    }

    #[test]
    fn redacts_unclosed_quoted_secret_through_eof() {
        // If the close quote never appears the scanner fails closed by
        // redacting through end-of-input — a partial leak is still a
        // leak.
        let input =
            "config: api_key=\"never-closed-secret-spanning\nmany lines without a close quote";
        let r = redact(input);
        assert!(!r.spans.is_empty(), "no span for unclosed quoted secret");
        assert!(
            !r.text.contains("never-closed-secret-spanning"),
            "unclosed secret prefix leaked: {}",
            r.text
        );
        assert!(
            !r.text.contains("many lines without a close quote"),
            "unclosed secret tail leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_unquoted_token_longer_than_old_regex_cap() {
        let secret: String = std::iter::repeat_n('z', 400).collect();
        let input = format!("token={secret} done");
        let r = redact(&input);
        assert!(!r.spans.is_empty(), "no span for >200B unquoted token");
        assert!(!r.text.contains(&secret), "long token leaked: {}", r.text);
    }

    #[test]
    fn redacts_authorization_bearer_header() {
        // The HTTP form `Authorization: Bearer <opaque>` — `bearer`
        // appears after a `:` separator, but the actual value is the
        // opaque token after the `Bearer ` scheme. The scanner walks
        // past the scheme prefix and consumes the token through the
        // next delimiter.
        let r = redact("Authorization: Bearer abcDEF1234567890ZYXWvut done");
        assert!(!r.spans.is_empty(), "no span for Authorization header");
        assert!(
            !r.text.contains("abcDEF1234567890ZYXWvut"),
            "bearer token leaked: {}",
            r.text
        );
    }

    #[test]
    fn redacts_authorization_token_scheme() {
        // Same shape, different scheme keyword.
        let r = redact("Authorization: Token abcDEF1234567890ZYXWvut done");
        assert!(!r.spans.is_empty());
        assert!(
            !r.text.contains("abcDEF1234567890ZYXWvut"),
            "scheme'd token leaked: {}",
            r.text
        );
    }

    #[test]
    fn detects_jwt() {
        let jwt =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NSJ9.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(one_tag(jwt), RedactionTag::Jwt);
    }

    #[test]
    fn detects_ssn() {
        assert_eq!(one_tag("ssn 123-45-6789 here"), RedactionTag::Ssn);
    }

    #[test]
    fn detects_phone() {
        assert_eq!(one_tag("call +1-555-123-4567"), RedactionTag::Phone);
    }

    #[test]
    fn detects_ipv4() {
        assert_eq!(one_tag("from 192.168.1.42 ok"), RedactionTag::Ipv4);
    }

    #[test]
    fn detects_hex_secret_long_enough() {
        let secret: String = std::iter::repeat_n('a', 64).collect();
        let s = format!("key {secret} done");
        assert_eq!(one_tag(&s), RedactionTag::HexSecret);
    }

    #[test]
    fn ignores_short_hex() {
        // 16 chars — below the 32-char threshold.
        let r = redact("short cafebabecafebabe done");
        assert!(r.spans.is_empty(), "got spans: {:?}", r.spans);
    }

    // ── Multi-hit + ordering ─────────────────────────────────────────

    #[test]
    fn multiple_hits_are_left_to_right_and_non_overlapping() {
        let input = "alice@example.com and 192.168.1.1 and 123-45-6789";
        let r = redact(input);
        assert_eq!(r.spans.len(), 3);
        // Span starts strictly increase.
        for w in r.spans.windows(2) {
            assert!(w[0].end <= w[1].start, "overlap: {:?}", r.spans);
            assert!(w[0].start < w[1].start);
        }
        let tags: Vec<_> = r.spans.iter().map(|s| s.tag).collect();
        assert_eq!(
            tags,
            vec![RedactionTag::Email, RedactionTag::Ipv4, RedactionTag::Ssn]
        );
    }

    #[test]
    fn span_offsets_point_at_real_input_bytes() {
        let input = "x alice@example.com y";
        let r = redact(input);
        let s = &r.spans[0];
        assert_eq!(&input[s.start..s.end], "alice@example.com");
    }

    // ── Idempotence + bytes-only-inside-spans ───────────────────────

    #[test]
    fn redact_is_idempotent() {
        let input = "alice@example.com and 192.168.1.1";
        let once = redact(input);
        let twice = redact(&once.text);
        assert_eq!(twice.text, once.text);
        assert!(twice.spans.is_empty());
    }

    #[test]
    fn non_redacted_text_is_byte_preserved() {
        let input = "PREFIX alice@example.com SUFFIX";
        let r = redact(input);
        assert!(r.text.starts_with("PREFIX "));
        assert!(r.text.ends_with(" SUFFIX"));
    }

    // ── Audit metadata: no body bytes leak via Debug ───────────────────

    #[test]
    fn span_debug_does_not_contain_redacted_bytes() {
        let input = "secret token AKIAIOSFODNN7EXAMPLE here";
        let r = redact(input);
        let dbg = format!("{:?}", r.spans);
        assert!(
            !dbg.contains("AKIAIOSFODNN7EXAMPLE"),
            "span Debug leaked body: {dbg}"
        );
    }
}
