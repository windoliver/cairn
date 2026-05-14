//! Frozen v1 artifacts of the replay-hash contract.
//!
//! Three pieces — projection, input struct, encoder — pinned forever
//! once shipped. Future `MemoryRecord` additions are ignored by
//! `project_v1`. Changes to canonical-JSON output rules require a
//! new version. Domain tag is `"cairn.replay_hash.v1"`.

use crate::domain::{Identity, MemoryRecord, SourceRef};

/// Frozen input struct hashed under v1. Field set is immutable.
pub struct InputV1<'a> {
    /// Schema version. Always `1` under v1.
    pub v: u32,
    /// Domain tag. Always `"cairn.replay_hash.v1"`.
    pub domain: &'static str,
    /// Canonical body bytes — the raw record body utf-8.
    pub body: &'a str,
    /// Sorted-unique source refs from the record's provenance.
    pub source_refs: &'a [SourceRef],
    /// Originating agent identity.
    pub originating_agent_id: &'a Identity,
    /// Source-sensor identity.
    pub source_sensor: &'a Identity,
}

/// Read only the fields v1 was minted with. Future `MemoryRecord`
/// fields are ignored — this projection MUST NOT evolve.
#[must_use]
pub fn project_v1(record: &MemoryRecord) -> InputV1<'_> {
    InputV1 {
        v: 1,
        domain: "cairn.replay_hash.v1",
        body: record.body.as_str(),
        source_refs: record.provenance.source_refs.as_slice(),
        originating_agent_id: &record.provenance.originating_agent_id,
        source_sensor: &record.provenance.source_sensor,
    }
}

/// Hand-rolled canonical-JSON encoder. Sort: byte-wise key ascending.
/// Strings: NFC-normalized, escapes per spec
/// (`\"`, `\\`, `\n`, `\r`, `\t`, `\b`, `\f`, `\u00XX` for control
/// chars). No insignificant whitespace.
#[must_use]
pub fn encode_v1(input: &InputV1<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    // Object key order: ascending byte-wise — sorted at compile time
    // ("body" < "domain" < "originating_agent_id" < "source_refs" <
    // "source_sensor" < "v").
    out.push(b'{');
    write_key(&mut out, "body");
    write_string(&mut out, input.body);
    out.push(b',');
    write_key(&mut out, "domain");
    write_string(&mut out, input.domain);
    out.push(b',');
    write_key(&mut out, "originating_agent_id");
    write_string(&mut out, input.originating_agent_id.as_str());
    out.push(b',');
    write_key(&mut out, "source_refs");
    write_source_refs(&mut out, input.source_refs);
    out.push(b',');
    write_key(&mut out, "source_sensor");
    write_string(&mut out, input.source_sensor.as_str());
    out.push(b',');
    write_key(&mut out, "v");
    write_u32(&mut out, input.v);
    out.push(b'}');
    out
}

fn write_key(out: &mut Vec<u8>, key: &str) {
    write_string(out, key);
    out.push(b':');
}

fn write_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(n.to_string().as_bytes());
}

fn write_source_refs(out: &mut Vec<u8>, refs: &[SourceRef]) {
    out.push(b'[');
    for (idx, r) in refs.iter().enumerate() {
        if idx > 0 {
            out.push(b',');
        }
        // SourceRef object: "hash" < "id" byte-wise.
        out.push(b'{');
        write_key(out, "hash");
        write_string(out, r.hash.as_str());
        out.push(b',');
        write_key(out, "id");
        write_string(out, r.id.as_str());
        out.push(b'}');
    }
    out.push(b']');
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    let normalized = nfc(s);
    out.push(b'"');
    for ch in normalized.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\x08' => out.extend_from_slice(b"\\b"),
            '\x0c' => out.extend_from_slice(b"\\f"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = std::io::Write::write_fmt(
                    &mut StdoutAdapter(out),
                    format_args!("\\u{:04x}", c as u32),
                );
            }
            c => {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                out.extend_from_slice(s.as_bytes());
            }
        }
    }
    out.push(b'"');
}

struct StdoutAdapter<'a>(&'a mut Vec<u8>);

impl std::io::Write for StdoutAdapter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    s.nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use sha2::Digest as _;

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("sha256:{:x}", sha2::Sha256::digest(bytes))
    }

    #[test]
    fn sample_record_projects_and_encodes() {
        let r = sample_record();
        let input = project_v1(&r);
        let bytes = encode_v1(&input);
        // Deterministic: encoding twice produces identical bytes.
        assert_eq!(bytes, encode_v1(&project_v1(&r)));
        // Output is valid JSON with the v1 domain tag inline.
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("canonical json");
        assert_eq!(parsed["v"], serde_json::json!(1));
        assert_eq!(parsed["domain"], serde_json::json!("cairn.replay_hash.v1"));
    }

    #[test]
    fn escape_boundary_control_chars_emit_u_escape() {
        let mut out = Vec::new();
        write_string(&mut out, "\u{0001}\u{007f}");
        assert_eq!(out, b"\"\\u0001\\u007f\"");
    }

    #[test]
    fn ascii_strings_pass_through_unescaped() {
        let mut out = Vec::new();
        write_string(&mut out, "hello-world");
        assert_eq!(out, b"\"hello-world\"");
    }

    #[test]
    fn nfc_normalization_collapses_decomposed_forms() {
        // "é" composed (1 codepoint) vs "e" + combining acute (2 codepoints).
        let composed = "\u{00e9}";
        let decomposed = "e\u{0301}";
        let mut a = Vec::new();
        let mut b = Vec::new();
        write_string(&mut a, composed);
        write_string(&mut b, decomposed);
        assert_eq!(a, b);
    }

    #[test]
    fn source_refs_serialize_with_sorted_object_keys() {
        let refs = vec![SourceRef {
            id: "sources/a.md".to_owned(),
            hash: "sha256:abcd".to_owned(),
        }];
        let mut out = Vec::new();
        write_source_refs(&mut out, &refs);
        // "hash" < "id" byte-wise.
        let s = String::from_utf8(out).expect("utf8");
        let hash_idx = s.find("\"hash\"").expect("hash key");
        let id_idx = s.find("\"id\"").expect("id key");
        assert!(hash_idx < id_idx, "object keys must be ascending: {s}");
    }

    /// Golden vector — a v1 hash this binary commits to forever.
    /// Computed from `sample_record()` at issue #257 land. If this
    /// test breaks, you have changed encoder/projection behaviour
    /// for v1 — bump to v2 instead.
    #[test]
    fn golden_v1_hash_for_sample_record() {
        let r = sample_record();
        let bytes = encode_v1(&project_v1(&r));
        let hash = sha256_hex(&bytes);
        // Recorded on first run; downstream tests use this as the
        // anchor for the journal's target-scope lookup.
        insta::assert_snapshot!("golden_sample_record_v1", hash);
    }
}
