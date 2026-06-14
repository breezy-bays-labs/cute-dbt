//! Fuzz target for cute-dbt's `--pr-diff` patch parser (cute-dbt#383).
//!
//! The `--pr-diff` unified-diff parser is cute-dbt's **highest-risk
//! untrusted-input surface**: in CI / PR-review mode the workflow runs
//! `git diff --unified=0 <base>...<head>` and hands cute-dbt the raw text,
//! so an attacker who can influence a PR's contents (or a malformed/huge
//! diff) feeds arbitrary bytes straight into the parser. This is the org
//! testing framework's named Q4 default blind spot, and the "single most
//! valuable Q4 move" per `.claude/rules/testing.md`.
//!
//! ## Harness — bolero on STABLE Rust (no nightly, no libFuzzer)
//!
//! [`bolero`] is chosen over `cargo-fuzz` because its `DefaultEngine`
//! mimics libtest and runs under plain `cargo test` / `cargo nextest` on
//! the crate's MSRV (1.88) — it replays the committed `corpus/` AND
//! generates fresh random inputs, with **no** `cargo-bolero` binary and
//! **no** nightly toolchain required. The target is `harness = false`
//! (bolero's `check!` owns `main()`), declared as the
//! `fuzz_pr_diff_parser` `[[test]]` in `Cargo.toml`.
//!
//! ## The fail-closed invariant under test
//!
//! cute-dbt's contract is *"never panics on a bad diff"* (see the panic
//! notes throughout `src/domain/pr_diff.rs`). The parser must, for **any**
//! byte sequence:
//!
//! - never panic, never hang, never OOM;
//! - return `Ok(PrDiff)` or `Err(String)` — both are fail-closed (an
//!   `Err` is a clap usage error → exit 2; an `Ok` with zero files is a
//!   zero-scope report);
//! - on `Ok`, emit a structurally well-formed [`PrDiff`]: every
//!   `added_lines`/`removed_lines` entry is free of embedded newlines
//!   (the line scanner splits on `\n`), and re-parsing the **same** input
//!   is deterministic (idempotent).
//!
//! ## Running it
//!
//! ```text
//! # Corpus replay + a short bounded random run (stable, in CI / locally):
//! cargo test --test fuzz_pr_diff_parser
//! BOLERO_RANDOM_ITERATIONS=200000 cargo test --test fuzz_pr_diff_parser
//!
//! # An extended campaign (optional; needs `cargo install cargo-bolero`):
//! cargo bolero test fuzz_pr_diff_parser --profile fuzz -T 60s
//! ```
//!
//! Newly-discovered interesting inputs land under
//! `tests/fuzz_pr_diff_parser/corpus/`; any crash lands under
//! `tests/fuzz_pr_diff_parser/crashes/`. Both are **committed** (the
//! repo's regression-files-committed rule, the same posture as
//! `proptest-regressions/`), so a reproducer travels with the repo and
//! replays as an ordinary test forever after.
//!
//! This is NOT a blocking merge gate — fuzz is schedule/manual. The
//! corpus-replay that `cargo test` performs is a cheap regression guard;
//! the open-ended search is run on demand.

use bolero::check;

fn main() {
    check!().for_each(|bytes: &[u8]| {
        // The parser consumes `&str`; real CLI input is UTF-8, but the
        // `@file` arm can hand it arbitrary file bytes, so model the
        // non-UTF-8 case with a lossy decode (U+FFFD for invalid runs).
        // Fuzzing the pure `parse_unified_diff` seam (NOT the public
        // `parse_diff`) keeps the fuzzed path free of `@file` filesystem
        // I/O — we are exercising the parser, not the OS.
        let text = String::from_utf8_lossy(bytes);

        // (1) Never panics / hangs / OOMs — the whole point. `check!`
        // catches a panic and records the input as a crash; an Err is a
        // perfectly valid fail-closed outcome, not a finding.
        let Ok(diff) = cute_dbt::cli::fuzz_parse_unified_diff(&text) else {
            return;
        };

        // (2) On success the POD is structurally well-formed: the line
        // scanner splits on `\n`, so no parsed body line may smuggle an
        // embedded newline back out (that would desync downstream
        // line-numbered reconstruction in `domain::pr_diff`).
        for file in &diff.files {
            assert!(
                !file.path.contains('\n'),
                "a parsed file path must not contain a newline: {:?}",
                file.path
            );
            for hunk in &file.hunks {
                for line in hunk.added_lines.iter().chain(&hunk.removed_lines) {
                    assert!(
                        !line.contains('\n'),
                        "a parsed hunk body line must not contain a newline: {line:?}",
                    );
                }
            }
        }
        for rename in &diff.renames {
            assert!(
                !rename.from.contains('\n') && !rename.to.contains('\n'),
                "a parsed rename path must not contain a newline: {rename:?}",
            );
        }
        for path in &diff.deleted {
            assert!(
                !path.contains('\n'),
                "a parsed deleted path must not contain a newline: {path:?}",
            );
        }

        // (3) Deterministic: parsing the same input twice yields the same
        // result. A parser with hidden state (or input-dependent
        // ordering) would be a real bug for a value-parser run once per
        // CLI invocation.
        let again = cute_dbt::cli::fuzz_parse_unified_diff(&text)
            .expect("a diff that parsed once parses again");
        assert_eq!(diff, again, "the parse must be deterministic");
    });
}
