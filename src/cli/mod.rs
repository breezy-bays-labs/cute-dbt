//! CLI surface: clap derive, run-loop composition, `ExitCode` mapping.
//!
//! v0.1 final wiring lands with PR 6 (#TBD). The clap surface parses
//! `--manifest <PATH>` (required), `--baseline-manifest <PATH>` (required),
//! `--out <PATH>`, and `--config <PATH>`; the run loop calls
//! `scope -> preflight_compiled -> parse_ctes -> render` as named call-sites.
//! Missing `--baseline-manifest` is a clap usage error raised before the
//! manifest is read — NOT a `PreflightError` variant (the `domain` module
//! lands the error type in PR 3).
//!
//! The placeholder [`run`] below exists so the bootstrap CI battery
//! (coverage gate, MSRV, clippy pedantic) can be green from PR 1.

use std::process::ExitCode;

/// Placeholder entry. PR 6 (#TBD) replaces this body with the named run
/// loop. Until then the binary exits success on a no-args invocation so
/// the smoke test covers `main` and the bootstrap coverage gate passes
/// at 100% on this surface.
#[must_use]
pub fn run() -> ExitCode {
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bootstrap-surface coverage: exercises the placeholder so the
    /// 85% coverage threshold passes on PR 1 (coverage-trap pattern).
    /// Replaced with real run-loop tests at PR 6.
    #[test]
    fn run_returns_success_without_panic() {
        // Using debug-format equality keeps the assertion meaningful even
        // when ExitCode adds variants in future Rust releases.
        assert_eq!(format!("{:?}", run()), format!("{:?}", ExitCode::SUCCESS));
    }
}
