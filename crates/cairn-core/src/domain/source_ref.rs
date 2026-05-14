//! `SourceRef` — logical pointer from a [`crate::domain::Provenance`] to the
//! source bytes a record was derived from (brief §6.5).
//!
//! `SourceRef.id` is a **logical key**, not a filesystem path. The mapping
//! from `id` to bytes lives behind the [`crate::contract::source_resolver`]
//! trait and depends on the configured vault layout (brief §3 — folder names
//! like `sources/` are configurable). `SourceRef.hash` is the same
//! `"<algo>:<hex>"` format used by [`crate::domain::Provenance::source_hash`]:
//! `algo ∈ {sha256, sha512, blake3}`.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Logical link from a record's provenance to a source under the vault.
///
/// Vectors of `SourceRef` carry a uniqueness-and-ordering invariant
/// enforced by [`validate_source_refs`]: entries are sorted ascending by
/// `id` byte-wise, and `id` is unique within the vector. This is what lets
/// callers that still read the scalar
/// [`crate::domain::Provenance::source_hash`] see a deterministic primary
/// source identity rather than an arbitrary one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    /// Logical identifier, opaque to lint. Must be non-empty, contain no
    /// `..` segments, no leading `/`, and no embedded NUL byte.
    pub id: String,
    /// `"<algo>:<hex>"`. Same hash space as
    /// [`crate::domain::Provenance::source_hash`].
    pub hash: String,
}

impl SourceRef {
    /// Validate the structural shape of a single entry.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::MissingProvenance`] when `id` or `hash` is
    /// empty, malformed, or contains a forbidden segment.
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_id(&self.id)?;
        validate_hash(&self.hash)?;
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<(), DomainError> {
    if id.is_empty() {
        return Err(DomainError::MissingProvenance {
            field: "source_refs[].id",
        });
    }
    if id.starts_with('/') {
        return Err(DomainError::MissingProvenance {
            field: "source_refs[].id",
        });
    }
    if id.contains('\0') {
        return Err(DomainError::MissingProvenance {
            field: "source_refs[].id",
        });
    }
    for segment in id.split('/') {
        if segment == ".." {
            return Err(DomainError::MissingProvenance {
                field: "source_refs[].id",
            });
        }
    }
    Ok(())
}

fn validate_hash(raw: &str) -> Result<(), DomainError> {
    if raw.is_empty() {
        return Err(DomainError::MissingProvenance {
            field: "source_refs[].hash",
        });
    }
    let Some((algo, hex)) = raw.split_once(':') else {
        return Err(DomainError::MissingProvenance {
            field: "source_refs[].hash",
        });
    };
    let expected_len = match algo {
        "sha256" | "blake3" => 64,
        "sha512" => 128,
        _ => {
            return Err(DomainError::MissingProvenance {
                field: "source_refs[].hash",
            });
        }
    };
    if hex.len() != expected_len {
        return Err(DomainError::MissingProvenance {
            field: "source_refs[].hash",
        });
    }
    if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(DomainError::MissingProvenance {
            field: "source_refs[].hash",
        });
    }
    Ok(())
}

