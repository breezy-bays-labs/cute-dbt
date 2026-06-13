//! Cross-platform test-support entrypoint (cute-dbt#331).
//!
//! The `cute-dbt review` subprocess integration suite spawns stand-in
//! `dbt`/`gh` tools on a controlled PATH so a developer's real toolchain
//! can never answer a test. Those stand-ins were `#!/bin/sh` shim scripts
//! made executable with `chmod 0o755` — a Unix-only mechanism (Windows'
//! `CreateProcess` does not honour shebangs), so the whole suite was
//! `#[cfg(unix)]`-gated.
//!
//! This module turns the **already-built `cute-dbt` binary itself** into
//! the stand-in: the harness copies `cute-dbt` (Cargo's
//! `CARGO_BIN_EXE_cute-dbt`, with the right extension per OS) into the
//! shim dir as `dbt`/`gh` and drops a sibling `<tool>.spec.toml`. When
//! the binary is started **under any name other than `cute-dbt`** and a
//! matching sibling spec exists, it runs as that fake tool instead of the
//! real CLI — a compiled, cross-platform helper that reads an argv/spec
//! behaviour contract, exactly as cute-dbt#331's Discovery hypothesis
//! proposed. No second published binary, no per-OS shell-syntax twins,
//! and no env var (the trigger is the argv[0] name, which the inherited
//! environment of a `review`-spawned child carries for free).
//!
//! The trigger is deliberately narrow: the production binary is always
//! invoked as `cute-dbt`, so `cute-dbt … report/review/explore` runs
//! never enter here — only a renamed copy with a sibling spec does.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;

/// One dispatch rule: when the invocation's leading arguments equal
/// [`Rule::when`] (joined by a single space), emit [`Rule::stdout`] /
/// [`Rule::stderr`] verbatim and exit with [`Rule::exit`].
#[derive(Debug, Deserialize)]
struct Rule {
    /// The space-joined argument prefix to match (e.g. `"--version"` or
    /// `"pr view"`). Matched against the same number of leading argv
    /// words, so `"pr view"` matches `gh pr view <n> --json …`.
    when: String,
    /// Bytes written verbatim to stdout (TOML multiline strings preserve
    /// the exact payload, including embedded newlines).
    #[serde(default)]
    stdout: String,
    /// Bytes written verbatim to stderr.
    #[serde(default)]
    stderr: String,
    /// The process exit code for this rule.
    exit: u8,
}

/// A fake-tool behaviour contract: an ordered rule list plus the exit
/// code used when no rule matches. Authored by the harness's typed shim
/// builders so each spec reproduces the exact byte contract of the
/// `#!/bin/sh` shim it replaces.
#[derive(Debug, Deserialize)]
struct FakeToolSpec {
    /// Dispatch rules, tried in order; the first whose [`Rule::when`]
    /// prefix matches wins.
    #[serde(default)]
    rules: Vec<Rule>,
    /// Exit code when no rule matches (the sh shims' trailing `exit N`).
    default_exit: u8,
}

/// Whether the current process should act as a fake `dbt`/`gh` tool,
/// returning the resolved spec path when so. True exactly when argv[0]'s
/// file stem is something **other than `cute-dbt`** (a renamed copy the
/// harness installed) and a sibling `<stem>.spec.toml` exists next to the
/// running executable. A cheap probe run once at `main` start; the common
/// case (real `cute-dbt`) returns `None` before any filesystem touch.
#[must_use]
pub fn requested() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let stem = exe.file_stem()?;
    // Case-insensitive: Windows executable names are case-insensitive, so
    // a `Cute-Dbt.exe` launch must still be recognized as the real CLI and
    // never fall into the fake-tool branch.
    if stem.eq_ignore_ascii_case("cute-dbt") {
        return None;
    }
    let stem = stem.to_str()?;
    let spec = exe.with_file_name(format!("{stem}.spec.toml"));
    spec.is_file().then_some(spec)
}

/// Run as the fake `dbt`/`gh` tool against `spec_path`.
///
/// Mirrors the retired `#!/bin/sh` shim contract: (1) append one
/// `cwd=… args=…` line to the per-tool invocation log so tests can assert
/// the planned-argv == executed-argv at the subprocess level, then
/// (2) dispatch on the leading arguments to emit the configured
/// stdout/stderr and exit code. A missing/unparseable spec exits non-zero
/// (a test-harness bug should fail loudly, never masquerade as a passing
/// tool).
#[must_use]
pub fn run(spec_path: &Path) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log_invocation(spec_path, &args);

    let Ok(raw) = std::fs::read_to_string(spec_path) else {
        eprintln!(
            "cute-dbt fake-tool: cannot read spec {}",
            spec_path.display()
        );
        return ExitCode::from(127);
    };
    let spec: FakeToolSpec = match toml::from_str(&raw) {
        Ok(spec) => spec,
        Err(err) => {
            eprintln!("cute-dbt fake-tool: malformed spec: {err}");
            return ExitCode::from(127);
        }
    };

    for rule in &spec.rules {
        if matches_prefix(&rule.when, &args) {
            // Raw-byte writes: the shim contract is byte-exact (engine
            // version banners, JSON), so go through stdout/stderr handles
            // rather than `print!`, which could newline-translate.
            let _ = std::io::stdout().write_all(rule.stdout.as_bytes());
            let _ = std::io::stderr().write_all(rule.stderr.as_bytes());
            return ExitCode::from(rule.exit);
        }
    }
    ExitCode::from(spec.default_exit)
}

/// Whether `when` (a space-joined argument prefix) matches the leading
/// words of `args`. An empty `when` matches any invocation (the
/// catch-all rule). Trailing args beyond the prefix are ignored — the sh
/// shims dispatched on `$1` / `"$1 $2"` exactly this way.
fn matches_prefix(when: &str, args: &[String]) -> bool {
    let wanted: Vec<&str> = if when.is_empty() {
        Vec::new()
    } else {
        when.split(' ').collect()
    };
    if wanted.len() > args.len() {
        return false;
    }
    wanted
        .iter()
        .zip(args.iter())
        .all(|(want, got)| want == got)
}

/// Append one `cwd=… args=…` line to `<spec-dir>/<tool>-invocations.log`,
/// where `<tool>` is this executable's file stem (the installed shim name
/// — `dbt`/`gh`). Best-effort: a log write failure must not change the
/// tool's exit behaviour (the dispatch is the contract; the log is an
/// assertion aid). Resolving the stem from `current_exe` (not argv[0])
/// keeps the name stable across platforms' differing argv[0] conventions.
fn log_invocation(spec_path: &Path, args: &[String]) {
    let Some(dir) = spec_path.parent() else {
        return;
    };
    let tool = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "tool".to_owned());
    // Log the CANONICAL cwd so the planned-vs-executed assertions, which
    // compare against `canonicalize(repo.root)`, match on every platform:
    // macOS tmpdirs traverse the /var symlink, and Windows canonicalize
    // emits a `\\?\` verbatim prefix — canonicalizing both sides aligns
    // them. Fall back to the raw cwd if canonicalize fails.
    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let line = format!("cwd={} args={}\n", cwd.display(), args.join(" "));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{tool}-invocations.log")))
    {
        let _ = f.write_all(line.as_bytes());
    }
}
