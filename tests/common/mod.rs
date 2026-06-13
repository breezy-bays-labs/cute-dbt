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
/// the `example-report-check` explore matrix row. Both gates scan
/// them: the resource-ref lint uniformly, the headless gate with a
/// **page-aware liveness oracle** (`dag.html` waits for the Mermaid
/// lineage SVG; `tests.html` is a static server-rendered page asserted
/// on DOM facts — it carries no Mermaid and no DataTables, so the
/// report's liveness probes must never be applied to it).
pub const COMMITTED_EXPLORE_PAGES: &[&str] = &["explore/dag.html", "explore/tests.html"];

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
// tracked: cute-dbt#331 — review subprocess shim harness is Unix-only;
// Windows-capable shim deferred. The shim machinery below
// (`PermissionsExt`/`set_mode(0o755)`, the `#!/bin/sh` shim bodies, the
// `{bin}:/usr/bin:/bin` controlled PATH, and the host-tool symlinks)
// uses Unix-only APIs, so the whole block — and the subprocess test
// suites that consume it — is `#[cfg(unix)]`-gated. The PORTABLE review
// unit tests (path/git logic) in `src/cli/review.rs` and the direct-
// spawn integration tests (run_loop.rs etc.) run everywhere, including
// the windows-latest job (cute-dbt#308/#316). Re-exported below so
// callers keep using `common::TestRepo` / `common::scrub_git_env`.
#[cfg(unix)]
mod review_harness {
    use super::{Output, PathBuf, fixture, tmp};
    use std::process::Command;

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
        /// Shim bin dir, prepended to a **fully controlled** PATH
        /// (`<bin>:/usr/bin:/bin`) on every spawn — so a developer's real
        /// `dbt` (homebrew, venv, …) can never answer a test, and `dbt` is
        /// genuinely missing unless a test installs a shim via
        /// [`TestRepo::install_dbt_shim`].
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
                // Fully controlled PATH: the shim dir, then just enough
                // system dirs for git/sh. A developer's real dbt can never
                // answer a test, and "dbt missing" is the true default.
                .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
                .env_remove("CUTE_DBT_EXPERIMENTAL")
                .env_remove("DBT_TARGET_PATH");
        }

        /// Install an executable shim named `name` into the controlled PATH.
        /// The body is `#!/bin/sh` script text; every invocation's cwd + argv
        /// is appended to `<bin>/<name>-invocations.log` before the body
        /// runs, so tests can assert exactly what review executed (the
        /// planned-argv == executed-argv pin at the subprocess level).
        pub fn install_shim(&self, name: &str, body: &str) {
            use std::os::unix::fs::PermissionsExt;
            let path = self.bin.join(name);
            let script = format!(
                "#!/bin/sh\nprintf 'cwd=%s args=%s\\n' \"$(pwd)\" \"$*\" >> \"{log}\"\n{body}\n",
                log = self.shim_log(name).display(),
            );
            std::fs::write(&path, script).expect("write shim");
            let mut perms = std::fs::metadata(&path)
                .expect("shim metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod shim");
        }

        /// Install an executable `dbt` shim (the common case).
        pub fn install_dbt_shim(&self, body: &str) {
            self.install_shim("dbt", body);
        }

        /// Install an executable `gh` shim (cute-dbt#303 — the PR-anchor +
        /// gh-rung tests).
        pub fn install_gh_shim(&self, body: &str) {
            self.install_shim("gh", body);
        }

        /// Remove a shim from the controlled PATH so the binary is genuinely
        /// "not found" (the gh-missing / dbt-missing scenarios).
        pub fn remove_shim(&self, name: &str) {
            let _ = std::fs::remove_file(self.bin.join(name));
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
        /// contains ONLY the shim dir — `/usr/bin` and `/bin` are excluded,
        /// so `gh` is genuinely `NotFound` even on hosts (GitHub-hosted
        /// Linux runners) that pre-install `/usr/bin/gh`. The tools review
        /// shells out to are symlinked into the shim dir first, so they
        /// still resolve: `git` (every stage), plus `sh`/`env` for any shell
        /// shim a test may have installed. `dbt`/`gh` are reachable ONLY if
        /// a test installed their shims — never the host binaries.
        ///
        /// This is the only way to exercise the `--pr` gh-MISSING branch
        /// (`ReviewError::DbtMissing`/`GhMissing` fire on
        /// `io::ErrorKind::NotFound`, the genuine-missing-binary case — a
        /// non-zero-exit shim is "present but failed", a different branch).
        pub fn review_hermetic(&self, args: &[&str]) -> Output {
            self.symlink_host_tools(&["git", "sh", "env"]);
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
                // HERMETIC: the shim dir ONLY. No /usr/bin, so a host
                // /usr/bin/gh (GitHub runners) is unreachable; git/sh resolve
                // via the symlinks created above.
                .env("PATH", self.bin.display().to_string())
                .env_remove("CUTE_DBT_EXPERIMENTAL")
                .env_remove("DBT_TARGET_PATH");
            cmd.output().expect("the cute-dbt binary spawns")
        }

        /// Symlink each named host tool into the shim dir (resolved from the
        /// real PATH), so it resolves on the hermetic PATH that excludes the
        /// system dirs. A tool already present (a real binary, a prior
        /// symlink, or a test shim) is left untouched; an unresolvable tool
        /// is skipped (the hermetic run will surface its own NotFound).
        fn symlink_host_tools(&self, tools: &[&str]) {
            for tool in tools {
                let target = self.bin.join(tool);
                if target.exists() {
                    continue;
                }
                if let Some(src) = resolve_on_host_path(tool) {
                    let _ = std::os::unix::fs::symlink(&src, &target);
                }
            }
        }
    }

    /// Resolve a bare tool name to its absolute path by searching the test
    /// process's real `PATH` (the host PATH, before any harness override).
    fn resolve_on_host_path(tool: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(tool))
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
        repo.install_dbt_shim(WELL_BEHAVED_FUSION_SHIM);
    }

    /// The default shim body: fusion version banner; `compile` succeeds
    /// without touching anything (the scaffolded manifest is the artifact).
    pub const WELL_BEHAVED_FUSION_SHIM: &str = "\
case \"$1\" in
  --version) printf 'dbt 2.0.0-preview.186\\n'; exit 0;;
  compile) exit 0;;
esac
exit 0";
}

// Re-export the Unix-only review harness so callers keep using
// `common::TestRepo`, `common::scrub_git_env`, etc. unchanged. The
// `pub use` is itself `#[cfg(unix)]`, so on Windows these names simply
// do not exist (and the test suites that reference them are gated too).
// `allow(unused_imports)`: `common/mod.rs` is `#[path]`-included by
// several test binaries (resource_ref_lint, golden_report, …) that use
// only the lint/fixture helpers, never the review harness — so the
// re-export is legitimately unused from *their* compile unit (it IS
// used by review_cli / skill_cli / the bdd review steps). The
// crate-level `#![allow(dead_code)]` does not cover unused *imports*.
#[cfg(unix)]
#[allow(unused_imports)]
pub use review_harness::{TestRepo, WELL_BEHAVED_FUSION_SHIM, scaffold_dbt_project, scrub_git_env};

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
