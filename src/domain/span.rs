//! src/domain/span.rs — THE position/span vocabulary. Pure POD + serde
//! derive only (no parser, no I/O — `tests/domain_clean_arch.rs` gate).
//! Mirrors dbt-core `CodeLocation` + minijinja's 6-field span.

use serde::{Deserialize, Serialize};

/// A position in a text. 1-based line/col (sqlparser `Location`, dbt
/// `CodeLocation`, the JS `.code-gutter`); 0-based UTF-8 byte offset
/// (sliceable, the engine's `index`/`offset`). Carrying BOTH is the fusion
/// convention, not redundancy. `u32` (not usize) matches the wire and halves
/// payload size — 4 GB of compiled SQL is not a real model. The `usize → u32`
/// narrowing is NOT assumed: it is guarded by a single checked conversion +
/// `debug_assert!` at the ONE ingestion boundary in the engine where spans are
/// constructed (§5, FIX 6) — fusion widened minijinja `u16 → u32` for exactly
/// this silent-truncation class, so cute-dbt asserts the cast rather than
/// trusting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourcePos {
    /// 1-based line number.
    pub line: u32, // 1-based
    /// 1-based unicode-char column (matching dbt `CodeLocation`).
    pub col: u32, // 1-based (unicode-char column, matching dbt CodeLocation)
    /// 0-based UTF-8 byte offset.
    pub byte: u32, // 0-based UTF-8 offset
}

/// A source span. HALF-OPEN `[start, end)` on bytes (codespan / LSP / dbt
/// `Span` / Rust `Range`), with its 1-based line/col endpoints carried
/// alongside. ONE type for CTE bodies, raw zones, finding anchors, future
/// model/macro file positions and column spans. The line endpoints are
/// 1-based for the JS gutter — the ONE documented byte/line discontinuity,
/// ISOLATED here so it can never leak into a fourth bespoke type.
/// INVARIANT (fitness-gated, §8): start.byte <= end.byte; the byte slice is
/// valid UTF-8; the line/col endpoints agree with the byte endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    /// Inclusive start position.
    pub start: SourcePos,
    /// Exclusive end position.
    pub end: SourcePos,
}

impl SourceSpan {
    /// The half-open `[start.byte, end.byte)` byte range — slice the
    /// compiled SQL directly with `compiled[span.byte_range()]`. The bytes
    /// are widened back to `usize` at this single slicing boundary.
    #[must_use]
    pub fn byte_range(&self) -> std::ops::Range<usize> {
        self.start.byte as usize..self.end.byte as usize
    }

