//! Shared helpers for the BDD outer-loop (`tests/bdd.rs`) and the
//! resource-ref lint (`tests/resource_ref_lint.rs`).
//!
//! Each test binary that uses this module declares `mod common;` (via
//! `#[path = "common/mod.rs"]` for the bdd binary, which lives at
//! `tests/bdd.rs`).
//!
//! The fast integration tests in `tests/run_loop.rs` and friends still
//! carry their own copies of `fixture()` / `tmp()` / `run()` so each
//! reads top-to-bottom; a future PR can consolidate by switching them
//! to `mod common;` too.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Absolute path to a committed fixture under `tests/fixtures/`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Every committed example REPORT HTML under `examples/`. Adding a new
/// `examples/<name>-report.html` requires appending its filename here
/// so the zero-egress gates (`tests/headless_zero_egress.rs` and
/// `tests/resource_ref_lint.rs`) AND the `.github/workflows/ci.yml`
/// `example-report-check` matrix both pick it up. Single source of
/// truth — duplicating this list across test files is the kind of
/// silent gap an audit gate catches months too late.
///
/// Report pages only: the headless gate applies the report's
/// Mermaid + DataTables liveness oracle to every entry. The explore
/// pages live in [`COMMITTED_EXPLORE_PAGES`] with page-aware oracles.
pub const COMMITTED_EXAMPLES: &[&str] = &["jaffle-shop-report.html", "playground-report.html"];

/// The committed `cute-dbt explore` example pages under `examples/`
/// (cute-dbt#100), rendered from the synthetic playground fixture by
/// the `example-report-check` explore matrix rows. Both gates scan
/// them: the resource-ref lint uniformly, the headless gate with a
/// **page-aware liveness oracle** (`dag.html` / `macro.html` wait for
/// the Cytoscape canvas; `tests.html` is a static server-rendered page
/// asserted on DOM facts — it carries no Cytoscape and no DataTables, so
/// the canvas liveness probe must never be applied to it).
///
/// `explore-macro/macro.html` (cute-dbt#345) is the focused macro DAG,
/// rendered with `--pr-diff` against `playground-macro-pr-diff.patch` (a
/// root macro `quarantine_filter` change) by its own matrix row — the
/// no-`--pr-diff` `explore` row can never emit it.
pub const COMMITTED_EXPLORE_PAGES: &[&str] = &[
    "explore/dag.html",
    "explore/tests.html",
    "explore-macro/macro.html",
];

/// Absolute path to a committed example HTML under `examples/`.
pub fn example_path(filename: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(filename)
}

/// A path inside the cargo-provided integration-test temp directory.
pub fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// Best-effort delete so a re-run starts from a known-absent file.
pub fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Stringify a path argument (every test path is valid UTF-8).
pub fn s(path: &Path) -> &str {
    path.to_str().expect("test paths are valid UTF-8")
}

/// Run the `cute-dbt` binary with `args`; return its captured output.
pub fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args(args)
        .output()
        .expect("the cute-dbt binary spawns")
}

// ===== Temp git repos for the `review` verb (cute-dbt#300) ==========
//
// Real `git init` repos under CARGO_TARGET_TMPDIR, fully isolated from
// the developer's git environment: an empty file stands in for the
// global AND system gitconfig (a host `commit.gpgsign = true`, a
// `diff.noprefix = true`, or a global `cute-dbt.base` must never steer
// a test), and identity comes from explicit env vars. The SAME
// isolation wraps the spawned `cute-dbt review` subprocess, because the
// binary itself shells out to git.
//
// cute-dbt#331 — the shim harness is cross-platform. Stand-in `dbt`/`gh`
// tools are no longer `#!/bin/sh` scripts (Unix-only: Windows'
// CreateProcess ignores shebangs); each shim is a hard link to (copy
// fallback) the already-built `cute-dbt` binary, installed under the tool
// name (`dbt`/`gh`, with the platform exe suffix) beside a
// `<tool>.spec.toml` behaviour contract. The renamed binary detects the
// sibling spec at startup and acts as that fake tool (see
// `src/fake_tool.rs`). The controlled PATH and the hermetic git resolution
// below are OS-aware: the PATH separator/system tail differ per platform,
// and `git` is symlinked into the shim dir on Unix but reached via its own
// install dir on Windows (Git for Windows' `git.exe` needs its tree). So
// this block — and the subprocess suites that consume it — now run on
// windows-latest too (cute-dbt#308/#316/#317). Re-exported below so callers
// keep using `common::TestRepo` / `common::scrub_git_env` /
// `common::ShimSpec`.
mod review_harness {
    use super::{Output, PathBuf, fixture, tmp};
    use std::path::Path;
    use std::process::Command;

