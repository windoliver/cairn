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
                build(
                    r"\b(?:sk_live|sk_test|pk_live|pk_test|rk_live|whsec)_[A-Za-z0-9]{16,128}\b",
                ),
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
                // Context-keyed secret assignments: `api_key=...`,
                // `secret: ...`, `token=...`, `password=...`. The
                // value is bounded to non-whitespace/quote characters
                // so we don't swallow surrounding prose.
                RedactionTag::ContextKeyedSecret,
                build(
                    r#"(?i)\b(?:api[_-]?key|secret|token|password|passwd|pwd|auth)\s*[:=]\s*['"]?[A-Za-z0-9_./+=-]{16,200}"#,
                ),
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
        // 36-char body satisfies the new GitHub token shape.
        let s = format!("token ghp_{} done", "a".repeat(36));
        assert_eq!(one_tag(&s), RedactionTag::GithubToken);
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
        assert_eq!(
            one_tag("token sk_live_aB3dEfGhIjKlMnOpQrStUv done"),
            RedactionTag::OpaqueApiKey
        );
    }

    #[test]
    fn detects_google_api_key() {
        // Exactly 35 chars after `AIza`.
        let key: String = std::iter::repeat_n('A', 35).collect();
        assert_eq!(
            one_tag(&format!("token AIza{key} done")),
            RedactionTag::OpaqueApiKey,
        );
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