    /// Pure, domain-legal — mirrors dbt `Span::contains`. The zone→node
    /// binding (v1's ad-hoc render-edge arithmetic) becomes this.
    #[must_use]
    pub fn contains(&self, p: SourcePos) -> bool {
        self.contains_byte(p.byte)
    }
    /// HALF-OPEN byte containment: `byte ∈ [start.byte, end.byte)`.
    #[must_use]
    pub fn contains_byte(&self, byte: u32) -> bool {
        self.start.byte <= byte && byte < self.end.byte
    }
    /// HALF-OPEN range containment — `self` fully contains `other`'s
    /// `[start, end)`. Guarded against the degenerate/fallback span: an empty
    /// `other` (`start == end`) is contained iff its start is inside `self`,
    /// and the `end.byte == 0` fallback span can never underflow (no
    /// `end.byte - 1` arithmetic). This is the off-by-one-free primitive the
    /// S5 raw-zone presence read uses — never a hand-rolled `end.byte - 1`.
    #[must_use]
    pub fn contains_range(&self, other: &SourceSpan) -> bool {
        self.start.byte <= other.start.byte && other.end.byte <= self.end.byte
    }
    /// Half-open overlap test — `self` and `other` share at least one byte
    /// of their `[start, end)` ranges. Empty spans (`start == end`) never
    /// overlap anything (no underflow on the `end.byte == 0` fallback span).
    #[must_use]
    pub fn overlaps(&self, other: &SourceSpan) -> bool {
        self.start.byte < other.end.byte && other.start.byte < self.end.byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `SourcePos` carrying only a byte offset (line/col fixed) —
    /// the arithmetic primitives key entirely on `byte`, so the byte axis is
    /// the one the exhaustive enumeration walks.
    fn pos(byte: u32) -> SourcePos {
        SourcePos {
            line: 1,
            col: byte + 1,
            byte,
        }
    }

    fn span(start: u32, end: u32) -> SourceSpan {
        SourceSpan {
            start: pos(start),
            end: pos(end),
        }
    }

    #[test]
    fn serde_round_trip_source_pos() {
        let p = SourcePos {
            line: 7,
            col: 12,
            byte: 240,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: SourcePos = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn serde_round_trip_source_span() {
        let s = SourceSpan {
            start: SourcePos {
                line: 3,
                col: 1,
                byte: 40,
            },
            end: SourcePos {
                line: 5,
                col: 2,
                byte: 90,
            },
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SourceSpan = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn contains_uses_the_byte_axis() {
        // `contains(pos)` delegates to `contains_byte(pos.byte)` — the
        // line/col fields are deliberately ignored.
        let s = span(10, 20);
        // A position whose byte is inside but whose line/col are nonsense
        // is still contained; a position outside the byte range is not.
        assert!(s.contains(SourcePos {
            line: 999,
            col: 999,
            byte: 15
        }));
        assert!(!s.contains(SourcePos {
            line: 1,
            col: 11,
            byte: 20
        }));
    }

    /// EXHAUSTIVE enumeration (house style — no proptest). Over the bounded
    /// byte cube `[0, N]^4` we assert each primitive against its
    /// mathematical definition, which is strictly stronger than sampling for
    /// this finite space.
    #[test]
    fn span_arithmetic_exhaustive() {
        const N: u32 = 6;
        for s0 in 0..=N {
            for s1 in 0..=N {
                // half-open self-span [s0, s1)
                let s = span(s0, s1);

                // contains_byte: b ∈ [s0, s1)
                for b in 0..=N {
                    assert_eq!(
                        s.contains_byte(b),
                        s0 <= b && b < s1,
                        "contains_byte({b}) for span [{s0},{s1})"
                    );
                    assert_eq!(
                        s.contains(pos(b)),
                        s.contains_byte(b),
                        "contains == contains_byte for span [{s0},{s1}) byte {b}"
                    );
                }

                for o0 in 0..=N {
                    for o1 in 0..=N {
                        let o = span(o0, o1);

                        // contains_range: s0 <= o0 && o1 <= s1
                        assert_eq!(
                            s.contains_range(&o),
                            s0 <= o0 && o1 <= s1,
                            "contains_range([{o0},{o1}) in [{s0},{s1}))"
                        );

                        // overlaps: s0 < o1 && o0 < s1
                        assert_eq!(
                            s.overlaps(&o),
                            s0 < o1 && o0 < s1,
                            "overlaps([{s0},{s1}) , [{o0},{o1}))"
                        );

                        // overlaps is symmetric across the byte axis.
                        assert_eq!(
                            s.overlaps(&o),
                            o.overlaps(&s),
                            "overlaps symmetric [{s0},{s1}) vs [{o0},{o1})"
                        );
                    }
                }
            }
        }
    }

    /// EMPTY-SPAN GUARD (feedback refinement 4): the `end.byte == 0`
    /// fallback span and empty (`start == end`) spans must NOT underflow and
    /// must satisfy the documented half-open semantics.
    #[test]
    fn empty_span_guard_no_underflow() {
        // The degenerate fallback span: start == end == 0.
        let zero = span(0, 0);
        // An empty span contains no byte (half-open [0,0) is empty).
        for b in 0..=4 {
            assert!(
                !zero.contains_byte(b),
                "empty [0,0) contains no byte (b={b})"
            );
        }
        // It overlaps nothing — including itself.
        assert!(!zero.overlaps(&zero), "empty span overlaps nothing");
        assert!(
            !zero.overlaps(&span(0, 3)),
            "empty [0,0) does not overlap [0,3)"
        );

        // An empty `other` (start == end) is CONTAINED iff its start is
        // inside `self`'s [start, end] (inclusive on the end because
        // o.end == o.start and the test is `o.end <= self.end`).
        let outer = span(2, 6);
        // empty at byte 2 (== outer.start): contained.
        assert!(
            outer.contains_range(&span(2, 2)),
            "empty other at outer.start is contained"
        );
        // empty at byte 4 (interior): contained.
        assert!(
            outer.contains_range(&span(4, 4)),
            "empty other interior is contained"
        );
        // empty at byte 6 (== outer.end): contained (o.end <= self.end).
        assert!(
            outer.contains_range(&span(6, 6)),
            "empty other at outer.end is contained"
        );
        // empty at byte 1 (before start): NOT contained.
        assert!(
            !outer.contains_range(&span(1, 1)),
            "empty other before start is not contained"
        );
        // empty at byte 7 (past end): NOT contained.
        assert!(
            !outer.contains_range(&span(7, 7)),
            "empty other past end is not contained"
        );
    }

    #[test]
    fn byte_range_slices_the_source() {
        let src = "select id from t";
        let s = span(7, 9); // "id"
        assert_eq!(&src[s.byte_range()], "id");
        assert_eq!(s.byte_range(), 7..9);
    }

    #[test]
    fn contains_range_reflexive_and_full() {
        let s = span(3, 9);
        assert!(s.contains_range(&s), "a span contains itself");
        assert!(s.contains_range(&span(4, 8)), "contains a strict sub-range");
        assert!(
            !s.contains_range(&span(2, 9)),
            "does not contain a left-overflow"
        );
        assert!(
            !s.contains_range(&span(3, 10)),
            "does not contain a right-overflow"
        );
    }
}