    /// The platform executable suffix (`.exe` on Windows, empty on Unix).
    fn exe_suffix() -> &'static str {
        if cfg!(windows) { ".exe" } else { "" }
    }

    /// PATH entries appended after the shim dir on the controlled (non-
    /// hermetic) PATH: just enough system dirs for `git` to resolve. The
    /// GitHub windows-latest runner's Git for Windows lives under
    /// `C:\Program Files\Git\...`; the workflow runs under Git Bash, whose
    /// `git`/`sh` resolve via the inherited process PATH, so on Windows we
    /// append the inherited PATH wholesale rather than guess system dirs
    /// (a developer's real `dbt`/`gh` is still shadowed by the shim dir,
    /// which is searched FIRST). On Unix the classic `/usr/bin:/bin` is
    /// sufficient and keeps a developer's homebrew/venv `dbt` out.
    fn system_path_tail() -> String {
        if cfg!(windows) {
            std::env::var("PATH").unwrap_or_default()
        } else {
            "/usr/bin:/bin".to_owned()
        }
    }

    /// Join PATH entries with the platform separator (`;` on Windows,
    /// `:` on Unix).
    fn join_path(dirs: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs.iter().map(|d| d.as_os_str())).expect("join PATH dirs")
    }

    /// A stand-in tool's behaviour contract — the cross-platform
    /// replacement for a `#!/bin/sh` shim body. Built fluently, then
    /// serialized to the `<tool>.spec.toml` the fake-tool reads
    /// (`src/fake_tool.rs`). Each rule's `stdout`/`stderr` are emitted
    /// byte-for-byte, so a spec reproduces the exact contract of the sh
    /// shim it replaces.
    #[derive(Debug, Default, Clone)]
    pub struct ShimSpec {
        rules: Vec<ShimRule>,
        default_exit: u8,
    }

    #[derive(Debug, Clone)]
    struct ShimRule {
        when: String,
        stdout: String,
        stderr: String,
        exit: u8,
    }

    impl ShimSpec {
        /// An empty spec: no rules, exit `default_exit` on every call.
        #[must_use]
        pub fn new(default_exit: u8) -> Self {
            Self {
                rules: Vec::new(),
                default_exit,
            }
        }

        /// Add a rule: when the invocation's leading args equal `when`
        /// (space-joined, e.g. `"--version"` or `"pr view"`), write
        /// `stdout` then `stderr` verbatim and exit `exit`.
        #[must_use]
        pub fn rule(mut self, when: &str, stdout: &str, stderr: &str, exit: u8) -> Self {
            self.rules.push(ShimRule {
                when: when.to_owned(),
                stdout: stdout.to_owned(),
                stderr: stderr.to_owned(),
                exit,
            });
            self
        }

        /// A `--version` rule emitting `banner` to stdout, exit 0 — the
        /// common engine-detection shape.
        #[must_use]
        pub fn version(self, banner: &str) -> Self {
            self.rule("--version", banner, "", 0)
        }

        /// A `compile` rule that succeeds with no output (the scaffolded
        /// manifest already plays the compiled artifact).
        #[must_use]
        pub fn compile_ok(self) -> Self {
            self.rule("compile", "", "", 0)
        }

        /// Serialize to the TOML spec format the fake-tool reads. Uses
        /// `toml`'s own string escaping so any byte (newlines, quotes,
        /// JSON braces) round-trips exactly.
        fn to_toml(&self) -> String {
            use std::fmt::Write as _;
            let mut out = format!("default_exit = {}\n", self.default_exit);
            for r in &self.rules {
                out.push_str("\n[[rules]]\n");
                let _ = writeln!(out, "when = {}", toml_str(&r.when));
                let _ = writeln!(out, "stdout = {}", toml_str(&r.stdout));
                let _ = writeln!(out, "stderr = {}", toml_str(&r.stderr));
                let _ = writeln!(out, "exit = {}", r.exit);
            }
            out
        }
    }

    /// Encode a string as a TOML string literal via the `toml` crate's own
    /// serializer, so every byte — quotes, backslashes, newlines, and any
    /// control character — round-trips correctly into the
    /// [`crate::common`]-written spec that `src/fake_tool.rs` parses back
    /// with the same crate (gemini review on cute-dbt#347).
    fn toml_str(s: &str) -> String {
        toml::Value::String(s.to_owned()).to_string()
    }

    /// The default fusion shim: a fusion `--version` banner and a no-op
    /// `compile` (the scaffolded manifest is the artifact). Replaces the
    /// retired `WELL_BEHAVED_FUSION_SHIM` sh body byte-for-byte.
    #[must_use]
    pub fn well_behaved_fusion_spec() -> ShimSpec {
        ShimSpec::new(0)
            .version("dbt 2.0.0-preview.186\n")
            .compile_ok()
    }

    /// Scrub every repo-pointing `GIT_*` variable from a command's
    /// environment. **Load-bearing**: `git push` exports `GIT_DIR` into its
    /// pre-push hook, so a test suite running UNDER lefthook would
    /// otherwise have every spawned `git add`/`git commit` operate on the
    /// *developer's actual repository* (with the work tree defaulting to
    /// the test cwd) instead of the temp repo — exactly the near-miss that
    /// rewrote this branch's index during cute-dbt#300 development. Applied
    /// to every test-spawned `git` AND to the spawned `cute-dbt` binary
    /// (which shells out to git itself).
    pub fn scrub_git_env(cmd: &mut Command) {
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
            "GIT_CEILING_DIRECTORIES",
            "GIT_NAMESPACE",
        ] {
            cmd.env_remove(var);
        }
    }

    /// A throwaway git repository for `cute-dbt review` tests.
    #[derive(Debug)]
    pub struct TestRepo {
        /// The repository root (also the spawn cwd for `review`).
        pub root: PathBuf,
        /// Holds the empty gitconfig stand-in, OUTSIDE the repo so it can
        /// never appear as an untracked file.
        home: PathBuf,
        /// Shim bin dir, prepended FIRST to a controlled PATH on every
        /// spawn (the shim dir then the platform system tail — `/usr/bin:
        /// /bin` on Unix, the inherited PATH on Windows) — so a developer's
        /// real `dbt` (homebrew, venv, …) is shadowed by an installed shim
        /// and `dbt` is genuinely missing unless a test installs one via
        /// [`TestRepo::install_dbt_shim`]. Genuine-absence cases use
        /// [`TestRepo::review_hermetic`] (shim dir only).
        bin: PathBuf,
    }

    impl TestRepo {
        /// Create a fresh repo under `CARGO_TARGET_TMPDIR` with `main` as
        /// the initial branch (the probes' first candidate).
        pub fn init(stem: &str) -> Self {
            Self::init_with_branch(stem, "main")
        }

        /// Create a fresh repo with an explicit initial branch name (the
        /// no-detectable-base scenarios use a branch the ladder never
        /// probes).
        pub fn init_with_branch(stem: &str, branch: &str) -> Self {
            let base = tmp(&format!("review-repo-{stem}"));
            let _ = std::fs::remove_dir_all(&base);
            let root = base.join("repo");
            let home = base.join("home");
            let bin = base.join("bin");
            std::fs::create_dir_all(&root).expect("create repo dir");
            std::fs::create_dir_all(&home).expect("create home dir");
            std::fs::create_dir_all(&bin).expect("create bin dir");
            std::fs::write(home.join("gitconfig"), "").expect("write empty gitconfig");
            let repo = Self { root, home, bin };
            repo.git(&["init", "-q", "-b", branch]);
            // Sanity tripwire: every later git command must operate on THIS
            // repo, never an enclosing one (see `scrub_git_env`). Canonical
            // comparison — macOS tempdirs traverse the /var symlink.
            let toplevel = repo.git(&["rev-parse", "--show-toplevel"]);
            let reported = std::fs::canonicalize(String::from_utf8_lossy(&toplevel.stdout).trim())
                .expect("canonicalize reported toplevel");
            let expected = std::fs::canonicalize(&repo.root).expect("canonicalize repo root");
            assert_eq!(
                reported, expected,
                "the temp repo's git context leaked outside its root",
            );
            repo
        }

        /// Apply the git-environment isolation to any command (git itself
        /// or the spawned `cute-dbt`, which shells out to git/dbt/gh).
        pub fn isolate(&self, cmd: &mut Command) {
            let empty = self.home.join("gitconfig");
            scrub_git_env(cmd);
            cmd.env("GIT_CONFIG_GLOBAL", &empty)
                .env("GIT_CONFIG_SYSTEM", &empty)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", &self.home)
                .env("GIT_AUTHOR_NAME", "cute-dbt-test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "cute-dbt-test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                // Fully controlled PATH: the shim dir FIRST (so a shim
                // shadows any host tool of the same name), then just enough
                // system dirs for git. A developer's real dbt can never
                // answer a test, and "dbt missing" is the true default.
                .env("PATH", prepend_path(&self.bin, &system_path_tail()))
                .env_remove("CUTE_DBT_EXPERIMENTAL")
                .env_remove("DBT_TARGET_PATH");
        }

        /// Install a stand-in tool named `name` into the controlled PATH:
        /// a hard link to (or copy of) the `cute-dbt` binary (with the
        /// platform exe suffix) plus its sibling `<name>.spec.toml`
        /// behaviour contract. When
        /// `review` spawns `<name>`, the renamed copy detects the spec and
        /// acts as the fake tool (`src/fake_tool.rs`), logging every
        /// invocation to `<bin>/<name>-invocations.log` so tests can assert
        /// the planned-argv == executed-argv pin at the subprocess level.
        pub fn install_shim(&self, name: &str, spec: &ShimSpec) {
            let tool = self.bin.join(format!("{name}{}", exe_suffix()));
            // Re-installs overwrite (a test swaps the scaffold's default dbt
            // shim for its own), so clear any prior tool first — `hard_link`
            // errors if the target exists.
            let _ = std::fs::remove_file(&tool);
            // Hard-link the (multi-MB) binary to avoid copying its bytes per
            // shim; fall back to a copy when a hard link can't be made (a
            // cross-filesystem temp dir, or a FS that disallows it). Both are
            // privilege-free on every platform (gemini review on
            // cute-dbt#347).
            if std::fs::hard_link(env!("CARGO_BIN_EXE_cute-dbt"), &tool).is_err() {
                std::fs::copy(env!("CARGO_BIN_EXE_cute-dbt"), &tool)
                    .expect("install cute-dbt shim");
            }
            std::fs::write(self.bin.join(format!("{name}.spec.toml")), spec.to_toml())
                .expect("write shim spec");
        }

        /// Install a stand-in `dbt` tool (the common case).
        pub fn install_dbt_shim(&self, spec: &ShimSpec) {
            self.install_shim("dbt", spec);
        }

        /// Install a stand-in `gh` tool (cute-dbt#303 — the PR-anchor +
        /// gh-rung tests).
        pub fn install_gh_shim(&self, spec: &ShimSpec) {
            self.install_shim("gh", spec);
        }

        /// Remove a shim from the controlled PATH so the binary is genuinely
        /// "not found" (the gh-missing / dbt-missing scenarios). Removes both
        /// the renamed copy and its sibling spec.
        pub fn remove_shim(&self, name: &str) {
            let _ = std::fs::remove_file(self.bin.join(format!("{name}{}", exe_suffix())));
            let _ = std::fs::remove_file(self.bin.join(format!("{name}.spec.toml")));
        }

        /// A named shim's invocation log (one `cwd=… args=…` line per call).
        pub fn shim_log(&self, name: &str) -> PathBuf {
            self.bin.join(format!("{name}-invocations.log"))
        }

        /// The `dbt` shim invocation log path.
        pub fn dbt_log(&self) -> PathBuf {
            self.shim_log("dbt")
        }

        /// The `dbt` shim log contents — empty when dbt never ran.
        pub fn dbt_log_contents(&self) -> String {
            std::fs::read_to_string(self.dbt_log()).unwrap_or_default()
        }

        /// The `gh` shim log contents — empty when gh never ran.
        pub fn gh_log_contents(&self) -> String {
            std::fs::read_to_string(self.shim_log("gh")).unwrap_or_default()
        }

        /// Run a git command in the repo, asserting success.
        pub fn git(&self, args: &[&str]) -> Output {
            let mut cmd = Command::new("git");
            cmd.args(args).current_dir(&self.root);
            self.isolate(&mut cmd);
            let output = cmd.output().expect("git spawns");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr),
            );
            output
        }

        /// Write a file (creating parents) relative to the repo root.
        pub fn write(&self, rel: &str, content: &str) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent dirs");
            }
            std::fs::write(&path, content).expect("write file");
        }

        /// Stage everything and commit.
        pub fn commit_all(&self, message: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", message]);
        }

        /// Spawn `cute-dbt review <args>` with cwd at `cwd_rel` under the
        /// repo root, fully environment-isolated, output captured (so
        /// stdout is never a TTY and auto-open can never fire).
        pub fn review_in(&self, cwd_rel: &str, args: &[&str]) -> Output {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
            cmd.arg("review")
                .args(args)
                .current_dir(self.root.join(cwd_rel));
            self.isolate(&mut cmd);
            cmd.output().expect("the cute-dbt binary spawns")
        }

        /// Spawn `cute-dbt review <args>` from the repo root.
        pub fn review(&self, args: &[&str]) -> Output {
            self.review_in(".", args)
        }

        /// Spawn `cute-dbt review <args>` on a **hermetic PATH** that
        /// contains ONLY the shim dir — the system dirs are excluded, so
        /// `gh` is genuinely `NotFound` even on hosts (GitHub-hosted
        /// runners) that pre-install it. The tools review shells out to are
        /// linked into the shim dir first, so they still resolve: `git`
        /// (every stage); on Unix `sh`/`env` are linked too (harmless, kept
        /// for parity). `dbt`/`gh` are reachable ONLY if a test installed
        /// their shims — never the host binaries.
        ///
        /// This is the only way to exercise the `--pr` gh-MISSING branch
        /// (`ReviewError::DbtMissing`/`GhMissing` fire on
        /// `io::ErrorKind::NotFound`, the genuine-missing-binary case — a
        /// non-zero-exit shim is "present but failed", a different branch).
        pub fn review_hermetic(&self, args: &[&str]) -> Output {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
            cmd.arg("review").args(args).current_dir(&self.root);
            let empty = self.home.join("gitconfig");
            scrub_git_env(&mut cmd);
            cmd.env("GIT_CONFIG_GLOBAL", &empty)
                .env("GIT_CONFIG_SYSTEM", &empty)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", &self.home)
                .env("GIT_AUTHOR_NAME", "cute-dbt-test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "cute-dbt-test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                // HERMETIC: shim dir + just enough to resolve `git`. A host
                // `gh`/`dbt` (GitHub runners pre-install gh) is unreachable,
                // so the binary sees io::ErrorKind::NotFound — the only
                // trigger for the dbt/gh-MISSING install remediation.
                .env("PATH", self.hermetic_path())
                .env_remove("CUTE_DBT_EXPERIMENTAL")
                .env_remove("DBT_TARGET_PATH");
            cmd.output().expect("the cute-dbt binary spawns")
        }

        /// The hermetic PATH: the shim dir plus only what `git` needs to
        /// resolve, never the broad system dirs where `gh`/`dbt` live.
        ///
        /// On Unix, `git` is symlinked INTO the shim dir (a symlink to
        /// `/usr/bin/git` works standalone), so the PATH is the shim dir
        /// alone. On Windows, Git for Windows' `git.exe` is a launcher that
        /// locates its install tree (libexec, DLLs, the `git-*` helpers)
        /// relative to its OWN directory — a bare copy/symlink into the
        /// shim dir would break (`not inside a git repository`). So we
        /// instead append `git.exe`'s REAL parent dir to the PATH. That dir
        /// (e.g. `C:\Program Files\Git\cmd`) carries `git` but not
        /// `gh`/`dbt` on the runner, preserving hermeticity for the
        /// missing-tool assertions.
        #[cfg(unix)]
        fn hermetic_path(&self) -> std::ffi::OsString {
            // Symlink `git` (and `sh`/`env` for parity) into the shim dir; a
            // symlink to e.g. `/usr/bin/git` works standalone. A tool already
            // present is left untouched; an unresolvable one is skipped (the
            // run surfaces its own NotFound).
            for tool in ["git", "sh", "env"] {
                let target = self.bin.join(tool);
                if target.exists() {
                    continue;
                }
                if let Some(src) = resolve_on_host_path(tool) {
                    let _ = std::os::unix::fs::symlink(&src, &target);
                }
            }
            self.bin.as_os_str().to_owned()
        }

        #[cfg(windows)]
        fn hermetic_path(&self) -> std::ffi::OsString {
            match resolve_on_host_path("git").and_then(|p| p.parent().map(Path::to_path_buf)) {
                Some(dir) => join_path(&[self.bin.as_path(), dir.as_path()]),
                None => self.bin.as_os_str().to_owned(),
            }
        }
    }

    /// Prepend `dir` to a PATH `tail`, joined with the platform separator.
    fn prepend_path(dir: &Path, tail: &str) -> std::ffi::OsString {
        if tail.is_empty() {
            return dir.as_os_str().to_owned();
        }
        let mut dirs: Vec<PathBuf> = vec![dir.to_path_buf()];
        dirs.extend(std::env::split_paths(tail));
        let refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
        join_path(&refs)
    }

    /// Resolve a bare tool name to its absolute path by searching the test
    /// process's real `PATH` (the host PATH, before any harness override).
    /// On Windows the tool resolves with its `.exe` suffix.
    fn resolve_on_host_path(tool: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        let name = format!("{tool}{}", exe_suffix());
        std::env::split_paths(&path)
            .map(|dir| dir.join(&name))
            .find(|candidate| candidate.is_file())
    }

    /// Scaffold a minimal dbt project at `project_rel` (`"."` = the repo
    /// root): `dbt_project.yml`, the jaffle-shop staging model the
    /// committed fixtures know, a `target/`-ignoring `.gitignore`, one
    /// initial commit — then the committed `manifest_fixture` copied to
    /// `<project>/target/manifest.json` (untracked + ignored, like a real
    /// `dbt compile` output).
    pub fn scaffold_dbt_project(repo: &TestRepo, project_rel: &str, manifest_fixture: &str) {
        let prefix = if project_rel == "." {
            String::new()
        } else {
            format!("{project_rel}/")
        };
        repo.write(
            &format!("{prefix}dbt_project.yml"),
            "name: jaffle_shop\nversion: \"1.0\"\nprofile: jaffle_shop\n",
        );
        repo.write(&format!("{prefix}.gitignore"), "target/\n");
        repo.write(
            &format!("{prefix}models/staging/stg_customers.sql"),
            "select 1 as customer_id\n",
        );
        repo.commit_all("initial dbt project");
        let target = repo.root.join(project_rel).join("target");
        std::fs::create_dir_all(&target).expect("create target dir");
        std::fs::copy(fixture(manifest_fixture), target.join("manifest.json"))
            .expect("copy manifest fixture");
        // A well-behaved fusion shim (cute-dbt#301): review compiles by
        // default, so the default scaffold answers `--version` with the
        // fusion single-line shape and no-op "compiles" (the manifest above
        // already plays the compiled artifact). Tests that need other
        // engine behaviors overwrite it via `install_dbt_shim`.
        repo.install_dbt_shim(&well_behaved_fusion_spec());
    }
}

