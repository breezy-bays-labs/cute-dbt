//! The `review` porcelain verb (cute-dbt#300, epic #294 Shape D):
//! one command from a checked-out branch to the rendered PR-review
//! report.
//!
//! `review` is the local twin of the documented CI pr-diff recipe. It
//! locates the dbt project (nearest `dbt_project.yml`), detects the
//! base branch (the research-294 ladder), computes the merge-base,
//! produces the working-tree-endpoint unified diff with the
//! config-proof flag set (research-294 §3, verbatim), and composes the
//! **existing** `report` run loop in-process through the
//! [`crate::cli::pr_diff::parse_diff`] literal arm — no temp file, no
//! self-exec, no parallel pipeline. Any future `report` behavior change
//! is picked up by `review` automatically (the no-drift seam).
//!
//! Two design invariants pinned by tests here and in
//! `tests/review_cli.rs`:
//!
//! - **Plans before execution.** Every subprocess stage builds a
//!   [`CommandPlan`] (argv + cwd + env) first; execution and the
//!   `--dry-run` listing both read the *same* plan value, so the
//!   printed commands cannot drift from the executed ones.
//!   [`CommandPlan::to_command`] is the single spawn mapping and is
//!   unit-tested field-for-field.
//! - **Review never mutates the user's working tree.** No checkout, no
//!   `git add`, no index writes — the only writes are the report file
//!   itself and (V1) nothing else. Wrapper-stage failures are
//!   [`ReviewError`] — a cli-layer enum, **not** new
//!   [`crate::domain::PreflightError`] variants (that enum stays at
//!   exactly four; the same reasoning that kept baseline-missing a
//!   usage error).
//!
//! The composed report run inherits `report`'s exit-code discipline:
//! `0` success (including the deliberate empty-diff "nothing to
//! review" exit), `1` any review-stage or preflight failure, `2` clap
//! usage errors.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use clap::{ArgGroup, Args};

use crate::adapters::config_reader::load_config;
use crate::domain::{AnalysisConfig, Experiment};

use super::RunError;
use super::args::ReportArgs;
use super::pr_diff::parse_diff;

// ===================================================================
// clap surface
// ===================================================================

/// Arguments for `cute-dbt review` — the one-command porcelain verb.
///
/// The `review_scope` `ArgGroup` holds the mutually-exclusive scope
/// selectors. V1 ships the default (everything on the branch vs the
/// detected base — committed + staged + unstaged) and
/// `--committed-only`; `--staged` / `--unstaged` (V3) and `--pr` (V4)
/// extend the same group, so a conflicting pair is a clap usage error
/// (exit 2) from the day the second member lands.
// struct_excessive_bools: each bool IS one CLI flag on a clap-derive
// surface (`--committed-only`, `--force`, `--no-open`, `--dry-run`) —
// folding them into two-variant enums would fight the derive for zero
// modeling gain. The flags are orthogonal switches, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Args)]
#[command(
    group = ArgGroup::new("review_scope")
        .multiple(false)
        .args(["committed_only", "staged", "unstaged", "pr"]),
    after_long_help = REVIEW_EXAMPLES,
)]
pub struct ReviewArgs {
    /// Base ref to diff against, skipping detection (e.g. `main`,
    /// `origin/release-2.4`).
    ///
    /// Without it, review walks the detection ladder: `git config
    /// cute-dbt.base` → the open PR's base via `gh` → the `origin/HEAD`
    /// symref → probing `origin/{main,master,trunk}` then local heads.
    /// The answering rung is announced on stderr.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,

    /// Anchor the review to the repo's open pull request: its base
    /// branch becomes the review base.
    ///
    /// Bare `--pr` uses the current branch's open PR (error with
    /// remediation if there is none). `--pr <N>` additionally asserts
    /// that the current HEAD *is* PR #N's head branch — if it is not,
    /// review tells you to `gh pr checkout <N>` first and stops; it
    /// NEVER checks out or mutates your working tree itself. Requires
    /// `gh` on PATH (the GitHub CLI). Mutually exclusive with `--base`
    /// and the other scope selectors.
    ///
    /// `Option<Option<u64>>` is clap's standard shape for a flag that
    /// optionally takes a value: `None` = flag absent, `Some(None)` =
    /// bare `--pr`, `Some(Some(n))` = `--pr n`. The nested option is
    /// load-bearing here (the three states are distinct), so the
    /// `option_option` lint is suppressed at this one field.
    #[allow(clippy::option_option)]
    #[arg(long, value_name = "N", num_args = 0..=1, conflicts_with = "base")]
    pub pr: Option<Option<u64>>,

    /// Review only committed changes (`<merge-base>..HEAD`) — exact
    /// parity with what a PR would show.
    ///
    /// The default includes your staged + unstaged edits (the
    /// working-tree endpoint), because the manifest is compiled from
    /// the working tree — the same-revision contract that keeps the
    /// inline diffs sound.
    #[arg(long)]
    pub committed_only: bool,

    /// Review only staged changes (HEAD → the index, `git diff
    /// --cached`).
    ///
    /// Honest caveat: the manifest is always compiled from the working
    /// tree, so if a file you have staged also has *unstaged* edits, the
    /// diff (index) and the manifest (working tree) disagree — review
    /// detects that, warns, and the existing drift-guard degrades the
    /// inline diffs gracefully rather than rendering wrong ones.
    #[arg(long)]
    pub staged: bool,

    /// Review only unstaged changes (the index → the working tree,
    /// bare `git diff`).
    ///
    /// This is the working-tree-vs-index slice; the manifest is
    /// compiled from the working tree, so it lines up with this diff's
    /// new side.
    #[arg(long)]
    pub unstaged: bool,

    /// Render the zero-scope report even when the diff is empty.
    ///
    /// Without it an empty diff prints "nothing to review", writes no
    /// file, and exits 0.
    #[arg(long)]
    pub force: bool,

    /// Report output path.
    ///
    /// Defaults to `<project>/target/cute-dbt-report.html` — inside
    /// dbt's conventionally-gitignored `target/`, so the generated
    /// report (whose inlined paths can carry your local directory
    /// layout) is never invited into version control.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,

    /// dbt project directory.
    ///
    /// Defaults to discovery: the working directory if it holds a
    /// `dbt_project.yml`, else exactly one immediate subdirectory that
    /// does (anything else is an error listing the candidates).
    #[arg(long, value_name = "DIR", value_parser = parse_project_dir)]
    pub project_dir: Option<PathBuf>,

    /// Skip the `dbt compile` step and trust the existing manifest.
    ///
    /// A staleness check warns (never blocks) when project sources are
    /// newer than the manifest. With this flag, dbt does not need to be
    /// installed at all.
    #[arg(long)]
    pub no_compile: bool,

    /// dbt target directory holding `manifest.json`.
    ///
    /// Wins over the `DBT_TARGET_PATH` environment variable; a relative
    /// value resolves against the project directory (dbt's own
    /// semantics). The same value is passed to `dbt compile` as
    /// `--target-path`, so dbt writes where review reads. Defaults to
    /// `<project>/target`.
    #[arg(long, value_name = "DIR")]
    pub target_path: Option<PathBuf>,

    /// Do not open the report when the run finishes.
    ///
    /// Auto-open only happens on an interactive terminal; scripts and
    /// agents (non-TTY stdout) never trigger it even without this flag.
    #[arg(long)]
    pub no_open: bool,

    /// Optional TOML configuration, passed through to the composed
    /// report run unchanged (same file format and same usage-error
    /// handling as `report --config`).
    #[arg(long, value_name = "PATH", value_parser = parse_review_config)]
    pub config: Option<ReviewConfig>,

    /// Print the exact commands a real run would execute — the git
    /// diff invocation and the equivalent `cute-dbt report` invocation
    /// — then exit without executing anything or writing any file.
    #[arg(long)]
    pub dry_run: bool,

    /// Experimental opt-in via `CUTE_DBT_EXPERIMENTAL` — flows through
    /// to the composed report run untouched (same hidden-flag shape as
    /// `report`, cute-dbt#289).
    #[arg(
        long,
        value_name = "IDS",
        env = "CUTE_DBT_EXPERIMENTAL",
        value_parser = super::args::parse_experimental_value,
        hide = true
    )]
    pub experimental: Option<BTreeSet<Experiment>>,
}

/// Worked examples + the privacy posture, appended to `review --help`'s
/// long help.
const REVIEW_EXAMPLES: &str = "\
Examples:
  # On a feature branch: detect the base, run your dbt compile, diff,
  # render, open.
  cute-dbt review

  # Diff against a specific base and skip auto-open (scripts/agents).
  cute-dbt review --base origin/main --no-open

  # Exactly what a PR would show — committed changes only.
  cute-dbt review --committed-only

  # Only what is staged (HEAD -> index), or only unstaged edits.
  cute-dbt review --staged
  cute-dbt review --unstaged

  # Trust an already-compiled manifest; dbt is not invoked (or needed).
  cute-dbt review --no-compile

  # Show every command a real run would execute, run nothing.
  cute-dbt review --dry-run

Review walks: dbt project discovery -> base detection -> merge-base ->
`git diff --unified=0` (config-proof flag set) -> your own `dbt compile`
(exit code is the success signal; skipped with --no-compile) -> the same
in-process pipeline as `cute-dbt report --pr-diff` -> report written ->
auto-open on a TTY. A persisted base lives in `git config cute-dbt.base`.

Privacy: review itself makes zero network requests, and the generated
report makes zero outbound requests when opened. The compile step runs
YOUR dbt, which may phone home on its own (engine version check,
anonymous usage stats) — that egress belongs to dbt, not cute-dbt.
Suppress it with dbt's own switches:
  DBT_DISABLE_VERSION_CHECK=1 DBT_SEND_ANONYMOUS_USAGE_STATS=false cute-dbt review
Review never reads or edits your profiles.yml; connection problems
surface dbt's own error verbatim.";

/// The `--config` value: the path as typed (for the `--dry-run`
/// listing) plus the eagerly-parsed config (for the composed report
/// run). Parsing eagerly keeps `review --config` byte-identical in UX
/// to `report --config`: a missing / invalid file is a clap usage
/// error (exit 2), never a runtime failure.
#[derive(Debug, Clone)]
pub struct ReviewConfig {
    /// The path exactly as the operator typed it.
    pub path: PathBuf,
    /// The parsed configuration handed to the report run.
    pub parsed: AnalysisConfig,
}

/// clap value-parser for `review --config`: parse the TOML eagerly and
/// keep the typed path for plan display.
fn parse_review_config(s: &str) -> Result<ReviewConfig, String> {
    let parsed = load_config(Path::new(s)).map_err(|err| err.to_string())?;
    Ok(ReviewConfig {
        path: PathBuf::from(s),
        parsed,
    })
}

/// clap value-parser for `review --project-dir`: the directory must
/// exist (whether it actually holds a `dbt_project.yml` is checked in
/// the run loop, where the error can carry the review remediation).
fn parse_project_dir(s: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(s);
    if !p.exists() {
        return Err(format!("project dir does not exist: {s}"));
    }
    if !p.is_dir() {
        return Err(format!("project dir is not a directory: {s}"));
    }
    Ok(p)
}

// ===================================================================
// ReviewError — the cli-layer wrapper-stage failure surface
// ===================================================================

/// Where a failed base ref came from — wording differs between the two
/// operator-supplied rungs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseSource {
    /// The `--base` flag.
    Flag,
    /// The persisted `git config cute-dbt.base` answer.
    GitConfig,
    /// The open PR's base branch (the `--pr` / gh path) — its base ref
    /// is not present locally.
    PullRequest,
}

/// A review-stage failure (git / detection / discovery), each carrying
/// a remediation. Deliberately **not** new [`crate::domain::PreflightError`]
/// variants: wrapper-stage and report-runtime failures must not
/// conflate (the baseline-missing precedent, ADR-2). All map to exit 1.
#[derive(Debug)]
pub enum ReviewError {
    /// `git` itself could not be spawned (`NotFound` on PATH).
    GitMissing,
    /// The working directory is not inside a git repository.
    NoGitRepo,
    /// The repository is bare — there is no working tree to compile or
    /// diff.
    BareRepo,
    /// HEAD is unborn — the repository has no commits to diff.
    NoCommits,
    /// Every ladder rung came up empty.
    BaseUndetectable,
    /// An operator-supplied base (flag or git config) does not resolve
    /// to a commit.
    BaseRefMissing {
        /// The ref as supplied.
        ref_name: String,
        /// Which surface supplied it.
        source: BaseSource,
    },
    /// `git merge-base` failed and the clone is shallow.
    ShallowClone {
        /// The detected/explicit base ref.
        base: String,
    },
    /// `git merge-base` failed on a full clone — no common ancestor.
    DisjointHistories {
        /// The detected/explicit base ref.
        base: String,
    },
    /// `--pr` was passed but `gh` (the GitHub CLI) is not on PATH.
    GhMissing,
    /// `--pr` was passed but no open PR resolves for the current branch
    /// (or `gh` failed for an unrelated reason).
    NoPullRequest {
        /// The current branch (for the message), when known.
        branch: Option<String>,
    },
    /// `--pr <n>` was passed but the current HEAD is not that PR's head
    /// branch. Review never checks out for you — it stops and tells you
    /// to do it.
    PrHeadMismatch {
        /// The PR number the operator named.
        number: u64,
        /// The PR's actual head branch.
        head_ref: String,
        /// The branch (or detached-HEAD note) currently checked out.
        current: String,
    },
    /// No `dbt_project.yml` was found.
    ProjectNotFound {
        /// Where discovery looked (or the explicit `--project-dir`).
        searched: PathBuf,
        /// Whether the operator named the directory explicitly.
        explicit: bool,
    },
    /// More than one candidate project one level down.
    ProjectAmbiguous {
        /// Every directory holding a `dbt_project.yml`.
        candidates: Vec<PathBuf>,
    },
    /// The dbt project lies outside the git repository's toplevel, so
    /// no repo-relative diff pathspec can cover it.
    ProjectOutsideRepo {
        /// The resolved project directory.
        project: PathBuf,
        /// The repository toplevel.
        toplevel: PathBuf,
    },
    /// No compiled manifest where review expects one.
    ManifestMissing {
        /// The resolved manifest path.
        path: PathBuf,
        /// `true` when a successful `dbt compile` still left no
        /// manifest at the resolved path (a target-path mismatch);
        /// `false` on the `--no-compile` arm (nothing was ever
        /// compiled).
        after_compile: bool,
    },
    /// `dbt` itself could not be spawned (`NotFound` on PATH).
    DbtMissing,
    /// `dbt --version` answered with an engine review cannot drive.
    EngineUnsupported(EngineIssue),
    /// `dbt compile` exited non-zero. The manifest file may exist
    /// anyway — fusion writes it even on failed compiles — so the exit
    /// code, never artifact presence, is the success signal.
    CompileFailed {
        /// The compile exit code, when the process was not killed by a
        /// signal.
        status: Option<i32>,
    },
    /// An unexpected failure in a review stage (a git command exiting
    /// non-zero, an unreadable directory, …).
    StageFailed {
        /// The stage that failed, for the message.
        context: &'static str,
        /// The underlying detail (stderr / io error).
        detail: String,
    },
}

/// `(description, remediation)` for [`ReviewError::BaseRefMissing`],
/// attributing the ref to its real source — the PR path must NOT claim
/// the ref came from `--base` (cute-dbt#303 bot review).
fn base_ref_missing_message(ref_name: &str, source: BaseSource) -> (String, String) {
    let from = match source {
        BaseSource::Flag => "--base",
        BaseSource::GitConfig => "git config cute-dbt.base",
        BaseSource::PullRequest => "the open PR's base branch",
    };
    let fix = match source {
        BaseSource::PullRequest => format!(
            "Fetch the PR's base (`git fetch origin {ref_name}`) and re-run — \
             or pass `--base <ref>` instead of `--pr`."
        ),
        BaseSource::Flag | BaseSource::GitConfig => {
            format!("Fetch it (`git fetch origin {ref_name}`) or pass a different `--base <ref>`.")
        }
    };
    (
        format!("the base ref `{ref_name}` (from {from}) does not resolve to a commit."),
        fix,
    )
}

impl ReviewError {
    /// The operator-facing stderr message: what is wrong, then what to
    /// do about it. Same description-plus-remediation shape as
    /// [`super::exit::remediation`].
    #[must_use]
    pub fn message(&self) -> String {
        let (what, fix) = self.describe();
        format!("cute-dbt review: {what}\n{fix}")
    }

    /// `(description, remediation)` for each variant — split into the
    /// git/ladder, dbt/manifest, and project/stage thirds so each match
    /// stays readable (and under the line-count lint).
    fn describe(&self) -> (String, String) {
        self.describe_git()
            .or_else(|| self.describe_dbt())
            .unwrap_or_else(|| self.describe_project())
    }

