//! Offset translators between original-body and fenced-body coordinates.
//! Pure functions over `FenceMark` slices. See spec §5.2.3 of
//! `docs/superpowers/specs/2026-05-01-issue-74-llm-extractor-design.md`.

use crate::pipeline::extract::TextSpan;
use crate::pipeline::filter::fence::FenceMark;

const OPEN_LEN: usize = "<cairn:fenced>".len();
const CLOSE_LEN: usize = "</cairn:fenced>".len();

/// Cast a `usize` byte offset to `u32`, asserting it fits. Fenced bodies
/// are produced from bodies bounded by `MAX_BODY_LEN_FOR_REGEX` (64 KiB)
/// plus at most `marks.len() * (OPEN_LEN + CLOSE_LEN)` inserted bytes —
/// well within `u32::MAX`.
#[allow(clippy::expect_used)] // invariant: body + sentinel inserts << u32::MAX; tested by proptest
#[inline]
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).expect("invariant: fenced offset fits in u32")
}

/// Produced when a fenced span overlaps a fencer-emitted sentinel byte
/// range. The model was instructed not to quote across sentinels, so any
/// such span is invalid and dropped at the validation step.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpanMapError {
    /// The fenced span overlaps a fence sentinel byte range.
    #[error("fenced span overlaps a fence sentinel byte range")]
    OverlapsSentinel,
}

/// Translate one original-body span into the set of fenced-body spans
/// that cover the same original bytes, **excluding** the fencer's
/// inserted sentinel bytes.
///
/// An original span that overlaps text the fencer wrapped will be split
/// into multiple fenced sub-spans (pre-mark piece, wrapped piece,
/// post-mark piece). The wrapped piece's offsets sit between the open
/// and close sentinels in the fenced body — never overlapping the
/// sentinels themselves.
///
/// Returns an empty `Vec` iff the input span is empty (`start == end`)
/// or the span lies wholly outside the original body. Otherwise returns
/// a non-empty `Vec` whose union of byte ranges in fenced coordinates
/// covers exactly the original bytes (modulo sentinel insertions).
#[must_use]
pub fn original_to_fenced(span: TextSpan, marks: &[FenceMark]) -> Vec<TextSpan> {
    if span.is_empty() {
        return Vec::new();
    }

    let s = span.start as usize;
    let e = span.end as usize;

    let mut pieces: Vec<TextSpan> = Vec::new();
    // cumulative bytes inserted by marks fully processed before cursor
    let mut offset: usize = 0;
    let mut cursor: usize = s;

    for m in marks {
        // mark is fully before our cursor; both sentinels already counted
        if m.end <= cursor {
            offset += OPEN_LEN + CLOSE_LEN;
            continue;
        }
        // mark starts at or after span end — we're done with marks
        if m.start >= e {
            break;
        }

        // emit pre-mark piece if cursor is before m.start
        if cursor < m.start {
            let pre_end = m.start.min(e);
            pieces.push(TextSpan::new(
                to_u32(cursor + offset),
                to_u32(pre_end + offset),
            ));
            cursor = pre_end;
            if cursor >= e {
                break;
            }
        }

        // we are now at or past m.start in original coords.
        // The OPEN sentinel is inserted at fenced(m.start) = m.start + offset.
        // Bytes from m.start to min(m.end, e) live INSIDE the wrap.
        offset += OPEN_LEN;
        let inside_start = cursor.max(m.start);
        let inside_end = m.end.min(e);
        if inside_start < inside_end {
            pieces.push(TextSpan::new(
                to_u32(inside_start + offset),
                to_u32(inside_end + offset),
            ));
        }
        cursor = inside_end;

        // we have moved past the wrap; CLOSE has been inserted
        offset += CLOSE_LEN;

        if cursor >= e {
            break;
        }
    }

    // trailing piece after all consumed marks
    if cursor < e {
        pieces.push(TextSpan::new(to_u32(cursor + offset), to_u32(e + offset)));
    }

    pieces
}