// Re-export the cross-platform review harness so callers keep using
// `common::TestRepo`, `common::scrub_git_env`, `common::ShimSpec`, etc.
// `allow(unused_imports)`: `common/mod.rs` is `#[path]`-included by
// several test binaries (resource_ref_lint, golden_report, …) that use
// only the lint/fixture helpers, never the review harness — so the
// re-export is legitimately unused from *their* compile unit (it IS
// used by review_cli / skill_cli / the bdd review steps). The
// crate-level `#![allow(dead_code)]` does not cover unused *imports*.
#[allow(unused_imports)]
pub use review_harness::{
    ShimSpec, TestRepo, scaffold_dbt_project, scrub_git_env, well_behaved_fusion_spec,
};

// ===== Resource-ref lint shared with tests/resource_ref_lint.rs =====
//
// Same shape as the standalone lint test — the BDD scenario for
// "report has no external resource references" runs the same scan
// against a freshly-generated report.html. The lint walks the parsed
// DOM via `tl` so minified bundles' inert URL string literals do not
// false-positive (the rationale lives in resource_ref_lint.rs).

#[derive(Debug)]
pub struct ResourceRefViolation {
    pub kind: &'static str,
    pub value: String,
}

impl std::fmt::Display for ResourceRefViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.value)
    }
}