    /// The git-and-ladder arms; `None` for the project/stage arms.
    fn describe_git(&self) -> Option<(String, String)> {
        let pair = match self {
            Self::GitMissing => (
                "`git` was not found on PATH.".to_owned(),
                "Install Git (on Windows: Git for Windows) and ensure `git` is on PATH, \
                 then re-run."
                    .to_owned(),
            ),
            Self::NoGitRepo => (
                "not inside a git repository.".to_owned(),
                "Run `cute-dbt review` from your dbt repo checkout — or produce a diff \
                 yourself and use `cute-dbt report --pr-diff @file`."
                    .to_owned(),
            ),
            Self::BareRepo => (
                "this is a bare repository — there is no working tree.".to_owned(),
                "Review diffs and reads the working tree; run it from a normal checkout."
                    .to_owned(),
            ),
            Self::NoCommits => (
                "the repository has no commits yet — nothing to diff.".to_owned(),
                "Commit your work first, then re-run `cute-dbt review`.".to_owned(),
            ),
            Self::BaseUndetectable => (
                "could not determine the base branch: no `--base`, no `git config \
                 cute-dbt.base`, no `origin/HEAD`, and no main/master/trunk ref."
                    .to_owned(),
                "Pass `--base <branch>` — or persist the answer once with \
                 `git config cute-dbt.base <branch>`."
                    .to_owned(),
            ),
            Self::BaseRefMissing { ref_name, source } => {
                base_ref_missing_message(ref_name, *source)
            }
            Self::ShallowClone { base } => (
                format!("no merge-base with `{base}`: this clone is shallow."),
                "Run `git fetch --unshallow` (or `git fetch --deepen=<n>`), then re-run."
                    .to_owned(),
            ),
            Self::DisjointHistories { base } => (
                format!("no merge-base with `{base}`: the histories share no common ancestor."),
                "Pass `--base <ref>` naming a branch that shares history with HEAD.".to_owned(),
            ),
            Self::GhMissing => (
                "`--pr` needs the GitHub CLI (`gh`), which was not found on PATH.".to_owned(),
                "Install `gh` (https://cli.github.com) and `gh auth login` — or drop `--pr` \
                 and pass `--base <ref>` instead."
                    .to_owned(),
            ),
            Self::NoPullRequest { branch } => {
                let on = branch
                    .as_deref()
                    .map_or_else(String::new, |b| format!(" for `{b}`"));
                (
                    format!("`--pr` found no open pull request{on}."),
                    "Open a PR first (`gh pr create`), or pass `--base <ref>` to review \
                     against a branch without a PR."
                        .to_owned(),
                )
            }
            Self::PrHeadMismatch {
                number,
                head_ref,
                current,
            } => (
                format!(
                    "`--pr {number}` is for head branch `{head_ref}`, but you are on `{current}`."
                ),
                format!(
                    "Run `gh pr checkout {number}` first, then `cute-dbt review --pr` — \
                     review never checks out for you."
                ),
            ),
            // Explicit (never `_`): a new variant must fail to compile
            // here AND in describe_dbt/describe_project, forcing a
            // deliberate remediation decision — the exit.rs precedent.
            Self::ProjectNotFound { .. }
            | Self::ProjectAmbiguous { .. }
            | Self::ProjectOutsideRepo { .. }
            | Self::ManifestMissing { .. }
            | Self::DbtMissing
            | Self::EngineUnsupported(_)
            | Self::CompileFailed { .. }
            | Self::StageFailed { .. } => return None,
        };
        Some(pair)
    }

    /// The dbt/manifest arms; `None` for everything else.
    fn describe_dbt(&self) -> Option<(String, String)> {
        let pair = match self {
            Self::ManifestMissing {
                path,
                after_compile,
            } => {
                if *after_compile {
                    (
                        format!(
                            "dbt compile succeeded but no manifest appeared at `{}`.",
                            path.display()
                        ),
                        "Your dbt writes its target dir somewhere else — align it via \
                         `--target-path` / `DBT_TARGET_PATH`, then re-run."
                            .to_owned(),
                    )
                } else {
                    (
                        format!("no compiled manifest at `{}`.", path.display()),
                        "Run `dbt compile` in your dbt project first (or drop \
                         --no-compile and let review run it), then re-run \
                         `cute-dbt review`."
                            .to_owned(),
                    )
                }
            }
            Self::DbtMissing => (
                "`dbt` was not found on PATH.".to_owned(),
                "Install dbt (dbt-core 1.8+: `pip install dbt-core` plus your adapter; \
                 or the dbt Fusion engine) — or pass --no-compile to trust an \
                 already-compiled manifest without dbt."
                    .to_owned(),
            ),
            Self::EngineUnsupported(issue) => issue.describe(),
            Self::CompileFailed { status } => {
                let exit = status.map_or_else(
                    || "killed by a signal".to_owned(),
                    |code| format!("exit {code}"),
                );
                (
                    format!("dbt compile failed ({exit}) — no report was rendered."),
                    "Fix the dbt error above (its output is your dbt's own, relayed \
                     verbatim). Profile problems: check ~/.dbt/profiles.yml, \
                     --profiles-dir, or DBT_PROFILES_DIR, and run `dbt debug`. A \
                     manifest file may exist anyway — dbt writes it even on failed \
                     compiles — but review trusts the exit code, not the artifact."
                        .to_owned(),
                )
            }
            // Explicit (never `_`): see the describe_git twin.
            Self::GitMissing
            | Self::NoGitRepo
            | Self::BareRepo
            | Self::NoCommits
            | Self::BaseUndetectable
            | Self::BaseRefMissing { .. }
            | Self::ShallowClone { .. }
            | Self::DisjointHistories { .. }
            | Self::GhMissing
            | Self::NoPullRequest { .. }
            | Self::PrHeadMismatch { .. }
            | Self::ProjectNotFound { .. }
            | Self::ProjectAmbiguous { .. }
            | Self::ProjectOutsideRepo { .. }
            | Self::StageFailed { .. } => return None,
        };
        Some(pair)
    }

    /// The project/stage arms.
    ///
    /// # Panics
    ///
    /// Unreachable for the git/ladder and dbt variants —
    /// [`Self::describe`] routes those through [`Self::describe_git`]
    /// and [`Self::describe_dbt`] first.
    fn describe_project(&self) -> (String, String) {
        match self {
            Self::ProjectNotFound { searched, explicit } => {
                let what = if *explicit {
                    format!(
                        "no dbt_project.yml in `{}` (from --project-dir).",
                        searched.display()
                    )
                } else {
                    format!(
                        "no dbt_project.yml found in `{}` or one level down.",
                        searched.display()
                    )
                };
                (
                    what,
                    "Run from your dbt project (or its parent directory), or pass \
                     `--project-dir <dir>`."
                        .to_owned(),
                )
            }
            Self::ProjectAmbiguous { candidates } => {
                let list = candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    format!(
                        "found {} dbt projects one level down: {list}.",
                        candidates.len()
                    ),
                    "Pass `--project-dir <dir>` to pick one.".to_owned(),
                )
            }
            Self::ProjectOutsideRepo { project, toplevel } => (
                format!(
                    "the dbt project `{}` is outside the git repository at `{}`.",
                    project.display(),
                    toplevel.display()
                ),
                "Run `cute-dbt review` inside the repository that contains the dbt project."
                    .to_owned(),
            ),
            Self::StageFailed { context, detail } => (
                format!("failed while {context}: {detail}"),
                "Re-run with `--dry-run` to see every planned command, and run the failing \
                 one manually to investigate."
                    .to_owned(),
            ),
            // Explicit (never `_`): see the describe_git twin.
            Self::GitMissing
            | Self::NoGitRepo
            | Self::BareRepo
            | Self::NoCommits
            | Self::BaseUndetectable
            | Self::BaseRefMissing { .. }
            | Self::ShallowClone { .. }
            | Self::DisjointHistories { .. }
            | Self::GhMissing
            | Self::NoPullRequest { .. }
            | Self::PrHeadMismatch { .. }
            | Self::ManifestMissing { .. }
            | Self::DbtMissing
            | Self::EngineUnsupported(_)
            | Self::CompileFailed { .. } => {
                unreachable!("describe() routes git/ladder and dbt variants through their halves")
            }
        }
    }
}

// ===================================================================
// dbt engine detection (research-294 sweep-dbt-engine-mechanics §1)
// ===================================================================

/// The detected dbt engine, from `dbt --version` output shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbtEngine {
    /// The Rust engine — a **single-line** `dbt X.Y.Z` banner (clap's
    /// default version template). The OSS standalone build brands
    /// itself `dbt-core` and older betas `dbt-fusion`; the single-line
    /// shape, not the name, is the discriminator (python core is
    /// always multi-line).
    Fusion {
        /// The reported version string, verbatim.
        version: String,
    },
    /// Python dbt-core — the **multi-line** `Core:` / `- installed:`
    /// block. Already validated ≥ 1.8 (manifest schema v12).
    Core {
        /// The reported `installed:` version, verbatim.
        version: String,
    },
}

impl std::fmt::Display for DbtEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fusion { version } => write!(f, "fusion {version}"),
            Self::Core { version } => write!(f, "dbt-core {version}"),
        }
    }
}

/// Why `dbt --version` output could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineIssue {
    /// The dbt Cloud CLI (`Cloud CLI - x.y.z (…)`) — it compiles in
    /// dbt Cloud, not locally, so review cannot drive it.
    CloudCli,
    /// Python dbt-core older than 1.8 — pre-manifest-v12.
    CoreTooOld {
        /// The reported `installed:` version.
        version: String,
    },
    /// Output matching no known engine shape.
    Unrecognized {
        /// The first line of what `dbt --version` printed.
        first_line: String,
    },
}

impl EngineIssue {
    /// `(description, remediation)` for the [`ReviewError::EngineUnsupported`]
    /// message.
    fn describe(&self) -> (String, String) {
        match self {
            Self::CloudCli => (
                "the `dbt` on PATH is the dbt Cloud CLI, which compiles in dbt Cloud — \
                 review needs a local engine."
                    .to_owned(),
                "Install dbt-core 1.8+ (`pip install dbt-core` plus your adapter) or the \
                 dbt Fusion engine, ensure it wins on PATH — or pass --no-compile to \
                 trust an already-compiled manifest."
                    .to_owned(),
            ),
            Self::CoreTooOld { version } => (
                format!(
                    "dbt-core {version} predates manifest schema v12 (dbt 1.8) — its \
                     manifests cannot drive the report."
                ),
                "Upgrade to dbt-core 1.8 or newer (or the dbt Fusion engine), then re-run."
                    .to_owned(),
            ),
            Self::Unrecognized { first_line } => (
                format!("could not recognize `dbt --version` output: `{first_line}`."),
                "Pass --no-compile to trust an already-compiled manifest — and please \
                 report this output so detection can learn the shape."
                    .to_owned(),
            ),
        }
    }
}

/// Classify `dbt --version` output (the pure half of detection).
///
/// Order matters: the Cloud CLI banner is checked first (it also
/// carries a version-looking token); the multi-line `Core:` block next
/// (python core); any remaining single-line `name x.y.z` shape is the
/// Rust engine.
///
/// # Errors
///
/// [`EngineIssue`] when the output names the Cloud CLI, a pre-1.8
/// core, or no recognizable engine at all.
pub fn parse_dbt_version(stdout: &str) -> Result<DbtEngine, EngineIssue> {
    if stdout.contains("Cloud CLI") {
        return Err(EngineIssue::CloudCli);
    }
    if stdout.contains("Core:") {
        let version = stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix("- installed:"))
            .map(|rest| rest.split_whitespace().next().unwrap_or("").to_owned())
            .unwrap_or_default();
        if version.is_empty() {
            return Err(EngineIssue::Unrecognized {
                first_line: first_line_of(stdout),
            });
        }
        if core_meets_floor(&version) {
            return Ok(DbtEngine::Core { version });
        }
        return Err(EngineIssue::CoreTooOld { version });
    }
    let first = first_line_of(stdout);
    let mut tokens = first.split_whitespace();
    if let (Some(name), Some(version)) = (tokens.next(), tokens.next())
        && matches!(name, "dbt" | "dbt-fusion" | "dbt-core")
        && version.starts_with(|c: char| c.is_ascii_digit())
    {
        return Ok(DbtEngine::Fusion {
            version: version.to_owned(),
        });
    }
    Err(EngineIssue::Unrecognized { first_line: first })
}

/// The first non-empty line, trimmed (for messages and shape checks).
fn first_line_of(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// Whether a python dbt-core `installed:` version meets the 1.8 floor
/// (manifest schema v12). An unparseable version fails closed (false ⇒
/// `CoreTooOld` carries the raw string for the operator to judge).
fn core_meets_floor(version: &str) -> bool {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts
        .next()
        .map(|p| {
            p.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    (major, minor) >= (1, 8)
}

/// The review run's failure type: a wrapper-stage [`ReviewError`] or a
/// composed-report [`RunError`] (preflight / output) passing through
/// with its own remediation untouched.
pub enum ReviewFailure {
    /// A review-stage (git / detection / discovery) failure.
    Review(ReviewError),
    /// A failure raised by the composed report run loop.
    Run(RunError),
}

impl ReviewFailure {
    /// The operator-facing stderr message.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Review(err) => err.message(),
            Self::Run(err) => err.message(),
        }
    }
}

impl From<ReviewError> for ReviewFailure {
    fn from(err: ReviewError) -> Self {
        Self::Review(err)
    }
}

// ===================================================================
// Base-detection ladder (research-294 sweep-scope-detection §1)
// ===================================================================

/// One candidate ref with its `git rev-parse --verify` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefProbe {
    /// The candidate ref name.
    pub name: String,
    /// Whether it resolves to a commit.
    pub resolves: bool,
}

/// The observed facts the ladder decides over. The impure gatherer
/// ([`gather_base_facts`]) fills every field with cheap read-only git
/// queries; [`decide_base`] is the pure decision table.
#[derive(Debug, Default)]
pub struct BaseFacts {
    /// `--base`, verified.
    pub explicit: Option<RefProbe>,
    /// `git config cute-dbt.base`, verified.
    pub configured: Option<RefProbe>,
    /// The open PR's base, resolved to a verified local ref via `gh`
    /// (e.g. `origin/main`). Fail-soft: `None` whenever `gh` is absent,
    /// HEAD is detached, there is no open PR, the call times out, or the
    /// resolved ref is not present locally — the ladder falls through.
    pub pr_base: Option<String>,
    /// The full open-PR context the gh rung saw (cute-dbt#346) — carried
    /// through so the change-context banner can link to the PR even on the
    /// fail-soft auto-ladder. `Some` exactly when `gh pr view` returned a
    /// usable PR (independent of whether the gh rung *won* the ladder).
    pub pr_info: Option<PrInfo>,
    /// The `origin/HEAD` symref target (e.g. `origin/main`), present
    /// only when the symref exists **and** resolves (a stale symref
    /// falls through to the probes).
    pub origin_head: Option<String>,
    /// The first existing ref among `refs/remotes/origin/{main,master,trunk}`.
    pub remote_probe: Option<String>,
    /// The first existing ref among `refs/heads/{main,master,trunk}`.
    pub local_probe: Option<String>,
}

/// Which ladder rung answered — announced on stderr so the operator can
/// see (and correct) what review assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Rung 0: the `--base` flag.
    ExplicitFlag,
    /// Rung 1: the persisted `git config cute-dbt.base`.
    GitConfig,
    /// Rung 2: the open PR's base, via `gh pr view`.
    GhPr,
    /// Rung 3: the `origin/HEAD` symref.
    OriginHead,
    /// Rung 4a: probing `origin/{main,master,trunk}`.
    RemoteProbe,
    /// Rung 4b: probing local `{main,master,trunk}` heads.
    LocalProbe,
}

impl Rung {
    /// Short human label for the stderr announcement.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::ExplicitFlag => "--base",
            Self::GitConfig => "git config cute-dbt.base",
            Self::GhPr => "gh pr view (open PR base)",
            Self::OriginHead => "origin/HEAD",
            Self::RemoteProbe => "origin/{main,master,trunk} probe",
            Self::LocalProbe => "local main/master/trunk probe",
        }
    }
}

/// The pure ladder decision table: first rung with an answer wins; an
/// operator-supplied ref that does not resolve is an error (never a
/// silent fall-through — the operator asked for something specific).
///
/// # Errors
///
/// [`ReviewError::BaseRefMissing`] when `--base` / the configured base
/// does not resolve; [`ReviewError::BaseUndetectable`] when every rung
/// is empty.
pub fn decide_base(facts: &BaseFacts) -> Result<(String, Rung), ReviewError> {
    if let Some(probe) = &facts.explicit {
        if probe.resolves {
            return Ok((probe.name.clone(), Rung::ExplicitFlag));
        }
        return Err(ReviewError::BaseRefMissing {
            ref_name: probe.name.clone(),
            source: BaseSource::Flag,
        });
    }
    if let Some(probe) = &facts.configured {
        if probe.resolves {
            return Ok((probe.name.clone(), Rung::GitConfig));
        }
        return Err(ReviewError::BaseRefMissing {
            ref_name: probe.name.clone(),
            source: BaseSource::GitConfig,
        });
    }
    if let Some(name) = &facts.pr_base {
        return Ok((name.clone(), Rung::GhPr));
    }
    if let Some(name) = &facts.origin_head {
        return Ok((name.clone(), Rung::OriginHead));
    }
    if let Some(name) = &facts.remote_probe {
        return Ok((name.clone(), Rung::RemoteProbe));
    }
    if let Some(name) = &facts.local_probe {
        return Ok((name.clone(), Rung::LocalProbe));
    }
    Err(ReviewError::BaseUndetectable)
}

/// Diagnose a failed `git merge-base`: a shallow clone gets the
/// `--unshallow` remediation; a full clone means the histories are
/// genuinely disjoint.
#[must_use]
pub fn diagnose_no_merge_base(base: &str, is_shallow: bool) -> ReviewError {
    if is_shallow {
        ReviewError::ShallowClone {
            base: base.to_owned(),
        }
    } else {
        ReviewError::DisjointHistories {
            base: base.to_owned(),
        }
    }
}

// ===================================================================
// `gh pr view` — the open-PR base rung + the --pr anchor
// (research-294 sweep-scope-detection §1)
// ===================================================================

