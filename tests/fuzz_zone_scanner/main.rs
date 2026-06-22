//! Fuzz target for cute-dbt's raw-Jinja zone scanner (cute-dbt#464, Z3).
//!
//! `locate_raw_zones` (the hand-rolled `{%…%}`/`{{…}}`/`{#…#}` tag-boundary
//! scanner, `src/adapters/render.rs`) is cute-dbt's **second untrusted-input
//! parser** after the `--pr-diff` patch parser fuzzed in cute-dbt#383: a
//! malicious manifest can carry arbitrary bytes in a node's `raw_code`, fed
//! straight into the scanner. Hand-rolling it means cute-dbt OWNS the
//! whitespace-control (`{%-`/`-%}`), string-literal-aware `%}`-skipping, and
//! comment-swallowing edge cases minijinja's lexer would have given for free
//! (the raw-jinja-zones shaping §2.4 / §9). That makes it exactly the org
//! testing framework's named Q4 default blind spot — and the design lists this
//! fuzz target as a BLOCKING Z3 deliverable, not a deferred risk.
//!
//! ## Harness — bolero on STABLE Rust (no nightly, no libFuzzer)
//!
//! [`bolero`] is chosen over `cargo-fuzz` because its `DefaultEngine` mimics
//! libtest and runs under plain `cargo test` / `cargo nextest` on the crate's
//! MSRV (1.88) — it replays the committed `corpus/` AND generates fresh random
//! inputs, with **no** `cargo-bolero` binary and **no** nightly toolchain
//! required. The target is `harness = false` (bolero's `check!` owns `main()`),
//! declared as the `fuzz_zone_scanner` `[[test]]` in `Cargo.toml`, mirroring
//! `tests/fuzz_pr_diff_parser/main.rs`.
//!
//! ## The fail-closed invariant under test
//!
//! The scanner's contract is *"never panics on bad `raw_code`; a
//! malformed/unbalanced tag stream degrades to an EMPTY result, never a
//! fabricated zone"* (the §2.4 honesty backstop). For **any** byte sequence the
//! scan must:
//!
//! - never panic, never hang, never OOM;
//! - return a `Vec` of well-formed zones only (each emitted span is non-empty
//!   and in-bounds of the input; `start.byte < end.byte <= len`);
//! - be deterministic — scanning the **same** input twice yields the same
//!   result (a value-parser run once per render must not carry hidden state).
//!
//! ## Running it
//!
//! ```text
//! # Corpus replay + a short bounded random run (stable, in CI / locally):
//! cargo test --test fuzz_zone_scanner
//! BOLERO_RANDOM_ITERATIONS=200000 cargo test --test fuzz_zone_scanner
//!
//! # An extended campaign (optional; needs `cargo install cargo-bolero`):
//! cargo bolero test fuzz_zone_scanner --profile fuzz -T 60s
//! ```
//!
//! Newly-discovered interesting inputs land under
//! `tests/fuzz_zone_scanner/corpus/`; any crash lands under
//! `tests/fuzz_zone_scanner/crashes/`. Both are **committed** (the repo's
//! regression-files-committed rule, the same posture as `proptest-regressions/`
//! and the pr-diff fuzz corpus), so a reproducer travels with the repo and
//! replays as an ordinary test forever after.
//!
//! This is NOT a blocking merge gate — fuzz is schedule/manual. The
//! corpus-replay that `cargo test` performs is a cheap regression guard; the
//! open-ended search is run on demand.

use bolero::check;

fn main() {
    check!().for_each(|bytes: &[u8]| {
        // `raw_code` is UTF-8 dbt source on the happy path, but a hostile
        // manifest can smuggle invalid bytes; model that with a lossy decode
        // (U+FFFD for invalid runs) so the scanner sees `&str` exactly as the
        // render path hands it.
        let text = String::from_utf8_lossy(bytes);

        // (1) Never panics / hangs / OOMs — the whole point. `check!` catches a
        // panic and records the input as a crash. The fail-closed scanner has
        // no Err channel: it either emits located zones or an empty Vec, both
        // valid outcomes.
        let zones = cute_dbt::adapters::render::fuzz_locate_raw_zones(&text);

        // (2) Every emitted zone is structurally well-formed against the input:
        // a non-empty, in-bounds half-open raw span. The flattened POD is
        // `(kind_tag, start_byte, end_byte, block_id)` (byte offsets are `u32`,
        // matching the domain `SourcePos`).
        let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
        for (_kind, start, end, _block_id) in &zones {
            assert!(
                start < end,
                "an emitted zone span must be non-empty (start {start} < end {end})",
            );
            assert!(
                *end <= len,
                "an emitted zone span must stay in-bounds (end {end} <= len {len})",
            );
        }

        // (3) Deterministic: scanning the same input twice yields the same
        // result. A scanner with hidden state (or input-dependent ordering)
        // would be a real bug for a fn run once per model render.
        let again = cute_dbt::adapters::render::fuzz_locate_raw_zones(&text);
        assert_eq!(zones, again, "the scan must be deterministic");
    });
}