/// Strip `<script>` bodies before handing the HTML to `tl`. tl does not
/// fully enforce the HTML5 "script content is raw text" rule, so
/// template-literal substrings like `<img src="${e}">` inside a
/// minified bundle can otherwise be materialized as spurious DOM nodes.
/// The lint only cares about the OPENING tag's attributes; the body
/// strip loses no real signal.
pub fn strip_script_bodies(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(open_rel) = lower[cursor..].find("<script") {
        let open_start = cursor + open_rel;
        out.push_str(&html[cursor..open_start]);
        let Some(open_end_rel) = lower[open_start..].find('>') else {
            out.push_str(&html[open_start..]);
            return out;
        };
        let open_end = open_start + open_end_rel + 1;
        out.push_str(&html[open_start..open_end]);
        if html[open_start..open_end].ends_with("/>") {
            cursor = open_end;
            continue;
        }
        match lower[open_end..].find("</script>") {
            Some(close_rel) => {
                let close_start = open_end + close_rel;
                out.push_str("</script>");
                cursor = close_start + "</script>".len();
            }
            None => {
                out.push_str(&html[open_end..]);
                return out;
            }
        }
    }
    out.push_str(&html[cursor..]);
    out
}

/// Whether an `href` / `src` / `srcset` value is forbidden under the
/// single-self-contained-file invariant. Allowlist: empty,
/// `#fragment`, `data:` URI, `mailto:`.
pub fn is_forbidden_resource_ref(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.starts_with('#') {
        return false;
    }
    if v.starts_with("data:") || v.starts_with("mailto:") {
        return false;
    }
    true
}