/// The fields review reads from `gh pr view --json
/// baseRefName,headRefName,number,title,url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    /// The PR's base branch (e.g. `main`) — becomes the review base.
    pub base_ref: String,
    /// The PR's head branch — `--pr <n>` asserts HEAD is on this.
    pub head_ref: String,
    /// The PR number.
    pub number: u64,
    /// The PR title (cute-dbt#346) — feeds the change-context banner link.
    /// Empty when the manifest carries no title (the banner then renders
    /// link-free — both a url and a title are required).
    pub title: String,
    /// The PR's GitHub URL (cute-dbt#346) — the `<a href>` the banner
    /// links to. Empty when absent (banner renders link-free).
    pub url: String,
}

/// Parse the `gh pr view --json baseRefName,headRefName,number,title,url`
/// payload. `None` for any shape review cannot use (not an object, missing
/// / blank `baseRefName`) — the caller treats `None` as "no usable PR".
#[must_use]
pub fn parse_pr_info(json: &str) -> Option<PrInfo> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let base_ref = value.get("baseRefName")?.as_str()?.trim().to_owned();
    if base_ref.is_empty() {
        return None;
    }
    let head_ref = value
        .get("headRefName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let number = value
        .get("number")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let title = value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    Some(PrInfo {
        base_ref,
        head_ref,
        number,
        title,
        url,
    })
}

/// Resolve a PR base branch name to a verified local ref. A `gh`
/// `baseRefName` is a bare branch (`main`); the review base must be the
/// remote-tracking ref (`origin/main`) so the diff matches what the PR
/// shows. Returns `None` (fall through) if neither the remote-tracking
/// nor a local ref resolves.
fn resolve_pr_base_ref(toplevel: &Path, base_ref: &str) -> Result<Option<String>, ReviewError> {
    let remote = format!("origin/{base_ref}");
    if ref_resolves(toplevel, &remote)? {
        return Ok(Some(remote));
    }
    if ref_resolves(toplevel, base_ref)? {
        return Ok(Some(base_ref.to_owned()));
    }
    Ok(None)
}

/// Decide whether `--pr <n>` is satisfied by the current checkout:
/// pure. `Ok(())` when the PR's head branch is the current branch;
/// otherwise the mismatch error (review never checks out for you).
/// Bare `--pr` (`requested = None`) imposes no head assertion.
///
/// # Errors
///
/// [`ReviewError::PrHeadMismatch`] when an explicit `--pr <n>`'s head
/// branch is not the currently checked-out branch.
pub fn check_pr_head(
    requested: Option<u64>,
    pr: &PrInfo,
    current_branch: Option<&str>,
) -> Result<(), ReviewError> {
    let Some(number) = requested else {
        return Ok(()); // bare --pr: no head assertion.
    };
    if current_branch == Some(pr.head_ref.as_str()) {
        return Ok(());
    }
    Err(ReviewError::PrHeadMismatch {
        number,
        head_ref: pr.head_ref.clone(),
        current: current_branch.unwrap_or("a detached HEAD").to_owned(),
    })
}

// ===================================================================
// Project discovery
// ===================================================================

/// Discover the dbt project: `start` itself if it holds a
/// `dbt_project.yml`, else exactly one immediate subdirectory that
/// does.
///
/// # Errors
///
/// [`ReviewError::ProjectNotFound`] (zero candidates),
/// [`ReviewError::ProjectAmbiguous`] (more than one, listed sorted),
/// or [`ReviewError::StageFailed`] when `start` cannot be read.
pub fn discover_project_dir(start: &Path) -> Result<PathBuf, ReviewError> {
    if start.join("dbt_project.yml").is_file() {
        return Ok(start.to_path_buf());
    }
    let entries = fs::read_dir(start).map_err(|err| ReviewError::StageFailed {
        context: "scanning for dbt_project.yml",
        detail: err.to_string(),
    })?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("dbt_project.yml").is_file())
        .collect();
    candidates.sort();
    match candidates.len() {
        0 => Err(ReviewError::ProjectNotFound {
            searched: start.to_path_buf(),
            explicit: false,
        }),
        1 => Ok(candidates.remove(0)),
        _ => Err(ReviewError::ProjectAmbiguous { candidates }),
    }
}

// ===================================================================
// Command plans — the plan/execute seam
// ===================================================================

/// One planned subprocess invocation: argv, working directory, and the
/// environment additions. Built **before** execution; `--dry-run`
/// prints it, a real run executes it — the same value, so the listing
/// cannot drift from reality. [`CommandPlan::to_command`] is the only
/// plan-to-`Command` mapping and is pinned field-for-field by a unit
/// test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    /// The program to spawn.
    pub program: &'static str,
    /// Its arguments, in order.
    pub args: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// Environment variables set on the child (added to the inherited
    /// environment).
    pub env: Vec<(&'static str, &'static str)>,
}

impl CommandPlan {
    /// Map the plan onto a [`Command`] — the single spawn mapping.
    #[must_use]
    pub fn to_command(&self) -> Command {
        let mut cmd = Command::new(self.program);
        cmd.args(&self.args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null());
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd
    }

    /// Execute the plan, capturing output.
    ///
    /// # Errors
    ///
    /// The underlying [`io::Error`] when the program cannot be spawned.
    pub fn execute(&self) -> io::Result<Output> {
        self.to_command().output()
    }

    /// Execute the plan with stdout/stderr **inherited** — the compile
    /// step's wiring, so dbt's own progress and errors stream to the
    /// operator's terminal verbatim (never buffered, never reworded).
    /// Same [`CommandPlan::to_command`] mapping as [`CommandPlan::execute`];
    /// only the io wiring differs.
    ///
    /// # Errors
    ///
    /// The underlying [`io::Error`] when the program cannot be spawned.
    pub fn execute_streaming(&self) -> io::Result<std::process::ExitStatus> {
        self.to_command().status()
    }

    /// Render the plan as one shell-style line for the `--dry-run`
    /// listing: `(cwd: …) KEY=VALUE program args…`.
    #[must_use]
    pub fn rendered(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (key, value) in &self.env {
            parts.push(format!("{key}={value}"));
        }
        parts.push(self.program.to_owned());
        parts.extend(self.args.iter().map(|a| shell_quote(a)));
        format!("(cwd: {}) {}", self.cwd.display(), parts.join(" "))
    }
}