/// Translate one fenced-body span back to its original-body coordinates.
///
/// Pre-condition: the input span lies wholly outside any
/// fencer-emitted sentinel byte range. Returns
/// `SpanMapError::OverlapsSentinel` for spans that contain or straddle
/// sentinels.
pub fn fenced_to_original(span: TextSpan, marks: &[FenceMark]) -> Result<TextSpan, SpanMapError> {
    let fs = span.start as usize;
    let fe = span.end as usize;

    // Walk marks; compute cumulative offset; check span doesn't overlap any sentinel.
    let mut accumulated: usize = 0;
    for m in marks {
        let open_at = m.start + accumulated;
        let open_end = open_at + OPEN_LEN;
        let close_at = m.end + accumulated + OPEN_LEN;
        let close_end = close_at + CLOSE_LEN;

        // OPEN occupies [open_at, open_end)
        // Check if span overlaps OPEN: span.start < open_end && open_at < span.end
        if fs < open_end && open_at < fe {
            return Err(SpanMapError::OverlapsSentinel);
        }
        // CLOSE occupies [close_at, close_end)
        // Check if span overlaps CLOSE: span.start < close_end && close_at < span.end
        if fs < close_end && close_at < fe {
            return Err(SpanMapError::OverlapsSentinel);
        }

        accumulated += OPEN_LEN + CLOSE_LEN;
    }

    // Compute the back-shift: for each mark, determine how many inserted bytes
    // precede fs.
    let mut subtract: usize = 0;
    let mut prefix: usize = 0;
    for m in marks {
        let open_at = m.start + prefix;
        let open_end = open_at + OPEN_LEN;
        let close_at = m.end + prefix + OPEN_LEN;
        let close_end = close_at + CLOSE_LEN;

        if close_end <= fs {
            // both OPEN and CLOSE are fully before span.start
            subtract += OPEN_LEN + CLOSE_LEN;
        } else if open_end <= fs && fe <= close_at {
            // span is entirely inside this wrap (between OPEN and CLOSE)
            subtract += OPEN_LEN;
        }
        // If OPEN is partially before and CLOSE is partially after,
        // the overlap check above already rejected it.

        prefix += OPEN_LEN + CLOSE_LEN;
    }

    Ok(TextSpan::new(to_u32(fs - subtract), to_u32(fe - subtract)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::filter::fence::{FenceMark, fence};

    fn mk(start: usize, end: usize) -> FenceMark {
        FenceMark { start, end }
    }

    #[test]
    fn no_marks_is_identity_in_both_directions() {
        let span = TextSpan::new(3, 7);
        assert_eq!(original_to_fenced(span, &[]), vec![span]);
        assert_eq!(fenced_to_original(span, &[]).unwrap(), span);
    }

    #[test]
    fn span_before_mark_unaffected() {
        // body "hello WORLD", mark wraps "WORLD" at [6, 11]
        let marks = vec![mk(6, 11)];
        let span = TextSpan::new(0, 5); // "hello"
        assert_eq!(original_to_fenced(span, &marks), vec![span]);
        assert_eq!(fenced_to_original(span, &marks).unwrap(), span);
    }

    #[test]
    fn span_after_mark_shifted_by_open_plus_close() {
        // body "WORLD then hi", mark wraps "WORLD" at [0, 5].
        // Fenced: "<cairn:fenced>WORLD</cairn:fenced> then hi"
        // Original byte at position 6 (' ' after WORLD) lands at 6 + OPEN.len() + CLOSE.len()
        let marks = vec![mk(0, 5)];
        let original = TextSpan::new(6, 13); // " then hi"
        let pieces = original_to_fenced(original, &marks);
        assert_eq!(pieces.len(), 1);
        assert_eq!(
            pieces[0],
            TextSpan::new(
                to_u32(6 + OPEN_LEN + CLOSE_LEN),
                to_u32(13 + OPEN_LEN + CLOSE_LEN)
            )
        );
        // Inverse direction: fenced span back to original
        assert_eq!(fenced_to_original(pieces[0], &marks).unwrap(), original);
    }

    #[test]
    fn span_inside_wrap_offsets_by_open_only() {
        // body "WORLD", mark wraps the whole thing at [0, 5].
        // Fenced: "<cairn:fenced>WORLD</cairn:fenced>"
        // Original byte at position 0 ('W') lands at OPEN_LEN; position 5 ('end') at OPEN_LEN+5.
        let marks = vec![mk(0, 5)];
        let original = TextSpan::new(0, 5);
        let pieces = original_to_fenced(original, &marks);
        assert_eq!(pieces.len(), 1);
        assert_eq!(
            pieces[0],
            TextSpan::new(to_u32(OPEN_LEN), to_u32(OPEN_LEN + 5))
        );
        // Inverse: fenced span back to original
        assert_eq!(fenced_to_original(pieces[0], &marks).unwrap(), original);
    }

    #[test]
    fn span_straddling_mark_split_into_three_pieces() {
        // body "abcWRAPPEDxyz", mark wraps "WRAPPED" at [3, 10].
        // Fenced: "abc<cairn:fenced>WRAPPED</cairn:fenced>xyz"
        // Original span [0, 13] (whole body) splits into:
        //   - "abc" at fenced [0, 3]
        //   - "WRAPPED" at fenced [3 + OPEN_LEN, 3 + OPEN_LEN + 7]
        //   - "xyz" at fenced [3 + OPEN_LEN + 7 + CLOSE_LEN, 13 + OPEN_LEN + CLOSE_LEN]
        let marks = vec![mk(3, 10)];
        let span = TextSpan::new(0, 13);
        let pieces = original_to_fenced(span, &marks);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0], TextSpan::new(0, 3));
        assert_eq!(
            pieces[1],
            TextSpan::new(to_u32(3 + OPEN_LEN), to_u32(3 + OPEN_LEN + 7))
        );
        assert_eq!(
            pieces[2],
            TextSpan::new(
                to_u32(3 + OPEN_LEN + 7 + CLOSE_LEN),
                to_u32(13 + OPEN_LEN + CLOSE_LEN)
            )
        );
    }

    #[test]
    fn fenced_span_overlapping_open_sentinel_is_err() {
        // mark wraps [3, 10] in body of length 13. Fenced OPEN sits at fenced [3, 3+OPEN_LEN].
        // A fenced span that includes any byte in that range must error.
        let marks = vec![mk(3, 10)];
        let bad = TextSpan::new(2, 5); // overlaps the OPEN sentinel
        assert_eq!(
            fenced_to_original(bad, &marks),
            Err(SpanMapError::OverlapsSentinel)
        );
    }

    #[test]
    fn fenced_span_overlapping_close_sentinel_is_err() {
        let marks = vec![mk(3, 10)];
        // CLOSE sits at fenced [3+OPEN_LEN+7, 3+OPEN_LEN+7+CLOSE_LEN]
        let bad = TextSpan::new(to_u32(3 + OPEN_LEN + 6), to_u32(3 + OPEN_LEN + 7 + 1));
        assert_eq!(
            fenced_to_original(bad, &marks),
            Err(SpanMapError::OverlapsSentinel)
        );
    }

    #[test]
    fn empty_span_returns_empty_vec_in_forward_direction() {
        let marks = vec![mk(2, 5)];
        let pieces = original_to_fenced(TextSpan::new(7, 7), &marks);
        assert!(pieces.is_empty());
    }

    use proptest::prelude::*;

    proptest! {
        // For any fenced body produced by the real fencer, every original-body span
        // can be forward-mapped to fenced pieces that, when inverse-mapped, recover
        // the original span.
        #[test]
        fn forward_inverse_round_trip(
            body in r"[A-Za-z .!?]{0,200}",
            start_frac in 0u32..200,
            end_frac in 0u32..200,
        ) {
            let payload = fence(&body);
            let body_len = body.len();
            if body_len == 0 { return Ok(()); }
            let s = (start_frac as usize) % (body_len + 1);
            let e = (end_frac as usize) % (body_len + 1);
            let (start, end) = (s.min(e), s.max(e));
            if start == end { return Ok(()); }
            let span = TextSpan::new(to_u32(start), to_u32(end));

            let pieces = original_to_fenced(span, &payload.marks);
            // Every piece, when inverse-mapped, lies within the original span and
            // none of them overlap a sentinel (because forward never produces those).
            let mut covered_original_bytes: Vec<bool> = vec![false; end - start];
            for p in &pieces {
                let back = fenced_to_original(*p, &payload.marks).unwrap();
                prop_assert!(back.start >= span.start && back.end <= span.end,
                    "back={:?} not in span={:?}", back, span);
                for i in (back.start as usize)..(back.end as usize) {
                    covered_original_bytes[i - start] = true;
                }
            }
            // The pieces collectively cover every byte of the original span
            // (excluding sentinel-emitted bytes — but those don't exist in the
            // original body, so coverage is total).
            for (i, c) in covered_original_bytes.iter().enumerate() {
                prop_assert!(*c, "byte {} of original span {:?} not covered by any piece", start + i, span);
            }
        }
    }
}