fn check_attr(
    attrs: &tl::Attributes<'_>,
    attr_name: &str,
    kind: &'static str,
    out: &mut Vec<ResourceRefViolation>,
) {
    let Some(Some(raw)) = attrs.get(attr_name) else {
        return;
    };
    let value = raw.as_utf8_str();
    if is_forbidden_resource_ref(&value) {
        out.push(ResourceRefViolation {
            kind,
            value: value.into_owned(),
        });
    }
}

/// Validate `<img srcset>` by splitting on commas: each candidate is
/// `<url> [<descriptor>]` (e.g. `data:image/png;... 1x, https://… 2x`).
/// Passing the whole srcset value into `is_forbidden_resource_ref`
/// would let a multi-value `data:foo 1x, https://attacker.com 2x`
/// bypass detection — the first candidate's `data:` prefix would mark
/// the whole value as allowed.
fn check_srcset(attrs: &tl::Attributes<'_>, out: &mut Vec<ResourceRefViolation>) {
    let Some(Some(raw)) = attrs.get("srcset") else {
        return;
    };
    let value = raw.as_utf8_str();
    for candidate in value.split(',') {
        let url = candidate.split_whitespace().next().unwrap_or("").trim();
        if is_forbidden_resource_ref(url) {
            out.push(ResourceRefViolation {
                kind: "<img srcset>",
                value: url.to_owned(),
            });
        }
    }
}