/// Quote an argument for display when it carries characters a shell
/// would interpret (the `:(exclude)…` pathspec, spaces in paths).
/// Display-only — execution passes argv discretely, never via a shell.
fn shell_quote(arg: &str) -> String {
    let safe = arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_-./=@:,+".contains(c));
    if safe && !arg.is_empty() {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Which endpoints the diff covers (the `review_scope` selector).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    /// Default: merge-base → working tree (single-rev form `git diff
    /// $MB`) — includes committed + staged + unstaged, matching what
    /// `dbt compile` compiled (the same-revision contract).
    WorkingTree,
    /// `--committed-only`: merge-base → HEAD (`$MB..HEAD`) — PR-exact
    /// parity.
    CommittedOnly,
    /// `--staged`: HEAD → the index (`git diff --cached`) — what is
    /// staged for commit. Revision-independent of the merge-base.
    Staged,
    /// `--unstaged`: the index → the working tree (bare `git diff`) —
    /// edits not yet staged. Revision-independent of the merge-base.
    Unstaged,
}

impl DiffScope {
    /// The revision/selector argument(s) for `git diff` under this scope
    /// — slotted in after the config-proof flags and before `--`. The
    /// staged/unstaged forms carry no merge-base (they are index-
    /// relative); `--staged` adds `--cached`, `--unstaged` adds nothing.
    fn rev_args(self, merge_base: &str) -> Vec<String> {
        match self {
            Self::WorkingTree => vec![merge_base.to_owned()],
            Self::CommittedOnly => vec![format!("{merge_base}..HEAD")],
            Self::Staged => vec!["--cached".to_owned()],
            Self::Unstaged => Vec::new(),
        }
    }

    /// Resolve the scope from the parsed flags. The `review_scope`
    /// `ArgGroup` guarantees at most one is set, so the order here is a
    /// formality; the default (none set) is the working-tree endpoint.
    #[must_use]
    pub fn from_flags(committed_only: bool, staged: bool, unstaged: bool) -> Self {
        if committed_only {
            Self::CommittedOnly
        } else if staged {
            Self::Staged
        } else if unstaged {
            Self::Unstaged
        } else {
            Self::WorkingTree
        }
    }

    /// Whether this scope diffs the index rather than the working tree.
    /// `--staged` is the one variant that can disagree with the
    /// compiled manifest (always built from the working tree), so it is
    /// the only scope that runs the same-revision drift check.
    #[must_use]
    pub fn diffs_the_index(self) -> bool {
        matches!(self, Self::Staged)
    }
}

/// The pathspec pair limiting a git command to the project, excluding
/// its `target/` build output: `<rel>/ :(exclude)<rel>/target/` — or
/// `.` / `:(exclude)target/` when the project **is** the repo toplevel
/// (`project_rel` empty).
fn project_pathspec(project_rel: &str) -> [String; 2] {
    if project_rel.is_empty() {
        [".".to_owned(), ":(exclude)target/".to_owned()]
    } else {
        [
            format!("{project_rel}/"),
            format!(":(exclude){project_rel}/target/"),
        ]
    }
}

/// Build the canonical diff plan — the research-294 §3 config-proof
/// flag set, **verbatim**: every flag neutralizes a user config that
/// would otherwise silently break the `+++ b/` parser or the
/// block-precise hunks (`diff.noprefix`, `diff.mnemonicPrefix`,
/// external diff drivers, textconv, `diff.relative`, color,
/// `diff.context`, `diff.renames`, submodule formats). cwd is the repo
/// toplevel and `LC_ALL=C` rides the child env (locale-stable output).
#[must_use]
pub fn diff_plan(
    toplevel: &Path,
    project_rel: &str,
    merge_base: &str,
    scope: DiffScope,
) -> CommandPlan {
    let mut args: Vec<String> = [
        "-c",
        "diff.noprefix=false",
        "-c",
        "diff.mnemonicPrefix=false",
        "--no-pager",
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-relative",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--unified=0",
        "--find-renames",
        "--submodule=short",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.extend(scope.rev_args(merge_base));
    args.push("--".to_owned());
    args.extend(project_pathspec(project_rel));
    CommandPlan {
        program: "git",
        args,
        cwd: toplevel.to_path_buf(),
        env: vec![("LC_ALL", "C")],
    }
}

/// Build the untracked-files scan plan: `git status --porcelain` over
/// the same project pathspec the diff uses.
#[must_use]
pub fn status_plan(toplevel: &Path, project_rel: &str) -> CommandPlan {
    let mut args: Vec<String> = ["status", "--porcelain", "--"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.extend(project_pathspec(project_rel));
    CommandPlan {
        program: "git",
        args,
        cwd: toplevel.to_path_buf(),
        env: vec![("LC_ALL", "C")],
    }
}

/// Parse `git status --porcelain` output for the same-revision drift
/// signal on `--staged`: files whose **index** status AND **worktree**
/// status are both non-blank — i.e. they are staged *and* carry further
/// unstaged edits. For such a file the `--staged` diff (HEAD → index)
/// and the manifest (compiled from the working tree) disagree, so the
/// inline diff would mislead.
///
/// Porcelain v1 format: two status columns `XY` then a space then the
/// path (`XY path`, or `XY orig -> path` for renames — the post-rename
/// path is the one that matters and is taken). Untracked `??` and
/// ignored `!!` lines have a blank index column, so they never count as
/// drift. Pure — the I/O lives in the caller.
#[must_use]
pub fn staged_files_with_unstaged_edits(porcelain: &str) -> Vec<String> {
    porcelain.lines().filter_map(porcelain_drift_path).collect()
}

/// One porcelain line → the drifting path, if it is staged-with-unstaged
/// edits. `None` otherwise.
fn porcelain_drift_path(line: &str) -> Option<String> {
    // A status line is `XY<space>path`; anything shorter is noise.
    if line.len() < 4 {
        return None;
    }
    let bytes = line.as_bytes();
    let index = bytes[0] as char;
    let worktree = bytes[1] as char;
    // Both columns must be a real status (not blank, not `?`/`!`): an
    // unstaged-only or staged-only change is fine; the drift is the
    // BOTH case (e.g. `MM`, `AM`, `MD`).
    let drifting = index != ' ' && index != '?' && index != '!' && worktree != ' ';
    if !drifting {
        return None;
    }
    let path = line[3..].trim();
    // Rename/copy form `orig -> new`: the post-rename path is what the
    // diff names.
    let resolved = path.rsplit(" -> ").next().unwrap_or(path);
    Some(resolved.to_owned())
}

/// Build the `dbt --version` detection plan (cwd = the project dir, so
/// any directory-sensitive dbt wrapper behaves as it would for the
/// user's own invocation).
#[must_use]
pub fn version_plan(project_dir: &Path) -> CommandPlan {
    CommandPlan {
        program: "dbt",
        args: vec!["--version".to_owned()],
        cwd: project_dir.to_path_buf(),
        env: Vec::new(),
    }
}

/// Build the full-project `dbt compile` plan — cwd = the project dir,
/// exactly the invocation the user would type. A `--target-path` flag
/// is forwarded so dbt writes where review reads; `DBT_TARGET_PATH`
/// needs no forwarding (dbt reads the inherited environment itself).
/// Review adds **no** other flags and **no** env: the compile is the
/// user's own dbt doing its own thing (privacy switches are documented,
/// never silently injected).
#[must_use]
pub fn compile_plan(project_dir: &Path, target_path: Option<&Path>) -> CommandPlan {
    let mut args = vec!["compile".to_owned()];
    if let Some(dir) = target_path {
        args.push("--target-path".to_owned());
        args.push(dir.display().to_string());
    }
    CommandPlan {
        program: "dbt",
        args,
        cwd: project_dir.to_path_buf(),
        env: Vec::new(),
    }
}

// ===================================================================
// In-process composition (the no-drift seam)
// ===================================================================

/// The inputs the composed report run is built from. One struct feeds
/// **both** [`ComposeInputs::report_display_argv`] (the `--dry-run`
/// listing) and [`ComposeInputs::report_args`] (the in-process
/// [`ReportArgs`]), so the displayed invocation and the executed one
/// cannot drift — pinned by `report_plan_matches_composed_args`.
#[derive(Debug)]
pub struct ComposeInputs {
    /// The compiled manifest (absolute).
    pub manifest: PathBuf,
    /// The project root **relative to the repo toplevel** (`.` when the
    /// project is the toplevel) — the same shape the CI recipe passes,
    /// so the diff-side path strip and the working-tree YAML reads both
    /// resolve once the run loop executes with cwd = toplevel.
    pub project_root: PathBuf,
    /// The report output path (absolute).
    pub out: PathBuf,
    /// The pass-through `--config`, when given.
    pub config: Option<ReviewConfig>,
    /// The resolved open-PR context (cute-dbt#346) — `Some` when `gh pr
    /// view` returned a usable PR (the auto-ladder gh rung or the `--pr`
    /// anchor). Feeds the change-context banner link: its url + title
    /// populate the composed [`ReportArgs`]'s `--pr-*` fields. `None` ⇒ the
    /// banner renders link-free (graceful degradation: local review with no
    /// PR, or `gh` absent).
    pub pr: Option<PrInfo>,
}

/// Placeholder standing for the diff text in the displayed report
/// invocation (a real run passes it in-process, never via a file).
const DIFF_STAND_IN: &str = "@<git-diff-output>";

impl ComposeInputs {
    /// The equivalent `cute-dbt report` argv for the `--dry-run`
    /// listing.
    #[must_use]
    pub fn report_display_argv(&self) -> Vec<String> {
        let mut argv = vec![
            "cute-dbt".to_owned(),
            "report".to_owned(),
            "--manifest".to_owned(),
            self.manifest.display().to_string(),
            "--pr-diff".to_owned(),
            DIFF_STAND_IN.to_owned(),
            "--project-root".to_owned(),
            self.project_root.display().to_string(),
            "--out".to_owned(),
            self.out.display().to_string(),
        ];
        if let Some(config) = &self.config {
            argv.push("--config".to_owned());
            argv.push(config.path.display().to_string());
        }
        argv
    }

    /// The in-process [`ReportArgs`] — the same fields the display argv
    /// names, plus the parsed diff and the untouched experimental
    /// pass-through. `baseline_manifest` stays `None` and
    /// `modified_selectors` stays empty by construction: review is the
    /// pr-diff arm's local twin and never auto-selects the baseline arm.
    #[must_use]
    pub fn report_args(
        &self,
        diff: crate::domain::PrDiff,
        experimental: Option<BTreeSet<Experiment>>,
    ) -> ReportArgs {
        ReportArgs {
            manifest: self.manifest.clone(),
            baseline_manifest: None,
            pr_diff: Some(diff),
            out: self.out.clone(),
            config: self.config.as_ref().map(|c| c.parsed.clone()),
            project_root: Some(self.project_root.clone()),
            modified_selectors: Vec::new(),
            experimental,
            // `review` has no direct `--macro-body-cap` flag — the cap
            // rides `[experimental] macro_body_cap` in `--config` (read via
            // `resolve_macro_body_cap`) or the default (cute-dbt#265 Slice
            // D). `None` defers to that config/default ladder.
            macro_body_cap: None,
            // cute-dbt#346 — the change-context banner link, derived from
            // `gh pr view` (number + title + url). `review` populates the
            // report's `--pr-*` fields directly (rather than synthesizing a
            // `[pr]` config), so the link rides EVERY review run that has a
            // resolvable PR — no extra config required. The renderer gates
            // it to the pr-diff arm (review is always pr-diff) and to
            // url-and-title presence (`PrConfig::resolve`). The pass-through
            // `--config` `[pr]` section still applies for keys the PrInfo
            // leaves blank, but the derived values win (CLI-over-TOML).
            pr_url: self.pr.as_ref().map(|p| p.url.clone()),
            pr_title: self.pr.as_ref().map(|p| p.title.clone()),
            pr_number: self.pr.as_ref().map(|p| p.number),
            // cute-dbt#386 — the findings-envelope sidecar + coverage gate
            // are `report`-verb surfaces; `review` does not emit the
            // envelope or apply the gate in this slice (both off). A future
            // slice can wire `review`'s own `--findings-out` / gate flags
            // through here.
            findings_out: None,
            fail_on_uncovered: false,
            // No envelope on `review` ⇒ the `--generated-at` override is
            // inert; `None` defers to the I/O-boundary date either way.
            generated_at: None,
        }
    }
}

/// Resolve the dbt target directory: the `--target-path` flag wins,
/// then `DBT_TARGET_PATH`, else the conventional `<project>/target`.
/// Relative values join the project dir — fusion's `in_out_dir`
/// semantics (dbt-core's `flag > env > dbt_project.yml` ladder agrees
/// on the flag/env half; review never reads `target-path:` from the
/// YAML, matching fusion).
#[must_use]
pub fn resolve_target_dir(
    project_dir: &Path,
    flag: Option<&Path>,
    dbt_target_path: Option<&str>,
) -> PathBuf {
    let chosen: Option<PathBuf> = match (flag, dbt_target_path) {
        (Some(p), _) => Some(p.to_path_buf()),
        (None, Some(p)) if !p.trim().is_empty() => Some(PathBuf::from(p)),
        _ => None,
    };
    match chosen {
        Some(path) if path.is_absolute() => path,
        Some(path) => project_dir.join(path),
        None => project_dir.join("target"),
    }
}

/// Whether the report should auto-open: never when `--no-open`, never
/// without an interactive terminal (so tests, scripts, agents, and CI
/// can never trigger it).
#[must_use]
pub fn should_open(no_open: bool, stdout_is_terminal: bool) -> bool {
    !no_open && stdout_is_terminal
}

/// The platform opener invocation for a rendered report.
#[must_use]
pub fn opener_invocation(path: &Path) -> (&'static str, Vec<String>) {
    let target = path.display().to_string();
    if cfg!(target_os = "macos") {
        ("open", vec![target])
    } else if cfg!(target_os = "windows") {
        (
            "cmd",
            vec!["/c".to_owned(), "start".to_owned(), String::new(), target],
        )
    } else {
        ("xdg-open", vec![target])
    }
}

// ===================================================================
// Execution
// ===================================================================

/// The named `review` run loop: `ensure_git_repo` →
/// `resolve_project` → `detect_base` → `find_merge_base` → build plans
/// (`--dry-run` prints them and stops) → execute the diff → compose the
/// existing report run loop in-process → announce + auto-open.
///
/// # Errors
///
/// Any [`ReviewError`] from the wrapper stages, or the composed
/// report's own [`RunError`] passing through.
pub fn execute_review(args: &ReviewArgs) -> Result<(), ReviewFailure> {
    let cwd = current_dir()?;
    let layout = resolve_review_layout(args, &cwd)?;
    let (merge_base, pr_info) = resolve_base(args, &layout.toplevel)?;

    let scope = DiffScope::from_flags(args.committed_only, args.staged, args.unstaged);
    let diff = diff_plan(&layout.toplevel, &layout.project_rel, &merge_base, scope);
    // The compile plan exists exactly when the run would compile —
    // `--dry-run` prints it from the SAME value a real run executes.
    let compile =
        (!args.no_compile).then(|| compile_plan(&layout.project_dir, args.target_path.as_deref()));
    let compose = build_compose_inputs(args, &cwd, &layout, pr_info);

    if args.dry_run {
        print_dry_run(&diff, compile.as_ref(), &compose);
        return Ok(());
    }

    warn_untracked(&layout.toplevel, &layout.project_rel);

    // Diff before compile: an empty diff exits here without spending
    // the (potentially slow) compile on nothing to review.
    let Some(pr_diff) = scope_diff(args, &diff, &merge_base)? else {
        return Ok(()); // "nothing to review" — already announced.
    };

    // Same-revision drift (--staged only): the diff is HEAD → index, but
    // the manifest is compiled from the working tree. A file that is
    // both staged AND further edited unstaged makes the two disagree —
    // warn (the drift-guard degrades those inline diffs gracefully).
    if scope.diffs_the_index() {
        warn_staged_drift(&layout.toplevel, &layout.project_rel);
    }

    ensure_manifest(args, &layout.project_dir, &compose.manifest)?;
    compose_and_render(args, &layout.toplevel, &compose, pr_diff)
}

/// The directory + project layout a review run resolves once up front:
/// the operator's cwd, the repo toplevel, the dbt project dir, and the
/// project's path relative to the toplevel.
struct ReviewLayout {
    toplevel: PathBuf,
    project_dir: PathBuf,
    project_rel: String,
}

/// The current working directory, mapped to a review-stage error.
fn current_dir() -> Result<PathBuf, ReviewError> {
    env::current_dir().map_err(|err| ReviewError::StageFailed {
        context: "reading the working directory",
        detail: err.to_string(),
    })
}

/// Resolve + announce the directory layout (git preconditions, the dbt
/// project, the toplevel-relative path).
fn resolve_review_layout(args: &ReviewArgs, cwd: &Path) -> Result<ReviewLayout, ReviewError> {
    ensure_git_repo(cwd)?;
    let toplevel = git_toplevel(cwd)?;
    let project_dir = resolve_project_dir(args.project_dir.as_deref(), cwd)?;
    let project_rel = project_rel_to_toplevel(&project_dir, &toplevel)?;
    eprintln!("cute-dbt: dbt project: {}", project_dir.display());
    Ok(ReviewLayout {
        toplevel,
        project_dir,
        project_rel,
    })
}

/// Walk the base-detection ladder, announce the answering rung, and
/// return the merge-base SHA plus the open-PR context (cute-dbt#346 — the
/// change-context banner link, `None` when no PR resolved).
fn resolve_base(
    args: &ReviewArgs,
    toplevel: &Path,
) -> Result<(String, Option<PrInfo>), ReviewError> {
    let (base_ref, rung, pr_info) = if let Some(requested) = args.pr {
        // `--pr [<n>]`: anchor to the open PR (gh failures surfaced).
        let (base, rung, pr) = resolve_pr_anchor_base(toplevel, requested)?;
        (base, rung, Some(pr))
    } else {
        // The auto-ladder (the gh rung is one fail-soft step within). The
        // PR context the gh rung saw rides along regardless of which rung
        // won, so the banner can link even when the base came from another
        // rung.
        let facts = gather_base_facts(toplevel, args.base.as_deref())?;
        let pr_info = facts.pr_info.clone();
        let (base, rung) = decide_base(&facts)?;
        (base, rung, pr_info)
    };
    let merge_base = find_merge_base(toplevel, &base_ref)?;
    eprintln!(
        "cute-dbt: base: {base_ref} (via {}; merge-base {})",
        rung.describe(),
        short_sha(&merge_base),
    );
    Ok((merge_base, pr_info))
}

/// The explicit `--pr [<n>]` base: require an open PR (gh failures are
/// surfaced, not silenced), assert the head branch on `--pr <n>` (never
/// checking out), then resolve the PR's base to a verified local ref.
/// Returns the resolved base, the rung, and the full PR context (the
/// banner link, cute-dbt#346).
fn resolve_pr_anchor_base(
    toplevel: &Path,
    requested: Option<u64>,
) -> Result<(String, Rung, PrInfo), ReviewError> {
    // Query PR #requested explicitly (bare `--pr` ⇒ None ⇒ the current
    // branch's PR), so `--pr <n>` resolves PR #n's base — not whatever
    // PR the current branch happens to have (cute-dbt#303 bot review).
    let pr = require_gh_pr(toplevel, requested)?;
    check_pr_head(requested, &pr, current_branch(toplevel)?.as_deref())?;
    let base = resolve_pr_base_ref(toplevel, &pr.base_ref)?.ok_or_else(|| {
        ReviewError::BaseRefMissing {
            ref_name: pr.base_ref.clone(),
            source: BaseSource::PullRequest,
        }
    })?;
    Ok((base, Rung::GhPr, pr))
}

/// Build the in-process report composition inputs from the resolved
/// layout (manifest location, project root, out path, config) and the
/// resolved open-PR context (cute-dbt#346 — the banner link, `None` when
/// no PR resolved).
fn build_compose_inputs(
    args: &ReviewArgs,
    cwd: &Path,
    layout: &ReviewLayout,
    pr: Option<PrInfo>,
) -> ComposeInputs {
    let target_dir = resolve_target_dir(
        &layout.project_dir,
        args.target_path.as_deref(),
        env::var("DBT_TARGET_PATH").ok().as_deref(),
    );
    ComposeInputs {
        manifest: target_dir.join("manifest.json"),
        project_root: rel_or_dot(&layout.project_rel),
        out: resolve_out_path(args.out.as_deref(), cwd, &target_dir),
        config: args.config.clone(),
        pr,
    }
}

/// Run the diff plan and parse it. `Ok(None)` is the deliberate
/// empty-diff exit ("nothing to review", announced here, exit 0 —
/// unless `--force` renders the zero-scope report).
fn scope_diff(
    args: &ReviewArgs,
    diff: &CommandPlan,
    merge_base: &str,
) -> Result<Option<crate::domain::PrDiff>, ReviewError> {
    let patch = run_diff(diff)?;
    if patch.trim().is_empty() && !args.force {
        eprintln!(
            "cute-dbt: nothing to review — no changes vs the base (merge-base {}). \
             No report written; pass --force to render the zero-scope report anyway.",
            short_sha(merge_base),
        );
        return Ok(None);
    }
    let pr_diff = parse_diff(&patch).map_err(|detail| ReviewError::StageFailed {
        context: "parsing the diff git produced",
        detail,
    })?;
    Ok(Some(pr_diff))
}

/// The manifest stage: run the user's own `dbt compile` (exit code is
/// the success signal — fusion writes manifest.json even on failed
/// compiles), or on `--no-compile` trust the existing manifest after a
/// staleness warning. Either way the manifest must exist afterward.
fn ensure_manifest(
    args: &ReviewArgs,
    project_dir: &Path,
    manifest: &Path,
) -> Result<(), ReviewError> {
    if args.no_compile {
        if !manifest.is_file() {
            return Err(ReviewError::ManifestMissing {
                path: manifest.to_path_buf(),
                after_compile: false,
            });
        }
        warn_if_stale(manifest, project_dir);
        return Ok(());
    }
    run_compile_stage(
        project_dir,
        &compile_plan(project_dir, args.target_path.as_deref()),
    )?;
    if manifest.is_file() {
        Ok(())
    } else {
        Err(ReviewError::ManifestMissing {
            path: manifest.to_path_buf(),
            after_compile: true,
        })
    }
}

/// Compose the existing report run loop in-process and finish: move to
/// the repo toplevel (the CI recipe's cwd = checkout root + relative
/// `--project-root` shape; every operator path was absolutized
/// already), render, print the path, auto-open on a TTY.
fn compose_and_render(
    args: &ReviewArgs,
    toplevel: &Path,
    compose: &ComposeInputs,
    pr_diff: crate::domain::PrDiff,
) -> Result<(), ReviewFailure> {
    env::set_current_dir(toplevel).map_err(|err| ReviewError::StageFailed {
        context: "moving to the repository toplevel",
        detail: err.to_string(),
    })?;
    let report_args = compose.report_args(pr_diff, args.experimental.clone());
    super::execute_report(&report_args).map_err(ReviewFailure::Run)?;
    println!("report written to {}", compose.out.display());
    maybe_open(&compose.out, args.no_open);
    Ok(())
}

/// First 12 hex characters of a SHA for human-facing messages.
fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// `rel` as a [`PathBuf`], with the empty (project == toplevel) case
/// spelled `.` so the composed run loop's working-tree reads resolve.
fn rel_or_dot(project_rel: &str) -> PathBuf {
    if project_rel.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(project_rel)
    }
}

/// Resolve `--out`: the operator's path absolutized against their
/// original cwd (never the repo toplevel the run loop later moves to),
/// defaulting to `<target-dir>/cute-dbt-report.html`.
fn resolve_out_path(out: Option<&Path>, cwd: &Path, target_dir: &Path) -> PathBuf {
    match out {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => cwd.join(p),
        None => target_dir.join("cute-dbt-report.html"),
    }
}

/// Run a read-only git query, mapping a missing binary to
/// [`ReviewError::GitMissing`].
fn git_query(cwd: &Path, args: &[&str]) -> Result<Output, ReviewError> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                ReviewError::GitMissing
            } else {
                ReviewError::StageFailed {
                    context: "spawning git",
                    detail: err.to_string(),
                }
            }
        })
}