/// Validate a `source_refs` vector: every entry is structurally valid,
/// the vector is sorted ascending by `id` byte-wise, and ids are unique.
///
/// Empty vectors are accepted — emptiness is a lint concern
/// (`source_link_missing`), not a domain-level structural error, so
/// synthetic and historical records remain constructible.
///
/// # Errors
///
/// Returns [`DomainError::MissingProvenance`] for the offending field
/// when any invariant is violated.
pub fn validate_source_refs(refs: &[SourceRef]) -> Result<(), DomainError> {
    for r in refs {
        r.validate()?;
    }
    for pair in refs.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        match a.id.as_bytes().cmp(b.id.as_bytes()) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => {
                return Err(DomainError::MissingProvenance {
                    field: "source_refs[].id (duplicate)",
                });
            }
            std::cmp::Ordering::Greater => {
                return Err(DomainError::MissingProvenance {
                    field: "source_refs (unsorted)",
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SourceRef {
        SourceRef {
            id: "sources/log/2026-05-10.md".to_owned(),
            hash: format!("sha256:{}", "a".repeat(64)),
        }
    }

    #[test]
    fn valid_round_trips() {
        let s = sample();
        s.validate().expect("valid");
        let j = serde_json::to_string(&s).expect("ser");
        let back: SourceRef = serde_json::from_str(&j).expect("de");
        assert_eq!(s, back);
    }

    #[test]
    fn rejects_empty_id() {
        let mut s = sample();
        s.id.clear();
        assert!(matches!(
            s.validate(),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].id"
            })
        ));
    }

    #[test]
    fn rejects_leading_slash_id() {
        let mut s = sample();
        s.id = "/etc/passwd".to_owned();
        assert!(matches!(
            s.validate(),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].id"
            })
        ));
    }

    #[test]
    fn rejects_dotdot_segment() {
        let mut s = sample();
        s.id = "sources/../etc/passwd".to_owned();
        assert!(matches!(
            s.validate(),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].id"
            })
        ));
    }

    #[test]
    fn rejects_nul_byte() {
        let mut s = sample();
        s.id = "sources/log\0evil".to_owned();
        assert!(matches!(
            s.validate(),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].id"
            })
        ));
    }

    #[test]
    fn rejects_unstructured_hash() {
        let mut s = sample();
        s.hash = "not-a-hash".to_owned();
        assert!(matches!(
            s.validate(),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].hash"
            })
        ));
    }

    #[test]
    fn rejects_unknown_algo() {
        let mut s = sample();
        s.hash = format!("md5:{}", "a".repeat(32));
        assert!(matches!(
            s.validate(),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].hash"
            })
        ));
    }

    #[test]
    fn rejects_wrong_hex_length() {
        let mut s = sample();
        s.hash = format!("sha256:{}", "a".repeat(63));
        assert!(matches!(
            s.validate(),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].hash"
            })
        ));
    }

    #[test]
    fn accepts_sha512_and_blake3() {
        let mut s = sample();
        s.hash = format!("sha512:{}", "a".repeat(128));
        s.validate().expect("sha512");
        s.hash = format!("blake3:{}", "a".repeat(64));
        s.validate().expect("blake3");
    }

    #[test]
    fn vector_validate_empty_ok() {
        validate_source_refs(&[]).expect("empty allowed");
    }

    #[test]
    fn vector_validate_single_ok() {
        validate_source_refs(&[sample()]).expect("single");
    }

    #[test]
    fn vector_validate_sorted_unique_ok() {
        let a = SourceRef {
            id: "a".into(),
            hash: format!("sha256:{}", "a".repeat(64)),
        };
        let b = SourceRef {
            id: "b".into(),
            hash: format!("sha256:{}", "b".repeat(64)),
        };
        validate_source_refs(&[a, b]).expect("sorted unique");
    }

    #[test]
    fn vector_validate_rejects_unsorted() {
        let a = SourceRef {
            id: "b".into(),
            hash: format!("sha256:{}", "a".repeat(64)),
        };
        let b = SourceRef {
            id: "a".into(),
            hash: format!("sha256:{}", "b".repeat(64)),
        };
        assert!(matches!(
            validate_source_refs(&[a, b]),
            Err(DomainError::MissingProvenance {
                field: "source_refs (unsorted)"
            })
        ));
    }

    #[test]
    fn vector_validate_rejects_duplicate() {
        let a = SourceRef {
            id: "x".into(),
            hash: format!("sha256:{}", "a".repeat(64)),
        };
        let b = SourceRef {
            id: "x".into(),
            hash: format!("sha256:{}", "b".repeat(64)),
        };
        assert!(matches!(
            validate_source_refs(&[a, b]),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].id (duplicate)"
            })
        ));
    }

    #[test]
    fn vector_validate_propagates_entry_error() {
        let bad = SourceRef {
            id: String::new(),
            hash: format!("sha256:{}", "a".repeat(64)),
        };
        assert!(matches!(
            validate_source_refs(&[bad]),
            Err(DomainError::MissingProvenance {
                field: "source_refs[].id"
            })
        ));
    }
}