fn find_css_external_refs(css: &str, out: &mut Vec<ResourceRefViolation>) {
    let lower = css.to_ascii_lowercase();
    let mut i = 0;
    while let Some(rel) = lower[i..].find("@import") {
        let start = i + rel;
        let end_of_snippet = (start + 80).min(css.len());
        out.push(ResourceRefViolation {
            kind: "@import",
            value: css[start..end_of_snippet]
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
        });
        i = start + "@import".len();
    }

    let mut i = 0;
    while let Some(rel) = lower[i..].find("url(") {
        let start = i + rel + 4;
        let end = lower[start..].find(')').map_or(lower.len(), |n| start + n);
        let inner = css[start..end]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'');
        if !inner.is_empty() && !inner.starts_with("data:") && !inner.starts_with('#') {
            out.push(ResourceRefViolation {
                kind: "url()",
                value: inner.to_string(),
            });
        }
        i = end + 1;
    }
}

/// Scan an HTML string and return every forbidden resource reference
/// found. Used by the BDD `zero_egress.feature` scenario and by the
/// standalone `tests/resource_ref_lint.rs` test.
pub fn scan_resource_refs(html: &str) -> Vec<ResourceRefViolation> {
    let stripped = strip_script_bodies(html);
    let dom = tl::parse(&stripped, tl::ParserOptions::default()).expect("HTML must be parseable");
    let parser = dom.parser();
    let mut out = Vec::new();
    for node_handle in dom.nodes() {
        let Some(tag) = node_handle.as_tag() else {
            continue;
        };
        let name_lc = tag.name().as_utf8_str().to_ascii_lowercase();
        let attrs = tag.attributes();
        match name_lc.as_str() {
            "script" => check_attr(attrs, "src", "<script src>", &mut out),
            "link" => check_attr(attrs, "href", "<link href>", &mut out),
            "img" => {
                check_attr(attrs, "src", "<img src>", &mut out);
                check_srcset(attrs, &mut out);
            }
            "style" => {
                let css = tag.inner_text(parser);
                find_css_external_refs(&css, &mut out);
            }
            _ => {}
        }
    }
    out
}

/// Assertion helper for the BDD `zero_egress.feature` scenarios and for
/// the report_generation "self-contained" assertion. Panics on any
/// violation.
pub fn assert_no_external_refs(html: &str) {
    let violations = scan_resource_refs(html);
    assert!(
        violations.is_empty(),
        "report contains {} external resource reference(s) — the zero-egress invariant is broken:\n{}",
        violations.len(),
        violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