/// `stdout` of a git query, trimmed.
fn stdout_line(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Preconditions: a non-bare git repository with at least one commit.
fn ensure_git_repo(cwd: &Path) -> Result<(), ReviewError> {
    let inside = git_query(cwd, &["rev-parse", "--git-dir"])?;
    if !inside.status.success() {
        return Err(ReviewError::NoGitRepo);
    }
    let bare = git_query(cwd, &["rev-parse", "--is-bare-repository"])?;
    if stdout_line(&bare) == "true" {
        return Err(ReviewError::BareRepo);
    }
    let head = git_query(cwd, &["rev-parse", "-q", "--verify", "HEAD^{commit}"])?;
    if !head.status.success() {
        return Err(ReviewError::NoCommits);
    }
    Ok(())
}

/// The repository toplevel, canonicalized (macOS `/var` → `/private/var`
/// symlinks would otherwise break the project-vs-toplevel prefix check).
fn git_toplevel(cwd: &Path) -> Result<PathBuf, ReviewError> {
    let out = git_query(cwd, &["rev-parse", "--show-toplevel"])?;
    if !out.status.success() {
        return Err(ReviewError::StageFailed {
            context: "resolving the repository toplevel",
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    canonicalized(Path::new(&stdout_line(&out)))
}

/// Canonicalize a path, wrapping the io failure.
fn canonicalized(path: &Path) -> Result<PathBuf, ReviewError> {
    fs::canonicalize(path).map_err(|err| ReviewError::StageFailed {
        context: "canonicalizing a path",
        detail: format!("{}: {err}", path.display()),
    })
}

/// The effective project directory: the validated `--project-dir`
/// (which must actually hold a `dbt_project.yml`) or discovery from the
/// working directory. Always canonicalized.
fn resolve_project_dir(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf, ReviewError> {
    if let Some(dir) = explicit {
        let abs = canonicalized(&cwd.join(dir))?;
        if !abs.join("dbt_project.yml").is_file() {
            return Err(ReviewError::ProjectNotFound {
                searched: abs,
                explicit: true,
            });
        }
        return Ok(abs);
    }
    let found = discover_project_dir(cwd)?;
    canonicalized(&found)
}

/// The project directory relative to the repo toplevel, as a
/// forward-slash string ("" when the project **is** the toplevel).
fn project_rel_to_toplevel(project_dir: &Path, toplevel: &Path) -> Result<String, ReviewError> {
    let rel = project_dir
        .strip_prefix(toplevel)
        .map_err(|_| ReviewError::ProjectOutsideRepo {
            project: project_dir.to_path_buf(),
            toplevel: toplevel.to_path_buf(),
        })?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// Gather every ladder fact with read-only git queries. One helper per
/// rung keeps each piece small + directly testable (the CRAP < 15 lane
/// rule): [`probe_explicit_ref`], [`probe_configured_base`],
/// [`probe_gh_pr_base`], [`probe_origin_head`], and [`probe_name_refs`].
fn gather_base_facts(toplevel: &Path, explicit: Option<&str>) -> Result<BaseFacts, ReviewError> {
    let (remote_probe, local_probe) = probe_name_refs(toplevel)?;
    let (pr_base, pr_info) = probe_gh_pr_base(toplevel)?;
    Ok(BaseFacts {
        explicit: probe_explicit_ref(toplevel, explicit)?,
        configured: probe_configured_base(toplevel)?,
        pr_base,
        pr_info,
        origin_head: probe_origin_head(toplevel)?,
        remote_probe,
        local_probe,
    })
}

/// The auto-ladder gh rung: the open PR's base, resolved to a verified
/// local ref, plus the full PR context (cute-dbt#346 — the banner link).
/// **Fail-soft** — `(None, None)` whenever `gh` is absent, HEAD is
/// detached, no PR resolves, the call times out, or the base ref is not
/// present locally. Only the local-ref resolution can surface a real
/// error (a git probe failure); everything `gh`-side degrades to `None`
/// so `gh` is never a hard dependency of the auto-ladder. The `PrInfo` is
/// returned even when the resolved base ref is absent locally — the PR
/// context is still valid for the banner link though the base rung falls
/// through.
fn probe_gh_pr_base(toplevel: &Path) -> Result<(Option<String>, Option<PrInfo>), ReviewError> {
    // Branch-only: `gh pr view` needs a branch, and the rung must never
    // run on a detached HEAD.
    if current_branch(toplevel)?.is_none() {
        return Ok((None, None));
    }
    match gh_pr_view(toplevel) {
        Some(pr) => {
            let base = resolve_pr_base_ref(toplevel, &pr.base_ref)?;
            Ok((base, Some(pr)))
        }
        None => Ok((None, None)),
    }
}

/// Whether `name^{commit}` resolves in the repo at `toplevel`.
fn ref_resolves(toplevel: &Path, name: &str) -> Result<bool, ReviewError> {
    let probe = git_query(
        toplevel,
        &["rev-parse", "-q", "--verify", &format!("{name}^{{commit}}")],
    )?;
    Ok(probe.status.success())
}

/// The currently checked-out branch short name, or `None` on a detached
/// HEAD (`git symbolic-ref -q --short HEAD`).
fn current_branch(toplevel: &Path) -> Result<Option<String>, ReviewError> {
    let out = git_query(toplevel, &["symbolic-ref", "-q", "--short", "HEAD"])?;
    let name = stdout_line(&out);
    Ok((out.status.success() && !name.is_empty()).then_some(name))
}

/// Run `gh pr view` for the **current branch's** PR, returning the
/// parsed PR — or `None` on **any** failure (`gh` missing, non-zero
/// exit, unparseable JSON). Fail-soft by contract: this is the
/// auto-ladder rung, where `gh` is a convenience, never a dependency.
/// The explicit `--pr` path uses [`require_gh_pr`] instead, which
/// distinguishes the failure modes (and can target a specific number).
fn gh_pr_view(toplevel: &Path) -> Option<PrInfo> {
    let output = run_gh_pr_view(toplevel, None).ok()??;
    if !output.status.success() {
        return None;
    }
    parse_pr_info(&String::from_utf8_lossy(&output.stdout))
}

/// Spawn `gh pr view [<number>]` and collect its output. `Ok(None)`
/// means `gh` is not on PATH; `Ok(Some(output))` is a completed run
/// (success or not).
///
/// `number` selects the PR: `Some(n)` queries PR #n explicitly
/// (`gh pr view <n> …`); `None` queries the current branch's PR (the
/// auto-ladder rung). Threading the number is load-bearing — without
/// it `--pr <n>` would query the current branch's PR and silently
/// review the wrong PR's base (cute-dbt#303 bot review).
///
/// The hang bound is structural rather than a wall-clock timer:
/// `stdin(Stdio::null())` denies `gh` an interactive prompt, so it
/// fails fast (non-zero exit) when it cannot answer — auth, network, no
/// PR — instead of blocking on a TTY question. (An earlier explicit
/// thread-based timeout was removed: every variant of it flaked under
/// concurrent test fork pressure — scheduler races on the result
/// channel, or the watchdog's own `kill` spawn stalling — while adding
/// no real safety over the null-stdin guarantee.)
fn run_gh_pr_view(toplevel: &Path, number: Option<u64>) -> Result<Option<Output>, ReviewError> {
    let mut command = Command::new("gh");
    command.arg("pr").arg("view");
    if let Some(n) = number {
        command.arg(n.to_string());
    }
    let result = command
        .args(["--json", "baseRefName,headRefName,number,title,url"])
        .current_dir(toplevel)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output();
    match result {
        Ok(output) => Ok(Some(output)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ReviewError::StageFailed {
            context: "spawning gh",
            detail: err.to_string(),
        }),
    }
}

/// The explicit `--pr [<n>]` path's gh resolution: unlike the fail-soft
/// auto-rung, the operator asked for the PR, so failures are surfaced.
/// `number` is `Some(n)` for `--pr <n>` (query PR #n) or `None` for bare
/// `--pr` (the current branch's PR). `gh` missing ⇒
/// [`ReviewError::GhMissing`]; no usable PR ⇒
/// [`ReviewError::NoPullRequest`] (carrying the current branch); a
/// read failure surfaces as the underlying [`ReviewError`].
fn require_gh_pr(toplevel: &Path, number: Option<u64>) -> Result<PrInfo, ReviewError> {
    let branch = current_branch(toplevel)?;
    let Some(output) = run_gh_pr_view(toplevel, number)? else {
        return Err(ReviewError::GhMissing);
    };
    if !output.status.success() {
        return Err(ReviewError::NoPullRequest { branch });
    }
    parse_pr_info(&String::from_utf8_lossy(&output.stdout))
        .ok_or(ReviewError::NoPullRequest { branch })
}

/// Rung 0: the `--base` flag, verified (an unresolvable explicit base is
/// kept as a non-resolving probe so [`decide_base`] can error on it
/// rather than fall through).
fn probe_explicit_ref(
    toplevel: &Path,
    explicit: Option<&str>,
) -> Result<Option<RefProbe>, ReviewError> {
    match explicit {
        Some(name) => Ok(Some(RefProbe {
            name: name.to_owned(),
            resolves: ref_resolves(toplevel, name)?,
        })),
        None => Ok(None),
    }
}

/// Rung 1: the persisted `git config cute-dbt.base`, verified.
fn probe_configured_base(toplevel: &Path) -> Result<Option<RefProbe>, ReviewError> {
    let out = git_query(toplevel, &["config", "--get", "cute-dbt.base"])?;
    let name = stdout_line(&out);
    if out.status.success() && !name.is_empty() {
        let resolves = ref_resolves(toplevel, &name)?;
        Ok(Some(RefProbe { name, resolves }))
    } else {
        Ok(None)
    }
}

/// Rung 2: the `origin/HEAD` symref target — kept only when it both
/// exists and resolves (a stale symref falls through to the probes).
fn probe_origin_head(toplevel: &Path) -> Result<Option<String>, ReviewError> {
    let out = git_query(
        toplevel,
        &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
    )?;
    let name = stdout_line(&out);
    if out.status.success() && !name.is_empty() && ref_resolves(toplevel, &name)? {
        Ok(Some(name))
    } else {
        Ok(None)
    }
}

/// Rung 3: probe `main`/`master`/`trunk` as remote-tracking refs and as
/// local heads, first hit each. Remote-tracking first is the jj
/// `trunk()` order (a local `main` may be months stale).
fn probe_name_refs(toplevel: &Path) -> Result<(Option<String>, Option<String>), ReviewError> {
    let mut remote_probe = None;
    let mut local_probe = None;
    for candidate in ["main", "master", "trunk"] {
        if remote_probe.is_none()
            && ref_exists(toplevel, &format!("refs/remotes/origin/{candidate}"))?
        {
            remote_probe = Some(format!("origin/{candidate}"));
        }
        if local_probe.is_none() && ref_exists(toplevel, &format!("refs/heads/{candidate}"))? {
            local_probe = Some(candidate.to_owned());
        }
    }
    Ok((remote_probe, local_probe))
}

/// Whether a fully-qualified ref exists (`show-ref --verify`).
fn ref_exists(toplevel: &Path, fqref: &str) -> Result<bool, ReviewError> {
    let out = git_query(toplevel, &["show-ref", "--verify", "-q", fqref])?;
    Ok(out.status.success())
}

/// `git merge-base HEAD <base>`, with the shallow-vs-disjoint diagnosis
/// on failure.
fn find_merge_base(toplevel: &Path, base: &str) -> Result<String, ReviewError> {
    let out = git_query(toplevel, &["merge-base", "HEAD", base])?;
    if out.status.success() {
        return Ok(stdout_line(&out));
    }
    let shallow = git_query(toplevel, &["rev-parse", "--is-shallow-repository"])?;
    Err(diagnose_no_merge_base(
        base,
        stdout_line(&shallow) == "true",
    ))
}

/// Print the `--dry-run` listing: the exact commands a real run
/// executes, rendered from the **same** plan values a real run would
/// execute.
fn print_dry_run(diff: &CommandPlan, compile: Option<&CommandPlan>, compose: &ComposeInputs) {
    print!("{}", dry_run_listing(diff, compile, compose));
}

/// Build the `--dry-run` listing (separated from printing so tests pin
/// the exact rows): the git diff plan, the dbt compile plan (when the
/// run would compile), and the equivalent `cute-dbt report` invocation
/// — every row rendered from the SAME plan values a real run executes.
fn dry_run_listing(
    diff: &CommandPlan,
    compile: Option<&CommandPlan>,
    compose: &ComposeInputs,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "cute-dbt review --dry-run: a real run executes, in order:"
    );
    let _ = writeln!(out, "  [git diff]        {}", diff.rendered());
    match compile {
        Some(plan) => {
            let _ = writeln!(out, "  [dbt compile]     {}", plan.rendered());
        }
        None => {
            let _ = writeln!(
                out,
                "  [dbt compile]     skipped (--no-compile): the existing manifest is trusted"
            );
        }
    }
    let _ = writeln!(
        out,
        "  [cute-dbt report] (cwd: {}) {}",
        diff.cwd.display(),
        compose
            .report_display_argv()
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let _ = writeln!(
        out,
        "nothing was executed and no file was written. A real run passes the diff \
         to report in-process ({DIFF_STAND_IN} stands for the first command's output)."
    );
    out
}

/// The compile stage: detect the engine from `dbt --version` output
/// shape, announce it, then run the user's own `dbt compile` with its
/// output streaming to the terminal verbatim. **The exit code is the
/// only success signal** — fusion writes `manifest.json` even on failed
/// compiles (dbt_lib.rs:996-1001 @ 9977b6cb), so artifact presence
/// proves nothing.
fn run_compile_stage(project_dir: &Path, plan: &CommandPlan) -> Result<(), ReviewError> {
    let engine = detect_engine(project_dir)?;
    eprintln!("cute-dbt: dbt engine: {engine}");
    eprintln!("cute-dbt: running `dbt compile` (the output below is your dbt's own)");
    let status = plan
        .execute_streaming()
        .map_err(|err| map_dbt_spawn_error(&err))?;
    if !status.success() {
        return Err(ReviewError::CompileFailed {
            status: status.code(),
        });
    }
    Ok(())
}

/// Run `dbt --version` and classify the engine.
fn detect_engine(project_dir: &Path) -> Result<DbtEngine, ReviewError> {
    let output = version_plan(project_dir)
        .execute()
        .map_err(|err| map_dbt_spawn_error(&err))?;
    if !output.status.success() {
        return Err(ReviewError::EngineUnsupported(EngineIssue::Unrecognized {
            first_line: first_line_of(&String::from_utf8_lossy(&output.stderr)),
        }));
    }
    parse_dbt_version(&String::from_utf8_lossy(&output.stdout))
        .map_err(ReviewError::EngineUnsupported)
}

/// Map a dbt spawn failure: a missing binary gets the install
/// remediation; anything else is a stage failure.
fn map_dbt_spawn_error(err: &io::Error) -> ReviewError {
    if err.kind() == io::ErrorKind::NotFound {
        ReviewError::DbtMissing
    } else {
        ReviewError::StageFailed {
            context: "spawning dbt",
            detail: err.to_string(),
        }
    }
}

/// The `--no-compile` staleness check: warn (never block) when any
/// project source file is newer than the manifest — the report would
/// not reflect the latest edits. File mtimes are the cheap,
/// engine-agnostic signal (the manifest's own `generated_at` would
/// need a parse before Stage-1 preflight).
fn warn_if_stale(manifest: &Path, project_dir: &Path) {
    let Ok(manifest_mtime) = fs::metadata(manifest).and_then(|m| m.modified()) else {
        return;
    };
    // Exclude the RESOLVED target dir (the manifest's parent) by path,
    // not just by the conventional name: with a custom --target-path /
    // DBT_TARGET_PATH, dbt's own artifacts (run_results.json is written
    // after manifest.json) would otherwise false-positive the warning.
    if let Some((newest_path, newest_mtime)) = newest_source_mtime(project_dir, manifest.parent())
        && newest_mtime > manifest_mtime
    {
        let shown = newest_path
            .strip_prefix(project_dir)
            .unwrap_or(&newest_path)
            .display();
        eprintln!(
            "cute-dbt: warning: --no-compile, but the manifest is older than {shown} — \
             the report may not reflect your latest edits (drop --no-compile to refresh)."
        );
    }
}

/// The newest-mtime file under the project, skipping build output (the
/// conventional `target/` name AND `exclude_dir` — the resolved custom
/// target dir — by path), VCS metadata (`.git`), installed packages
/// (`dbt_packages/`), and symlinks (never followed).
///
/// Decomposed into named single-purpose helpers (each directly
/// unit-tested) per the lane-wide CRAP < 15 rule: the walk loop here,
/// the per-entry classification in [`visit_source_entry`], the skip
/// list in [`is_skipped_source_dir`], and the max-fold in
/// [`fold_newest`].
fn newest_source_mtime(
    project_dir: &Path,
    exclude_dir: Option<&Path>,
) -> Option<(PathBuf, SystemTime)> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    let mut stack = vec![project_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in readable_dir_entries(&dir) {
            visit_source_entry(&entry, exclude_dir, &mut stack, &mut newest);
        }
    }
    newest
}

/// The readable entries of a directory — an unreadable directory walks
/// as empty (the staleness check is a warning, never worth failing on).
fn readable_dir_entries(dir: &Path) -> Vec<fs::DirEntry> {
    fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Classify one walk entry: symlinks are never followed, non-skipped
/// directories queue onto `stack` (skipped = the conventional names OR
/// the resolved `exclude_dir` by path), and plain files fold their
/// mtime into `newest`. Unreadable file types / metadata are silently
/// skipped (warning-grade signal).
fn visit_source_entry(
    entry: &fs::DirEntry,
    exclude_dir: Option<&Path>,
    stack: &mut Vec<PathBuf>,
    newest: &mut Option<(PathBuf, SystemTime)>,
) {
    let Ok(file_type) = entry.file_type() else {
        return;
    };
    if file_type.is_symlink() {
        return;
    }
    if file_type.is_dir() {
        let path = entry.path();
        if !is_skipped_source_dir(&entry.file_name()) && Some(path.as_path()) != exclude_dir {
            stack.push(path);
        }
        return;
    }
    if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
        fold_newest(newest, entry.path(), mtime);
    }
}

/// Whether a directory name is excluded from the staleness walk: build
/// output, VCS metadata, installed packages.
fn is_skipped_source_dir(name: &std::ffi::OsStr) -> bool {
    name == "target" || name == ".git" || name == "dbt_packages"
}

/// Fold one `(path, mtime)` candidate into the running newest — strictly
/// newer wins; an equal mtime keeps the incumbent.
fn fold_newest(newest: &mut Option<(PathBuf, SystemTime)>, path: PathBuf, mtime: SystemTime) {
    if newest.as_ref().is_none_or(|(_, t)| mtime > *t) {
        *newest = Some((path, mtime));
    }
}

/// Run the project `git status --porcelain` scan and return its stdout,
/// or `None` when git could not be spawned / exited non-zero (both
/// warnings built on it are advisory — a failed scan just stays silent).
fn project_status_porcelain(toplevel: &Path, project_rel: &str) -> Option<String> {
    let output = status_plan(toplevel, project_rel).execute().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Format a capped, comma-joined file list for a warning: at most five
/// names, then `, …` when more were elided.
fn format_capped_file_list(files: &[String]) -> String {
    let shown = files
        .iter()
        .take(5)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let suffix = if files.len() > 5 { ", …" } else { "" };
    format!("{shown}{suffix}")
}

/// Soft warning for untracked files under the project: invisible to
/// `git diff`, so a brand-new model would silently miss the report.
/// Never a failure — and never an index mutation (`git add -N` is the
/// operator's call).
fn warn_untracked(toplevel: &Path, project_rel: &str) {
    let Some(porcelain) = project_status_porcelain(toplevel, project_rel) else {
        return;
    };
    let untracked: Vec<String> = porcelain
        .lines()
        .filter_map(|line| line.strip_prefix("?? ").map(str::to_owned))
        .collect();
    if untracked.is_empty() {
        return;
    }
    eprintln!(
        "cute-dbt: warning: {} untracked file(s) under the project are invisible to the \
         diff: {} — track them with `git add -N <file>` to include them.",
        untracked.len(),
        format_capped_file_list(&untracked),
    );
}

/// Soft warning for the `--staged` same-revision drift: files that are
/// staged AND further edited unstaged. The diff is HEAD → index but the
/// manifest is compiled from the working tree, so those files' inline
/// diffs would mislead — the drift-guard degrades them gracefully.
/// Never a failure (exit stays 0).
fn warn_staged_drift(toplevel: &Path, project_rel: &str) {
    let Some(porcelain) = project_status_porcelain(toplevel, project_rel) else {
        return;
    };
    let drifted = staged_files_with_unstaged_edits(&porcelain);
    if drifted.is_empty() {
        return;
    }
    eprintln!(
        "cute-dbt: warning: {} file(s) are staged but also have unstaged edits: {} — \
         --staged diffs the index, but the manifest is compiled from the working tree, \
         so the inline diffs for these files degrade to the plain view (the report is \
         still correct about WHICH tests changed).",
        drifted.len(),
        format_capped_file_list(&drifted),
    );
}

/// Execute the diff plan and return the patch text (raw bytes captured,
/// decoded lossily — content lines may legitimately carry `\r`).
fn run_diff(plan: &CommandPlan) -> Result<String, ReviewError> {
    let output = plan.execute().map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            ReviewError::GitMissing
        } else {
            ReviewError::StageFailed {
                context: "spawning git diff",
                detail: err.to_string(),
            }
        }
    })?;
    if !output.status.success() {
        return Err(ReviewError::StageFailed {
            context: "producing the diff",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Auto-open the report on an interactive terminal (spawn-and-forget;
/// a failed opener is a warning, never a run failure).
fn maybe_open(path: &Path, no_open: bool) {
    if !should_open(no_open, io::stdout().is_terminal()) {
        return;
    }
    let (program, args) = opener_invocation(path);
    let spawned = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(err) = spawned {
        eprintln!("cute-dbt: warning: could not open the report ({program}: {err})");
    }
}

// ===================================================================
// Tests
// ===================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(stem: &str) -> PathBuf {
        let nonce = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("cute-dbt-review-{pid}-{nonce}-{stem}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    // ----- ladder decision table -----------------------------------

    fn probe(name: &str, resolves: bool) -> RefProbe {
        RefProbe {
            name: name.to_owned(),
            resolves,
        }
    }

    #[test]
    fn explicit_base_wins_over_every_other_rung() {
        let facts = BaseFacts {
            explicit: Some(probe("release-2.4", true)),
            configured: Some(probe("develop", true)),
            pr_base: Some("origin/dev".to_owned()),
            pr_info: None,
            origin_head: Some("origin/main".to_owned()),
            remote_probe: Some("origin/main".to_owned()),
            local_probe: Some("main".to_owned()),
        };
        let (base, rung) = decide_base(&facts).expect("explicit resolves");
        assert_eq!(base, "release-2.4");
        assert_eq!(rung, Rung::ExplicitFlag);
    }

    #[test]
    fn an_unresolvable_explicit_base_is_an_error_never_a_fall_through() {
        // The operator asked for something specific — silently using a
        // different base would be a wrong-report hazard.
        let facts = BaseFacts {
            explicit: Some(probe("nope", false)),
            configured: Some(probe("develop", true)),
            ..BaseFacts::default()
        };
        let err = decide_base(&facts).expect_err("an unresolvable --base errors");
        match err {
            ReviewError::BaseRefMissing { ref_name, source } => {
                assert_eq!(ref_name, "nope");
                assert_eq!(source, BaseSource::Flag);
            }
            other => panic!("expected BaseRefMissing, got {other:?}"),
        }
    }

    #[test]
    fn configured_base_answers_when_no_flag_is_given() {
        let facts = BaseFacts {
            configured: Some(probe("develop", true)),
            origin_head: Some("origin/main".to_owned()),
            ..BaseFacts::default()
        };
        let (base, rung) = decide_base(&facts).expect("config rung answers");
        assert_eq!(base, "develop");
        assert_eq!(rung, Rung::GitConfig);
    }

    #[test]
    fn an_unresolvable_configured_base_is_an_error_naming_the_config() {
        let facts = BaseFacts {
            configured: Some(probe("gone", false)),
            origin_head: Some("origin/main".to_owned()),
            ..BaseFacts::default()
        };
        let err = decide_base(&facts).expect_err("a stale persisted base errors");
        match err {
            ReviewError::BaseRefMissing { ref_name, source } => {
                assert_eq!(ref_name, "gone");
                assert_eq!(source, BaseSource::GitConfig);
            }
            other => panic!("expected BaseRefMissing, got {other:?}"),
        }
    }

    #[test]
    fn the_gh_pr_rung_outranks_origin_head_and_the_probes() {
        // Rung 2: after --base and persisted config, before origin/HEAD.
        let facts = BaseFacts {
            pr_base: Some("origin/release-3".to_owned()),
            origin_head: Some("origin/main".to_owned()),
            remote_probe: Some("origin/main".to_owned()),
            local_probe: Some("main".to_owned()),
            ..BaseFacts::default()
        };
        let (base, rung) = decide_base(&facts).expect("the gh rung answers");
        assert_eq!(base, "origin/release-3");
        assert_eq!(rung, Rung::GhPr);
    }

    #[test]
    fn configured_base_outranks_the_gh_rung() {
        // The persisted answer is rung 1 — it wins over the gh rung so a
        // user who set cute-dbt.base is never surprised by a PR base.
        let facts = BaseFacts {
            configured: Some(probe("develop", true)),
            pr_base: Some("origin/main".to_owned()),
            ..BaseFacts::default()
        };
        let (base, rung) = decide_base(&facts).expect("config wins");
        assert_eq!(base, "develop");
        assert_eq!(rung, Rung::GitConfig);
    }

    #[test]
    fn origin_head_outranks_the_probes() {
        let facts = BaseFacts {
            origin_head: Some("origin/develop".to_owned()),
            remote_probe: Some("origin/main".to_owned()),
            local_probe: Some("main".to_owned()),
            ..BaseFacts::default()
        };
        let (base, rung) = decide_base(&facts).expect("origin/HEAD answers");
        assert_eq!(base, "origin/develop");
        assert_eq!(rung, Rung::OriginHead);
    }

    #[test]
    fn remote_probe_outranks_the_local_probe() {
        // A local `main` may be months stale; remote-tracking first
        // (the jj trunk() order).
        let facts = BaseFacts {
            remote_probe: Some("origin/master".to_owned()),
            local_probe: Some("main".to_owned()),
            ..BaseFacts::default()
        };
        let (base, rung) = decide_base(&facts).expect("remote probe answers");
        assert_eq!(base, "origin/master");
        assert_eq!(rung, Rung::RemoteProbe);
    }

    #[test]
    fn local_probe_is_the_last_answering_rung() {
        let facts = BaseFacts {
            local_probe: Some("trunk".to_owned()),
            ..BaseFacts::default()
        };
        let (base, rung) = decide_base(&facts).expect("local probe answers");
        assert_eq!(base, "trunk");
        assert_eq!(rung, Rung::LocalProbe);
    }

    #[test]
    fn an_empty_ladder_is_base_undetectable_naming_the_flag() {
        let err = decide_base(&BaseFacts::default()).expect_err("nothing answers");
        assert!(matches!(err, ReviewError::BaseUndetectable));
        assert!(
            err.message().contains("--base"),
            "the remediation names --base: {}",
            err.message(),
        );
    }

    #[test]
    fn merge_base_failure_diagnoses_shallow_vs_disjoint() {
        let shallow = diagnose_no_merge_base("main", true);
        assert!(matches!(shallow, ReviewError::ShallowClone { .. }));
        assert!(
            shallow.message().contains("--unshallow"),
            "shallow remediation: {}",
            shallow.message(),
        );
        let disjoint = diagnose_no_merge_base("main", false);
        assert!(matches!(disjoint, ReviewError::DisjointHistories { .. }));
        assert!(
            disjoint.message().contains("--base"),
            "disjoint remediation: {}",
            disjoint.message(),
        );
    }

    // ----- gh PR parsing + --pr anchor (research §1) ----------------

    #[test]
    fn parse_pr_info_reads_the_three_fields() {
        let pr = parse_pr_info(r#"{"baseRefName":"main","headRefName":"feature/x","number":42}"#)
            .expect("a complete payload parses");
        assert_eq!(pr.base_ref, "main");
        assert_eq!(pr.head_ref, "feature/x");
        assert_eq!(pr.number, 42);
    }

    #[test]
    fn parse_pr_info_reads_the_title_and_url_for_the_banner_link() {
        // cute-dbt#346 — the same `gh pr view` call now also carries
        // `title` + `url` (one more --json field, no new gh call).
        let pr = parse_pr_info(
            r#"{"baseRefName":"main","headRefName":"f","number":7,"title":"Add churn","url":"https://github.com/o/r/pull/7"}"#,
        )
        .expect("a payload with title + url parses");
        assert_eq!(pr.number, 7);
        assert_eq!(pr.title, "Add churn");
        assert_eq!(pr.url, "https://github.com/o/r/pull/7");
    }

    #[test]
    fn parse_pr_info_defaults_title_and_url_to_blank_when_absent() {
        // The pre-#346 payload shape (no title/url) still parses; the
        // banner then renders link-free (both fields required downstream).
        let pr = parse_pr_info(r#"{"baseRefName":"main","headRefName":"f","number":7}"#)
            .expect("a title-less payload still parses");
        assert_eq!(pr.title, "");
        assert_eq!(pr.url, "");
    }

    #[test]
    fn parse_pr_info_rejects_unusable_shapes() {
        // No base ref ⇒ unusable; blank base ref ⇒ unusable; non-object
        // / non-JSON ⇒ None. Each is "no usable PR", never a panic.
        assert!(parse_pr_info(r#"{"headRefName":"x","number":1}"#).is_none());
        assert!(parse_pr_info(r#"{"baseRefName":"  ","number":1}"#).is_none());
        assert!(parse_pr_info("not json at all").is_none());
        assert!(parse_pr_info("[]").is_none());
        assert!(parse_pr_info("").is_none());
    }

    #[test]
    fn parse_pr_info_tolerates_a_missing_head_or_number() {
        // gh always sends the requested fields, but be defensive: a
        // present base ref is enough to anchor (head defaults blank,
        // number to 0).
        let pr = parse_pr_info(r#"{"baseRefName":"release-2"}"#).expect("base alone is usable");
        assert_eq!(pr.base_ref, "release-2");
        assert_eq!(pr.head_ref, "");
        assert_eq!(pr.number, 0);
    }

    fn pr(base: &str, head: &str, number: u64) -> PrInfo {
        PrInfo {
            base_ref: base.to_owned(),
            head_ref: head.to_owned(),
            number,
            title: String::new(),
            url: String::new(),
        }
    }

    #[test]
    fn bare_pr_imposes_no_head_assertion() {
        // `--pr` with no number: any checked-out branch is fine.
        let info = pr("main", "feature/x", 42);
        assert!(check_pr_head(None, &info, Some("a-totally-different-branch")).is_ok());
        assert!(
            check_pr_head(None, &info, None).is_ok(),
            "even detached HEAD"
        );
    }

    #[test]
    fn pr_with_a_number_passes_when_head_matches() {
        let info = pr("main", "feature/x", 42);
        assert!(check_pr_head(Some(42), &info, Some("feature/x")).is_ok());
    }

    #[test]
    fn pr_with_a_number_errors_on_a_head_mismatch_naming_checkout() {
        // The never-mutate contract: the remediation tells the operator
        // to check out themselves — review does not.
        let info = pr("main", "feature/x", 42);
        let err = check_pr_head(Some(42), &info, Some("some-other-branch"))
            .expect_err("a head mismatch errors");
        match &err {
            ReviewError::PrHeadMismatch {
                number,
                head_ref,
                current,
            } => {
                assert_eq!(*number, 42);
                assert_eq!(head_ref, "feature/x");
                assert_eq!(current, "some-other-branch");
            }
            other => panic!("expected PrHeadMismatch, got {other:?}"),
        }
        let msg = err.message();
        assert!(
            msg.contains("gh pr checkout 42") && msg.contains("never checks out"),
            "the remediation names checkout + the never-mutate contract: {msg}",
        );
    }

    #[test]
    fn pr_with_a_number_on_detached_head_is_a_mismatch() {
        let info = pr("main", "feature/x", 7);
        let err =
            check_pr_head(Some(7), &info, None).expect_err("detached HEAD is not the PR head");
        assert!(
            err.message().contains("detached HEAD"),
            "the message names the detached state: {}",
            err.message(),
        );
    }

    // ----- remediation messages -------------------------------------

    /// `(error, a substring its message must contain)` — every
    /// `ReviewError` variant, exercised by
    /// `every_review_error_message_carries_a_remediation`. Split out of
    /// the test body so the table can grow past the line-count lint.
    fn remediation_samples() -> Vec<(ReviewError, &'static str)> {
        vec![
            (ReviewError::GitMissing, "PATH"),
            (ReviewError::NoGitRepo, "--pr-diff"),
            (ReviewError::BareRepo, "working tree"),
            (ReviewError::NoCommits, "Commit"),
            (ReviewError::BaseUndetectable, "--base"),
            (
                ReviewError::BaseRefMissing {
                    ref_name: "rel".to_owned(),
                    source: BaseSource::Flag,
                },
                "git fetch origin rel",
            ),
            // (the PullRequest BaseRefMissing variant is covered in full
            // by `a_pr_path_missing_base_is_not_attributed_to_the_base_flag`)
            (
                ReviewError::ShallowClone {
                    base: "main".to_owned(),
                },
                "--unshallow",
            ),
            (
                ReviewError::DisjointHistories {
                    base: "main".to_owned(),
                },
                "--base",
            ),
            (ReviewError::GhMissing, "cli.github.com"),
            (
                ReviewError::NoPullRequest {
                    branch: Some("feature/x".to_owned()),
                },
                "gh pr create",
            ),
            (
                ReviewError::PrHeadMismatch {
                    number: 9,
                    head_ref: "feature/y".to_owned(),
                    current: "main".to_owned(),
                },
                "gh pr checkout 9",
            ),
            (
                ReviewError::ProjectNotFound {
                    searched: PathBuf::from("/tmp/x"),
                    explicit: false,
                },
                "--project-dir",
            ),
            (
                ReviewError::ProjectAmbiguous {
                    candidates: vec![PathBuf::from("a"), PathBuf::from("b")],
                },
                "--project-dir",
            ),
            (
                ReviewError::ProjectOutsideRepo {
                    project: PathBuf::from("/p"),
                    toplevel: PathBuf::from("/t"),
                },
                "inside the repository",
            ),
            (
                ReviewError::ManifestMissing {
                    path: PathBuf::from("/p/target/manifest.json"),
                    after_compile: false,
                },
                "dbt compile",
            ),
            (
                ReviewError::ManifestMissing {
                    path: PathBuf::from("/p/target/manifest.json"),
                    after_compile: true,
                },
                "--target-path",
            ),
            (ReviewError::DbtMissing, "--no-compile"),
            (
                ReviewError::EngineUnsupported(EngineIssue::CloudCli),
                "dbt Cloud CLI",
            ),
            (
                ReviewError::EngineUnsupported(EngineIssue::CoreTooOld {
                    version: "1.7.6".to_owned(),
                }),
                "1.8",
            ),
            (
                ReviewError::EngineUnsupported(EngineIssue::Unrecognized {
                    first_line: "weird".to_owned(),
                }),
                "--no-compile",
            ),
            (ReviewError::CompileFailed { status: Some(2) }, "dbt debug"),
            (
                ReviewError::StageFailed {
                    context: "producing the diff",
                    detail: "boom".to_owned(),
                },
                "--dry-run",
            ),
        ]
    }

    #[test]
    fn every_review_error_message_carries_a_remediation() {
        for (err, needle) in remediation_samples() {
            let msg = err.message();
            assert!(
                msg.contains(needle),
                "{err:?} message must carry {needle:?}: {msg}"
            );
            assert!(
                msg.starts_with("cute-dbt review:"),
                "messages identify the verb: {msg}"
            );
        }
    }

    #[test]
    fn a_pr_path_missing_base_is_not_attributed_to_the_base_flag() {
        // cute-dbt#303 bot review: on the `--pr` path a base ref that is
        // missing locally came from the PR, not `--base` — the message
        // must say so (never "(from --base)").
        let err = ReviewError::BaseRefMissing {
            ref_name: "release-2".to_owned(),
            source: BaseSource::PullRequest,
        };
        let msg = err.message();
        assert!(
            msg.contains("the open PR's base branch") && msg.contains("release-2"),
            "the message attributes the ref to the PR: {msg}",
        );
        assert!(
            !msg.contains("(from --base)"),
            "the message must NOT claim the ref came from --base: {msg}",
        );
        assert!(
            msg.contains("git fetch origin release-2"),
            "the remediation suggests fetching the PR's base: {msg}",
        );
    }

    #[test]
    fn ambiguous_project_message_lists_every_candidate() {
        let err = ReviewError::ProjectAmbiguous {
            candidates: vec![PathBuf::from("analytics"), PathBuf::from("warehouse")],
        };
        let msg = err.message();
        assert!(msg.contains("analytics"), "{msg}");
        assert!(msg.contains("warehouse"), "{msg}");
    }

    #[test]
    fn explicit_vs_discovered_project_not_found_word_differently() {
        let explicit = ReviewError::ProjectNotFound {
            searched: PathBuf::from("/x"),
            explicit: true,
        };
        assert!(
            explicit.message().contains("--project-dir)"),
            "{}",
            explicit.message()
        );
        let discovered = ReviewError::ProjectNotFound {
            searched: PathBuf::from("/x"),
            explicit: false,
        };
        assert!(
            discovered.message().contains("one level down"),
            "{}",
            discovered.message()
        );
    }

    // ----- diff plan (research-294 §3, verbatim) ---------------------

    #[test]
    fn diff_plan_carries_the_config_proof_flag_set_verbatim() {
        let plan = diff_plan(Path::new("/repo"), "proj", "abc123", DiffScope::WorkingTree);
        assert_eq!(plan.program, "git");
        assert_eq!(
            plan.args,
            vec![
                "-c",
                "diff.noprefix=false",
                "-c",
                "diff.mnemonicPrefix=false",
                "--no-pager",
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-relative",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--unified=0",
                "--find-renames",
                "--submodule=short",
                "abc123",
                "--",
                "proj/",
                ":(exclude)proj/target/",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        );
        assert_eq!(plan.cwd, PathBuf::from("/repo"));
        assert_eq!(plan.env, vec![("LC_ALL", "C")]);
    }

    #[test]
    fn committed_only_diffs_merge_base_to_head() {
        let plan = diff_plan(
            Path::new("/repo"),
            "proj",
            "abc123",
            DiffScope::CommittedOnly,
        );
        assert!(
            plan.args.contains(&"abc123..HEAD".to_owned()),
            "committed-only uses the two-endpoint form: {:?}",
            plan.args,
        );
        assert!(
            !plan.args.contains(&"abc123".to_owned()),
            "no bare single-rev arg remains: {:?}",
            plan.args,
        );
    }

    #[test]
    fn staged_diffs_the_index_with_cached_and_no_merge_base() {
        let plan = diff_plan(Path::new("/repo"), "proj", "abc123", DiffScope::Staged);
        assert!(
            plan.args.contains(&"--cached".to_owned()),
            "--staged adds --cached (HEAD -> index): {:?}",
            plan.args,
        );
        assert!(
            !plan.args.iter().any(|a| a.contains("abc123")),
            "the staged form carries no merge-base: {:?}",
            plan.args,
        );
        // The config-proof flags survive on the variant.
        assert!(plan.args.contains(&"--unified=0".to_owned()));
        assert!(plan.args.contains(&"--find-renames".to_owned()));
    }

    #[test]
    fn unstaged_is_a_bare_diff_index_to_working_tree() {
        let plan = diff_plan(Path::new("/repo"), "proj", "abc123", DiffScope::Unstaged);
        assert!(
            !plan.args.contains(&"--cached".to_owned()),
            "--unstaged is NOT --cached: {:?}",
            plan.args,
        );
        assert!(
            !plan.args.iter().any(|a| a.contains("abc123")),
            "the unstaged form carries no merge-base: {:?}",
            plan.args,
        );
        // The selector args slot is empty, so `--` immediately follows
        // the last config-proof flag (`--submodule=short`).
        let dashdash = plan
            .args
            .iter()
            .position(|a| a == "--")
            .expect("-- present");
        assert_eq!(
            plan.args[dashdash - 1],
            "--submodule=short",
            "no rev/selector arg precedes the pathspec: {:?}",
            plan.args,
        );
        assert!(plan.args.contains(&"--unified=0".to_owned()));
    }

    #[test]
    fn scope_from_flags_maps_each_selector() {
        assert_eq!(
            DiffScope::from_flags(false, false, false),
            DiffScope::WorkingTree,
            "no flag is the working-tree default",
        );
        assert_eq!(
            DiffScope::from_flags(true, false, false),
            DiffScope::CommittedOnly,
        );
        assert_eq!(DiffScope::from_flags(false, true, false), DiffScope::Staged);
        assert_eq!(
            DiffScope::from_flags(false, false, true),
            DiffScope::Unstaged,
        );
    }

    #[test]
    fn only_staged_diffs_the_index() {
        // The drift check runs ONLY on --staged (the one scope whose
        // diff endpoint disagrees with the compiled working tree).
        assert!(DiffScope::Staged.diffs_the_index());
        assert!(!DiffScope::Unstaged.diffs_the_index());
        assert!(!DiffScope::WorkingTree.diffs_the_index());
        assert!(!DiffScope::CommittedOnly.diffs_the_index());
    }

    // ----- staged same-revision drift parser ------------------------

    #[test]
    fn staged_drift_flags_only_files_with_both_index_and_worktree_changes() {
        // Porcelain v1: column 1 = index, column 2 = worktree.
        let porcelain = "\
MM models/staging/stg_customers.sql
M  models/orders.sql
 M models/customers.sql
A  models/new.sql
?? models/untracked.sql
AM models/added_then_edited.sql
";
        let drifted = staged_files_with_unstaged_edits(porcelain);
        assert_eq!(
            drifted,
            vec![
                "models/staging/stg_customers.sql".to_owned(),
                "models/added_then_edited.sql".to_owned(),
            ],
            "only the both-columns-dirty files drift (MM, AM); \
             staged-only (M␠/A␠), unstaged-only (␠M), and untracked (??) do not",
        );
    }

    #[test]
    fn staged_drift_takes_the_post_rename_path() {
        // A staged rename further edited unstaged: `RM orig -> new`.
        let drifted = staged_files_with_unstaged_edits("RM models/old.sql -> models/new.sql\n");
        assert_eq!(drifted, vec!["models/new.sql".to_owned()]);
    }

    #[test]
    fn staged_drift_is_empty_on_a_clean_or_staged_only_tree() {
        assert!(staged_files_with_unstaged_edits("").is_empty());
        assert!(
            staged_files_with_unstaged_edits("M  models/a.sql\nA  models/b.sql\n").is_empty(),
            "staged-only changes are not drift",
        );
        assert!(
            staged_files_with_unstaged_edits(" M models/c.sql\n?? d.sql\n").is_empty(),
            "unstaged-only and untracked are not drift",
        );
        // A too-short malformed line is ignored, never panics.
        assert!(staged_files_with_unstaged_edits("M\n").is_empty());
    }

    #[test]
    fn capped_file_list_truncates_after_five() {
        let many: Vec<String> = (0..7).map(|i| format!("f{i}.sql")).collect();
        let listed = format_capped_file_list(&many);
        assert!(listed.contains("f0.sql") && listed.contains("f4.sql"));
        assert!(listed.ends_with(", …"), "elision marker present: {listed}");
        assert!(!listed.contains("f5.sql"), "the sixth is elided: {listed}");
        let few = vec!["only.sql".to_owned()];
        assert_eq!(format_capped_file_list(&few), "only.sql");
    }

    #[test]
    fn a_project_at_the_toplevel_scopes_to_dot_and_excludes_target() {
        let plan = diff_plan(Path::new("/repo"), "", "abc123", DiffScope::WorkingTree);
        let tail: Vec<&str> = plan
            .args
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(String::as_str)
            .collect();
        assert_eq!(tail, vec!["--", ".", ":(exclude)target/"]);
    }

    #[test]
    fn status_plan_scans_the_same_pathspec_as_the_diff() {
        let plan = status_plan(Path::new("/repo"), "proj");
        assert_eq!(
            plan.args,
            vec![
                "status",
                "--porcelain",
                "--",
                "proj/",
                ":(exclude)proj/target/",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        );
    }

    // ----- the plan/execute seam -------------------------------------

    #[test]
    fn to_command_maps_the_plan_field_for_field() {
        // The structural executed-argv == planned-argv pin: execution
        // and the --dry-run listing both read this one plan value, and
        // this test proves the spawn mapping is faithful.
        let plan = diff_plan(Path::new("/repo"), "proj", "abc123", DiffScope::WorkingTree);
        let cmd = plan.to_command();
        assert_eq!(cmd.get_program(), "git");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, plan.args);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/repo")));
        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect();
        assert_eq!(envs, vec![("LC_ALL".to_owned(), "C".to_owned())]);
    }

    #[test]
    fn rendered_plan_quotes_shell_magic_and_names_cwd_and_env() {
        let plan = diff_plan(Path::new("/repo"), "proj", "abc123", DiffScope::WorkingTree);
        let line = plan.rendered();
        assert!(line.starts_with("(cwd: /repo) LC_ALL=C git "), "{line}");
        assert!(
            line.contains("':(exclude)proj/target/'"),
            "the pathspec magic is quoted for display: {line}",
        );
        assert!(line.contains("--unified=0"), "{line}");
    }

    // ----- compose inputs (display ≡ composed args) ------------------

    fn sample_compose() -> ComposeInputs {
        ComposeInputs {
            manifest: PathBuf::from("/repo/proj/target/manifest.json"),
            project_root: PathBuf::from("proj"),
            out: PathBuf::from("/repo/proj/target/cute-dbt-report.html"),
            config: None,
            pr: None,
        }
    }

    #[test]
    fn report_plan_matches_composed_args() {
        // The no-drift pin: the displayed `cute-dbt report` invocation
        // and the in-process ReportArgs are built from the SAME
        // ComposeInputs — every path the display names must be the path
        // the composed run receives.
        let compose = sample_compose();
        let argv = compose.report_display_argv();
        let composed = compose.report_args(
            parse_diff("").expect("an empty diff parses to zero scope"),
            None,
        );
        let displayed_manifest = argv
            .iter()
            .position(|a| a == "--manifest")
            .map(|i| &argv[i + 1])
            .expect("--manifest displayed");
        assert_eq!(Path::new(displayed_manifest), composed.manifest);
        let displayed_out = argv
            .iter()
            .position(|a| a == "--out")
            .map(|i| &argv[i + 1])
            .expect("--out displayed");
        assert_eq!(Path::new(displayed_out), composed.out);
        let displayed_root = argv
            .iter()
            .position(|a| a == "--project-root")
            .map(|i| &argv[i + 1])
            .expect("--project-root displayed");
        assert_eq!(
            Some(Path::new(displayed_root)),
            composed.project_root.as_deref()
        );
        // Review never selects the baseline arm and never widens
        // selectors — by construction.
        assert!(composed.baseline_manifest.is_none());
        assert!(composed.modified_selectors.is_empty());
        assert!(composed.pr_diff.is_some());
    }

    #[test]
    fn a_config_pass_through_is_displayed_and_composed() {
        let dir = unique_temp_dir("config");
        let path = dir.join("cfg.toml");
        fs::write(&path, "[report]\ntitle = \"T\"\n").expect("write config");
        let compose = ComposeInputs {
            config: Some(
                parse_review_config(path.to_str().expect("utf-8")).expect("config parses"),
            ),
            ..sample_compose()
        };
        let argv = compose.report_display_argv();
        let displayed = argv
            .iter()
            .position(|a| a == "--config")
            .map(|i| &argv[i + 1])
            .expect("--config displayed");
        assert_eq!(Path::new(displayed), path);
        let composed = compose.report_args(parse_diff("").expect("empty diff"), None);
        assert_eq!(
            composed
                .config
                .expect("config composed")
                .report
                .title
                .as_deref(),
            Some("T"),
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_resolved_pr_threads_the_banner_link_fields() {
        // cute-dbt#346 — a resolved open PR populates the composed report's
        // `--pr-*` fields (number + title + url), so the change-context
        // banner links to the PR on every review run that has one.
        let compose = ComposeInputs {
            pr: Some(PrInfo {
                base_ref: "main".to_owned(),
                head_ref: "feature/x".to_owned(),
                number: 314,
                title: "Refine payer dims".to_owned(),
                url: "https://github.com/acme/shop/pull/314".to_owned(),
            }),
            ..sample_compose()
        };
        let composed = compose.report_args(parse_diff("").expect("empty diff"), None);
        assert_eq!(composed.pr_number, Some(314));
        assert_eq!(composed.pr_title.as_deref(), Some("Refine payer dims"));
        assert_eq!(
            composed.pr_url.as_deref(),
            Some("https://github.com/acme/shop/pull/314"),
        );
    }

    #[test]
    fn no_resolved_pr_leaves_the_banner_link_fields_unset() {
        // Graceful degradation: a review run with no resolvable PR composes
        // a report with no `--pr-*` fields ⇒ a link-free banner.
        let composed = sample_compose().report_args(parse_diff("").expect("empty diff"), None);
        assert!(composed.pr_number.is_none());
        assert!(composed.pr_title.is_none());
        assert!(composed.pr_url.is_none());
    }

    // ----- manifest / out resolution ---------------------------------

    #[test]
    fn target_dir_defaults_to_project_target() {
        assert_eq!(
            resolve_target_dir(Path::new("/p"), None, None),
            PathBuf::from("/p/target"),
        );
    }

    #[test]
    fn dbt_target_path_relative_joins_the_project_dir() {
        // fusion's in_out_dir semantics: relative → joined to project.
        assert_eq!(
            resolve_target_dir(Path::new("/p"), None, Some("build")),
            PathBuf::from("/p/build"),
        );
        assert_eq!(
            resolve_target_dir(Path::new("/p"), None, Some("/abs/out")),
            PathBuf::from("/abs/out"),
        );
        assert_eq!(
            resolve_target_dir(Path::new("/p"), None, Some("  ")),
            PathBuf::from("/p/target"),
            "a blank value falls back to the default",
        );
    }

    #[test]
    fn the_target_path_flag_wins_over_the_env() {
        assert_eq!(
            resolve_target_dir(Path::new("/p"), Some(Path::new("flagged")), Some("build")),
            PathBuf::from("/p/flagged"),
            "the --target-path flag outranks DBT_TARGET_PATH",
        );
        assert_eq!(
            resolve_target_dir(Path::new("/p"), Some(Path::new("/abs/flag")), Some("build")),
            PathBuf::from("/abs/flag"),
        );
    }

    #[test]
    fn out_path_defaults_into_target_and_absolutizes_against_the_users_cwd() {
        assert_eq!(
            resolve_out_path(None, Path::new("/anywhere"), Path::new("/p/target")),
            PathBuf::from("/p/target/cute-dbt-report.html"),
        );
        // Relative --out resolves against the operator's cwd — never
        // the repo toplevel the run loop later moves to.
        assert_eq!(
            resolve_out_path(
                Some(Path::new("r.html")),
                Path::new("/work/sub"),
                Path::new("/p/target"),
            ),
            PathBuf::from("/work/sub/r.html"),
        );
        assert_eq!(
            resolve_out_path(
                Some(Path::new("/abs/r.html")),
                Path::new("/work"),
                Path::new("/p/target"),
            ),
            PathBuf::from("/abs/r.html"),
        );
    }

    // ----- engine detection (research-294 dbt-engine-mechanics §1) ----

    #[test]
    fn a_single_line_banner_is_the_fusion_engine_under_any_brand() {
        // The product binary, the old beta brand, AND the OSS
        // standalone (which brands itself dbt-core!) all emit the
        // single-line clap template — the SHAPE is the discriminator.
        for (raw, version) in [
            ("dbt 2.0.0-preview.186\n", "2.0.0-preview.186"),
            ("dbt-fusion 2.0.0-beta.51\n", "2.0.0-beta.51"),
            ("dbt-core 2.0.0-preview.186\n", "2.0.0-preview.186"),
        ] {
            assert_eq!(
                parse_dbt_version(raw),
                Ok(DbtEngine::Fusion {
                    version: version.to_owned()
                }),
                "single-line {raw:?} is fusion",
            );
        }
    }

    #[test]
    fn the_multi_line_core_block_is_python_core_with_its_installed_version() {
        let raw = "Core:\n  - installed: 1.10.2\n  - latest:    1.10.2 - Up to date!\n\n\
                   Plugins:\n  - duckdb: 1.9.1 - Up to date!\n";
        assert_eq!(
            parse_dbt_version(raw),
            Ok(DbtEngine::Core {
                version: "1.10.2".to_owned()
            }),
        );
    }

    #[test]
    fn a_pre_1_8_core_fails_the_floor() {
        let raw = "Core:\n  - installed: 1.7.6\n";
        assert_eq!(
            parse_dbt_version(raw),
            Err(EngineIssue::CoreTooOld {
                version: "1.7.6".to_owned()
            }),
        );
        // 1.8 exactly meets the floor; 2.x core (hypothetical) passes.
        assert!(parse_dbt_version("Core:\n  - installed: 1.8.0\n").is_ok());
        assert!(parse_dbt_version("Core:\n  - installed: 2.1.0\n").is_ok());
    }

    #[test]
    fn the_cloud_cli_banner_is_rejected_before_anything_else() {
        let raw = "Cloud CLI - 0.38.0 (abc1234 2026-05-01T00:00:00Z)\n";
        assert_eq!(parse_dbt_version(raw), Err(EngineIssue::CloudCli));
    }

    #[test]
    fn unrecognizable_version_output_is_an_honest_error() {
        for raw in ["", "\n", "not a dbt at all\n", "dbt\n"] {
            assert!(
                matches!(
                    parse_dbt_version(raw),
                    Err(EngineIssue::Unrecognized { .. })
                ),
                "{raw:?} must be Unrecognized",
            );
        }
    }

    #[test]
    fn engine_display_names_the_engine_and_version() {
        assert_eq!(
            DbtEngine::Fusion {
                version: "2.0.0".to_owned()
            }
            .to_string(),
            "fusion 2.0.0",
        );
        assert_eq!(
            DbtEngine::Core {
                version: "1.10.2".to_owned()
            }
            .to_string(),
            "dbt-core 1.10.2",
        );
    }

    // ----- dbt plans ----------------------------------------------------

    #[test]
    fn compile_plan_is_the_users_own_invocation() {
        // Bare `dbt compile`, cwd = the project dir, NO extra flags and
        // NO injected env — the privacy posture is documented, never
        // silently applied to the user's dbt.
        let plan = compile_plan(Path::new("/p"), None);
        assert_eq!(plan.program, "dbt");
        assert_eq!(plan.args, vec!["compile".to_owned()]);
        assert_eq!(plan.cwd, PathBuf::from("/p"));
        assert!(plan.env.is_empty(), "no env is injected into dbt");
    }

    #[test]
    fn compile_plan_forwards_the_target_path_flag() {
        let plan = compile_plan(Path::new("/p"), Some(Path::new("build")));
        assert_eq!(
            plan.args,
            vec![
                "compile".to_owned(),
                "--target-path".to_owned(),
                "build".to_owned()
            ],
            "dbt writes where review reads",
        );
    }

    #[test]
    fn version_plan_asks_dbt_for_its_version() {
        let plan = version_plan(Path::new("/p"));
        assert_eq!(plan.program, "dbt");
        assert_eq!(plan.args, vec!["--version".to_owned()]);
        assert_eq!(plan.cwd, PathBuf::from("/p"));
    }

    // ----- dry-run listing ----------------------------------------------

    #[test]
    fn dry_run_listing_carries_all_three_rows_in_execution_order() {
        let diff = diff_plan(Path::new("/repo"), "proj", "abc123", DiffScope::WorkingTree);
        let compile = compile_plan(Path::new("/repo/proj"), None);
        let listing = dry_run_listing(&diff, Some(&compile), &sample_compose());
        let diff_at = listing.find("[git diff]").expect("diff row");
        let compile_at = listing.find("[dbt compile]").expect("compile row");
        let report_at = listing.find("[cute-dbt report]").expect("report row");
        assert!(
            diff_at < compile_at && compile_at < report_at,
            "rows appear in real execution order: {listing}",
        );
        assert!(
            listing.contains("dbt compile"),
            "the compile plan argv is shown: {listing}",
        );
    }

    #[test]
    fn dry_run_listing_marks_the_skipped_compile_under_no_compile() {
        let diff = diff_plan(Path::new("/repo"), "proj", "abc123", DiffScope::WorkingTree);
        let listing = dry_run_listing(&diff, None, &sample_compose());
        assert!(
            listing.contains("skipped (--no-compile)"),
            "the skip is said out loud: {listing}",
        );
    }

    // ----- staleness (--no-compile) ---------------------------------------

    #[test]
    fn newest_source_mtime_skips_target_git_and_packages() {
        let dir = unique_temp_dir("stale-walk");
        fs::write(dir.join("dbt_project.yml"), "name: x\n").expect("write");
        fs::create_dir_all(dir.join("models")).expect("mkdir");
        fs::write(dir.join("models/a.sql"), "select 1\n").expect("write");
        for skipped in ["target", ".git", "dbt_packages"] {
            fs::create_dir_all(dir.join(skipped)).expect("mkdir");
            fs::write(dir.join(skipped).join("noise.txt"), "x\n").expect("write");
        }
        // Make the skipped files the newest on disk — they must still
        // never win.
        let future = SystemTime::now() + std::time::Duration::from_secs(3600);
        for skipped in ["target", ".git", "dbt_packages"] {
            let f = fs::File::options()
                .write(true)
                .open(dir.join(skipped).join("noise.txt"))
                .expect("open");
            f.set_modified(future).expect("set mtime");
        }
        let (path, _) = newest_source_mtime(&dir, None).expect("sources found");
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(
            name == "a.sql" || name == "dbt_project.yml",
            "build output / VCS / packages never win the walk: {}",
            path.display(),
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_skipped_source_dir_excludes_exactly_the_three_names() {
        for skipped in ["target", ".git", "dbt_packages"] {
            assert!(
                is_skipped_source_dir(std::ffi::OsStr::new(skipped)),
                "{skipped} is excluded",
            );
        }
        for walked in ["models", "tests", "seeds", "macros", "targets", "git"] {
            assert!(
                !is_skipped_source_dir(std::ffi::OsStr::new(walked)),
                "{walked} is walked",
            );
        }
    }

    #[test]
    fn fold_newest_keeps_the_strictly_newest_candidate() {
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(10);
        let mut newest = None;
        // First candidate always seeds.
        fold_newest(&mut newest, PathBuf::from("a"), t0);
        assert_eq!(newest, Some((PathBuf::from("a"), t0)));
        // Strictly newer replaces.
        fold_newest(&mut newest, PathBuf::from("b"), t1);
        assert_eq!(newest, Some((PathBuf::from("b"), t1)));
        // Older never replaces.
        fold_newest(&mut newest, PathBuf::from("c"), t0);
        assert_eq!(newest, Some((PathBuf::from("b"), t1)));
        // An EQUAL mtime keeps the incumbent (strictly-greater fold).
        fold_newest(&mut newest, PathBuf::from("d"), t1);
        assert_eq!(newest, Some((PathBuf::from("b"), t1)));
    }

    #[test]
    fn readable_dir_entries_treats_an_unreadable_dir_as_empty() {
        let missing = std::env::temp_dir().join("cute-dbt-definitely-not-a-dir");
        let _ = fs::remove_dir_all(&missing);
        assert!(
            readable_dir_entries(&missing).is_empty(),
            "a missing/unreadable dir walks as empty — warning-grade signal",
        );
        let dir = unique_temp_dir("readable-entries");
        fs::write(dir.join("one.sql"), "select 1\n").expect("write");
        assert_eq!(readable_dir_entries(&dir).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    // Unix-by-design: this test asserts the symlink-skip BEHAVIOR of
    // `visit_source_entry`, built on `std::os::unix::fs::symlink`. The
    // walk's symlink handling is exercised on Unix runners (the only
    // place creating a symlink is portable without elevated perms); no
    // tracking issue — a platform-specific test for a platform-specific
    // runtime path. The cross-platform walk logic is covered by the
    // non-symlink tests, which run everywhere.
    #[cfg(unix)]
    #[test]
    fn visit_source_entry_queues_dirs_folds_files_and_skips_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = unique_temp_dir("visit-entry");
        fs::write(dir.join("model.sql"), "select 1\n").expect("write");
        fs::create_dir_all(dir.join("models")).expect("mkdir");
        fs::create_dir_all(dir.join("target")).expect("mkdir");
        symlink(dir.join("model.sql"), dir.join("link.sql")).expect("create symlink");

        let mut stack: Vec<PathBuf> = Vec::new();
        let mut newest: Option<(PathBuf, SystemTime)> = None;
        for entry in readable_dir_entries(&dir) {
            visit_source_entry(&entry, None, &mut stack, &mut newest);
        }
        assert_eq!(
            stack,
            vec![dir.join("models")],
            "the walked dir queues; the skipped target/ does not",
        );
        let (path, _) = newest.expect("the plain file folded");
        assert_eq!(path, dir.join("model.sql"), "the symlink never folds");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_custom_target_dir_is_excluded_by_path_not_just_by_name() {
        // The gemini-flagged false positive on cute-dbt#312: with a
        // custom --target-path, dbt's own artifacts (run_results.json
        // lands after manifest.json) live in a dir NOT named `target` —
        // the resolved dir must be excluded by path equality.
        let dir = unique_temp_dir("custom-target");
        fs::write(dir.join("model.sql"), "select 1\n").expect("write");
        fs::create_dir_all(dir.join("build")).expect("mkdir");
        fs::write(dir.join("build/run_results.json"), "{}").expect("write");
        let future = SystemTime::now() + std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(dir.join("build/run_results.json"))
            .expect("open")
            .set_modified(future)
            .expect("set mtime");

        // WITHOUT the exclusion the artifact wins (the bug shape)…
        let (path, _) = newest_source_mtime(&dir, None).expect("found");
        assert_eq!(path, dir.join("build/run_results.json"));
        // …and WITH the resolved dir excluded, only real sources count.
        let (path, _) = newest_source_mtime(&dir, Some(&dir.join("build"))).expect("found");
        assert_eq!(path, dir.join("model.sql"));
        let _ = fs::remove_dir_all(&dir);
    }

    // Unix-by-design (see `visit_source_entry_..._skips_symlinks`): the
    // symlink-never-folds path is exercised on Unix runners; no tracking
    // issue.
    #[cfg(unix)]
    #[test]
    fn newest_source_mtime_ignores_a_symlink_even_when_it_is_newest() {
        use std::os::unix::fs::symlink;
        let dir = unique_temp_dir("stale-symlink");
        fs::write(dir.join("model.sql"), "select 1\n").expect("write");
        symlink(dir.join("model.sql"), dir.join("newer-link.sql")).expect("create symlink");
        let (path, _) = newest_source_mtime(&dir, None).expect("source found");
        assert_eq!(path, dir.join("model.sql"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn staleness_is_newer_source_strictly_after_manifest() {
        let dir = unique_temp_dir("stale-cmp");
        fs::create_dir_all(dir.join("target")).expect("mkdir");
        fs::write(dir.join("target/manifest.json"), "{}").expect("write");
        fs::write(dir.join("model.sql"), "select 1\n").expect("write");
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        // Manifest older than the source ⇒ stale.
        fs::File::options()
            .write(true)
            .open(dir.join("target/manifest.json"))
            .expect("open")
            .set_modified(past)
            .expect("set mtime");
        let manifest_mtime = fs::metadata(dir.join("target/manifest.json"))
            .and_then(|m| m.modified())
            .expect("mtime");
        let (_, newest) = newest_source_mtime(&dir, None).expect("source found");
        assert!(newest > manifest_mtime, "the fixture is genuinely stale");
        // Manifest newer than every source ⇒ fresh.
        let future = SystemTime::now() + std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(dir.join("target/manifest.json"))
            .expect("open")
            .set_modified(future)
            .expect("set mtime");
        let manifest_mtime = fs::metadata(dir.join("target/manifest.json"))
            .and_then(|m| m.modified())
            .expect("mtime");
        let (_, newest) = newest_source_mtime(&dir, None).expect("source found");
        assert!(newest <= manifest_mtime, "a fresh manifest is not stale");
        let _ = fs::remove_dir_all(&dir);
    }

    // ----- project discovery -----------------------------------------

    #[test]
    fn discovery_prefers_the_working_directory_itself() {
        let dir = unique_temp_dir("disc-cwd");
        fs::write(dir.join("dbt_project.yml"), "name: x\n").expect("write");
        // A subdirectory project must NOT shadow the cwd hit.
        fs::create_dir_all(dir.join("sub")).expect("mkdir");
        fs::write(dir.join("sub/dbt_project.yml"), "name: y\n").expect("write");
        let found = discover_project_dir(&dir).expect("cwd wins");
        assert_eq!(found, dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_finds_exactly_one_project_one_level_down() {
        let dir = unique_temp_dir("disc-one");
        fs::create_dir_all(dir.join("analytics")).expect("mkdir");
        fs::write(dir.join("analytics/dbt_project.yml"), "name: a\n").expect("write");
        fs::create_dir_all(dir.join("docs")).expect("mkdir");
        let found = discover_project_dir(&dir).expect("single candidate");
        assert_eq!(found, dir.join("analytics"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_with_zero_candidates_is_project_not_found() {
        let dir = unique_temp_dir("disc-zero");
        let err = discover_project_dir(&dir).expect_err("nothing to find");
        assert!(matches!(
            err,
            ReviewError::ProjectNotFound {
                explicit: false,
                ..
            }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_with_two_candidates_is_ambiguous_listing_both_sorted() {
        let dir = unique_temp_dir("disc-two");
        for name in ["beta", "alpha"] {
            fs::create_dir_all(dir.join(name)).expect("mkdir");
            fs::write(dir.join(name).join("dbt_project.yml"), "name: x\n").expect("write");
        }
        let err = discover_project_dir(&dir).expect_err("two candidates");
        match err {
            ReviewError::ProjectAmbiguous { candidates } => {
                assert_eq!(candidates, vec![dir.join("alpha"), dir.join("beta")]);
            }
            other => panic!("expected ProjectAmbiguous, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    // ----- open guard -------------------------------------------------

    #[test]
    fn should_open_requires_a_tty_and_no_opt_out() {
        assert!(should_open(false, true));
        assert!(!should_open(true, true), "--no-open always wins");
        assert!(!should_open(false, false), "non-TTY never opens");
        assert!(!should_open(true, false));
    }

    #[test]
    fn opener_invocation_names_the_report_path() {
        let (program, args) = opener_invocation(Path::new("/p/target/r.html"));
        assert!(
            ["open", "xdg-open", "cmd"].contains(&program),
            "a known platform opener: {program}",
        );
        assert!(
            args.iter().any(|a| a.contains("r.html")),
            "the path rides the argv: {args:?}",
        );
    }

    // ----- helpers -----------------------------------------------------

    #[test]
    fn short_sha_truncates_to_twelve() {
        assert_eq!(short_sha("0123456789abcdef0123"), "0123456789ab");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn rel_or_dot_spells_the_toplevel_project_as_dot() {
        assert_eq!(rel_or_dot(""), PathBuf::from("."));
        assert_eq!(rel_or_dot("analytics"), PathBuf::from("analytics"));
    }

    #[test]
    fn shell_quote_passes_safe_args_and_quotes_magic() {
        assert_eq!(shell_quote("--unified=0"), "--unified=0");
        assert_eq!(shell_quote(":(exclude)t/"), "':(exclude)t/'");
        assert_eq!(shell_quote("a b"), "'a b'");
    }

    // ----- clap surface -------------------------------------------------

    #[derive(Debug, Parser)]
    #[command(name = "cute-dbt")]
    struct TestCli {
        #[command(subcommand)]
        command: TestCommand,
    }

    #[derive(Debug, clap::Subcommand)]
    enum TestCommand {
        Review(ReviewArgs),
    }

    fn parse_review(args: &[&str]) -> Result<ReviewArgs, clap::Error> {
        TestCli::try_parse_from(args).map(|cli| match cli.command {
            TestCommand::Review(review) => review,
        })
    }

    #[test]
    fn review_parses_with_zero_flags() {
        let review = parse_review(&["cute-dbt", "review"]).expect("zero flags parse");
        assert!(review.base.is_none());
        assert!(!review.committed_only);
        assert!(!review.force);
        assert!(review.out.is_none());
        assert!(review.project_dir.is_none());
        assert!(!review.no_compile, "compile is the default");
        assert!(review.target_path.is_none());
        assert!(!review.no_open);
        assert!(review.config.is_none());
        assert!(!review.dry_run);
    }

    #[test]
    fn review_parses_no_compile_and_target_path() {
        let review = parse_review(&[
            "cute-dbt",
            "review",
            "--no-compile",
            "--target-path",
            "build",
        ])
        .expect("the V2 flags parse");
        assert!(review.no_compile);
        assert_eq!(review.target_path, Some(PathBuf::from("build")));
    }

    #[test]
    fn review_long_help_documents_the_privacy_switches() {
        // The honest privacy posture (issue #301 AC): the egress during
        // compile belongs to the user's own dbt; the help names dbt's
        // own suppression switches.
        use clap::CommandFactory;
        let mut cmd = super::super::args::Cli::command();
        let review = cmd
            .find_subcommand_mut("review")
            .expect("review is a listed verb");
        let help = review.render_long_help().to_string();
        assert!(
            help.contains("DBT_DISABLE_VERSION_CHECK=1"),
            "names the version-check switch: {help}",
        );
        assert!(
            help.contains("DBT_SEND_ANONYMOUS_USAGE_STATS=false"),
            "names the usage-stats switch: {help}",
        );
    }

    #[test]
    fn review_parses_the_full_v1_flag_set() {
        let dir = unique_temp_dir("clap-proj");
        let review = parse_review(&[
            "cute-dbt",
            "review",
            "--base",
            "origin/main",
            "--committed-only",
            "--force",
            "--out",
            "r.html",
            "--project-dir",
            dir.to_str().expect("utf-8"),
            "--no-open",
            "--dry-run",
        ])
        .expect("the full V1 flag set parses");
        assert_eq!(review.base.as_deref(), Some("origin/main"));
        assert!(review.committed_only);
        assert!(review.force);
        assert_eq!(review.out, Some(PathBuf::from("r.html")));
        assert_eq!(review.project_dir, Some(dir.clone()));
        assert!(review.no_open);
        assert!(review.dry_run);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_parses_the_staged_and_unstaged_scope_flags() {
        let staged = parse_review(&["cute-dbt", "review", "--staged"]).expect("--staged parses");
        assert!(staged.staged && !staged.unstaged && !staged.committed_only);
        let unstaged =
            parse_review(&["cute-dbt", "review", "--unstaged"]).expect("--unstaged parses");
        assert!(unstaged.unstaged && !unstaged.staged && !unstaged.committed_only);
    }

    #[test]
    fn the_scope_flags_are_mutually_exclusive() {
        // Every conflicting pair across the review_scope ArgGroup is a
        // clap usage error (exit 2), not a silent precedence.
        for pair in [
            ["--staged", "--unstaged"],
            ["--staged", "--committed-only"],
            ["--unstaged", "--committed-only"],
            ["--pr", "--staged"],
            ["--pr", "--committed-only"],
        ] {
            let err = parse_review(&["cute-dbt", "review", pair[0], pair[1]])
                .expect_err("conflicting scope flags are a usage error");
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{pair:?} must conflict",
            );
        }
    }

    #[test]
    fn pr_parses_bare_and_with_a_number() {
        // `Option<Option<u64>>`: absent / bare / numbered.
        let absent = parse_review(&["cute-dbt", "review"]).expect("no --pr parses");
        assert_eq!(absent.pr, None);
        let bare = parse_review(&["cute-dbt", "review", "--pr"]).expect("bare --pr parses");
        assert_eq!(bare.pr, Some(None), "bare --pr is Some(None)");
        let numbered = parse_review(&["cute-dbt", "review", "--pr", "42"]).expect("--pr 42 parses");
        assert_eq!(numbered.pr, Some(Some(42)));
    }

    #[test]
    fn pr_conflicts_with_an_explicit_base() {
        // Two base sources: --pr derives the base from the PR, --base
        // names it directly. Supplying both is a usage error.
        let err = parse_review(&["cute-dbt", "review", "--pr", "--base", "main"])
            .expect_err("--pr and --base conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn pr_rejects_a_non_numeric_value() {
        let err = parse_review(&["cute-dbt", "review", "--pr", "not-a-number"])
            .expect_err("--pr takes a number");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn review_rejects_an_unknown_flag() {
        let err =
            parse_review(&["cute-dbt", "review", "--frobnitz"]).expect_err("unknown flag rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn review_rejects_a_manifest_flag() {
        // review derives the manifest; --manifest is report plumbing.
        let err = parse_review(&["cute-dbt", "review", "--manifest", "m.json"])
            .expect_err("--manifest is not on the review surface");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn review_project_dir_must_exist() {
        let err = parse_review(&[
            "cute-dbt",
            "review",
            "--project-dir",
            "/definitely/not/a/dir",
        ])
        .expect_err("a missing --project-dir is a usage error");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn review_config_is_parsed_eagerly_like_report() {
        let dir = unique_temp_dir("clap-cfg");
        let bad = dir.join("broken.toml");
        fs::write(&bad, "not toml { =").expect("write");
        let err = parse_review(&[
            "cute-dbt",
            "review",
            "--config",
            bad.to_str().expect("utf-8"),
        ])
        .expect_err("a broken --config is a usage error");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_long_help_carries_worked_examples() {
        use clap::CommandFactory;
        let mut cmd = super::super::args::Cli::command();
        let review = cmd
            .find_subcommand_mut("review")
            .expect("review is a listed verb");
        let help = review.render_long_help().to_string();
        assert!(
            help.contains("Examples:"),
            "worked examples present: {help}"
        );
        assert!(
            help.contains("--dry-run"),
            "examples cover the transparency affordance: {help}",
        );
    }
}
