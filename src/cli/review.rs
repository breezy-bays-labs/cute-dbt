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
    group = ArgGroup::new("review_scope").multiple(false).args(["committed_only"]),
    after_long_help = REVIEW_EXAMPLES,
)]
pub struct ReviewArgs {
    /// Base ref to diff against, skipping detection (e.g. `main`,
    /// `origin/release-2.4`).
    ///
    /// Without it, review walks the detection ladder: `git config
    /// cute-dbt.base` → the `origin/HEAD` symref → probing
    /// `origin/{main,master,trunk}` then local heads. The answering
    /// rung is announced on stderr.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,

    /// Review only committed changes (`<merge-base>..HEAD`) — exact
    /// parity with what a PR would show.
    ///
    /// The default includes your staged + unstaged edits (the
    /// working-tree endpoint), because the manifest is compiled from
    /// the working tree — the same-revision contract that keeps the
    /// inline diffs sound.
    #[arg(long)]
    pub committed_only: bool,

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

/// Worked examples appended to `review --help`'s long help.
const REVIEW_EXAMPLES: &str = "\
Examples:
  # On a feature branch, after `dbt compile`: detect the base, diff,
  # render, open.
  cute-dbt review

  # Diff against a specific base and skip auto-open (scripts/agents).
  cute-dbt review --base origin/main --no-open

  # Exactly what a PR would show — committed changes only.
  cute-dbt review --committed-only

  # Show every command a real run would execute, run nothing.
  cute-dbt review --dry-run

Review walks: dbt project discovery -> base detection -> merge-base ->
`git diff --unified=0` (config-proof flag set) -> the same in-process
pipeline as `cute-dbt report --pr-diff` -> report written ->
auto-open on a TTY. The manifest must already be compiled
(`dbt compile`); a persisted base lives in `git config cute-dbt.base`.";

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
    /// git/ladder half and the project/stage half so each match stays
    /// readable (and under the line-count lint).
    fn describe(&self) -> (String, String) {
        self.describe_git()
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
                let from = match source {
                    BaseSource::Flag => "--base",
                    BaseSource::GitConfig => "git config cute-dbt.base",
                };
                (
                    format!(
                        "the base ref `{ref_name}` (from {from}) does not resolve to a commit."
                    ),
                    format!(
                        "Fetch it (`git fetch origin {ref_name}`) or pass a different \
                         `--base <ref>`."
                    ),
                )
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
            // Explicit (never `_`): a new variant must fail to compile
            // here AND in describe_project, forcing a deliberate
            // remediation decision — the exit.rs precedent.
            Self::ProjectNotFound { .. }
            | Self::ProjectAmbiguous { .. }
            | Self::ProjectOutsideRepo { .. }
            | Self::ManifestMissing { .. }
            | Self::StageFailed { .. } => return None,
        };
        Some(pair)
    }

    /// The project/manifest/stage arms.
    ///
    /// # Panics
    ///
    /// Unreachable for the git-and-ladder variants — [`Self::describe`]
    /// routes those through [`Self::describe_git`] first.
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
            Self::ManifestMissing { path } => (
                format!("no compiled manifest at `{}`.", path.display()),
                "Run `dbt compile` in your dbt project so target/manifest.json exists, \
                 then re-run `cute-dbt review`."
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
            | Self::DisjointHistories { .. } => {
                unreachable!("describe() routes git/ladder variants through describe_git()")
            }
        }
    }
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
    /// Rung 2: the `origin/HEAD` symref.
    OriginHead,
    /// Rung 3a: probing `origin/{main,master,trunk}`.
    RemoteProbe,
    /// Rung 3b: probing local `{main,master,trunk}` heads.
    LocalProbe,
}

impl Rung {
    /// Short human label for the stderr announcement.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::ExplicitFlag => "--base",
            Self::GitConfig => "git config cute-dbt.base",
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
}

impl DiffScope {
    /// The revision argument(s) for `git diff` under this scope.
    fn rev_args(self, merge_base: &str) -> Vec<String> {
        match self {
            Self::WorkingTree => vec![merge_base.to_owned()],
            Self::CommittedOnly => vec![format!("{merge_base}..HEAD")],
        }
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
        }
    }
}

/// Resolve the manifest location: `DBT_TARGET_PATH` (relative values
/// join the project dir — fusion's `in_out_dir` semantics) else the
/// conventional `<project>/target`, then `manifest.json`.
#[must_use]
pub fn resolve_target_dir(project_dir: &Path, dbt_target_path: Option<&str>) -> PathBuf {
    match dbt_target_path {
        Some(p) if !p.trim().is_empty() => {
            let path = Path::new(p);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                project_dir.join(path)
            }
        }
        _ => project_dir.join("target"),
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
    let cwd = env::current_dir().map_err(|err| ReviewError::StageFailed {
        context: "reading the working directory",
        detail: err.to_string(),
    })?;
    ensure_git_repo(&cwd)?;
    let toplevel = git_toplevel(&cwd)?;
    let project_dir = resolve_project_dir(args.project_dir.as_deref(), &cwd)?;
    let project_rel = project_rel_to_toplevel(&project_dir, &toplevel)?;
    eprintln!("cute-dbt: dbt project: {}", project_dir.display());

    let facts = gather_base_facts(&toplevel, args.base.as_deref())?;
    let (base_ref, rung) = decide_base(&facts)?;
    let merge_base = find_merge_base(&toplevel, &base_ref)?;
    eprintln!(
        "cute-dbt: base: {base_ref} (via {}; merge-base {})",
        rung.describe(),
        short_sha(&merge_base),
    );

    let scope = if args.committed_only {
        DiffScope::CommittedOnly
    } else {
        DiffScope::WorkingTree
    };
    let diff = diff_plan(&toplevel, &project_rel, &merge_base, scope);
    let target_dir = resolve_target_dir(&project_dir, env::var("DBT_TARGET_PATH").ok().as_deref());
    let compose = ComposeInputs {
        manifest: target_dir.join("manifest.json"),
        project_root: rel_or_dot(&project_rel),
        out: resolve_out_path(args.out.as_deref(), &cwd, &target_dir),
        config: args.config.clone(),
    };

    if args.dry_run {
        print_dry_run(&diff, &compose);
        return Ok(());
    }

    if !compose.manifest.is_file() {
        return Err(ReviewError::ManifestMissing {
            path: compose.manifest.clone(),
        }
        .into());
    }
    warn_untracked(&toplevel, &project_rel);

    let patch = run_diff(&diff)?;
    if patch.trim().is_empty() && !args.force {
        eprintln!(
            "cute-dbt: nothing to review — no changes vs {base_ref} (merge-base {}). \
             No report written; pass --force to render the zero-scope report anyway.",
            short_sha(&merge_base),
        );
        return Ok(());
    }
    let pr_diff = parse_diff(&patch).map_err(|detail| ReviewError::StageFailed {
        context: "parsing the diff git produced",
        detail,
    })?;

    // The composed run loop resolves the relative `--project-root`
    // (diff-side path strip + working-tree YAML reads) against the
    // process cwd, so move to the repo toplevel — the CI recipe's
    // exact shape (cwd = checkout root, relative --project-root).
    // Every path the operator supplied was absolutized above, before
    // this point.
    env::set_current_dir(&toplevel).map_err(|err| ReviewError::StageFailed {
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

/// Gather every ladder fact with read-only git queries (each rung's
/// candidate verified to resolve where the ladder requires it).
fn gather_base_facts(toplevel: &Path, explicit: Option<&str>) -> Result<BaseFacts, ReviewError> {
    let verify = |name: &str| -> Result<bool, ReviewError> {
        let probe = git_query(
            toplevel,
            &["rev-parse", "-q", "--verify", &format!("{name}^{{commit}}")],
        )?;
        Ok(probe.status.success())
    };
    let explicit = match explicit {
        Some(name) => Some(RefProbe {
            name: name.to_owned(),
            resolves: verify(name)?,
        }),
        None => None,
    };
    let configured = {
        let out = git_query(toplevel, &["config", "--get", "cute-dbt.base"])?;
        let name = stdout_line(&out);
        if out.status.success() && !name.is_empty() {
            let resolves = verify(&name)?;
            Some(RefProbe { name, resolves })
        } else {
            None
        }
    };
    let origin_head = {
        let out = git_query(
            toplevel,
            &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
        )?;
        let name = stdout_line(&out);
        if out.status.success() && !name.is_empty() && verify(&name)? {
            Some(name)
        } else {
            None
        }
    };
    let mut remote_probe = None;
    let mut local_probe = None;
    for candidate in ["main", "master", "trunk"] {
        if remote_probe.is_none() {
            let r = git_query(
                toplevel,
                &[
                    "show-ref",
                    "--verify",
                    "-q",
                    &format!("refs/remotes/origin/{candidate}"),
                ],
            )?;
            if r.status.success() {
                remote_probe = Some(format!("origin/{candidate}"));
            }
        }
        if local_probe.is_none() {
            let l = git_query(
                toplevel,
                &[
                    "show-ref",
                    "--verify",
                    "-q",
                    &format!("refs/heads/{candidate}"),
                ],
            )?;
            if l.status.success() {
                local_probe = Some(candidate.to_owned());
            }
        }
    }
    Ok(BaseFacts {
        explicit,
        configured,
        origin_head,
        remote_probe,
        local_probe,
    })
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
fn print_dry_run(diff: &CommandPlan, compose: &ComposeInputs) {
    println!("cute-dbt review --dry-run: a real run executes, in order:");
    println!("  [git diff]       {}", diff.rendered());
    println!(
        "  [cute-dbt report] (cwd: {}) {}",
        diff.cwd.display(),
        compose
            .report_display_argv()
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" "),
    );
    println!(
        "nothing was executed and no file was written. A real run passes the diff \
         to report in-process ({DIFF_STAND_IN} stands for the first command's output)."
    );
}

/// Soft warning for untracked files under the project: invisible to
/// `git diff`, so a brand-new model would silently miss the report.
/// Never a failure — and never an index mutation (`git add -N` is the
/// operator's call).
fn warn_untracked(toplevel: &Path, project_rel: &str) {
    let plan = status_plan(toplevel, project_rel);
    let Ok(output) = plan.execute() else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let untracked: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("?? "))
        .collect();
    if untracked.is_empty() {
        return;
    }
    let shown = untracked
        .iter()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let suffix = if untracked.len() > 5 { ", …" } else { "" };
    eprintln!(
        "cute-dbt: warning: {} untracked file(s) under the project are invisible to the \
         diff: {shown}{suffix} — track them with `git add -N <file>` to include them.",
        untracked.len(),
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

    // ----- remediation messages -------------------------------------

    #[test]
    fn every_review_error_message_carries_a_remediation() {
        let samples: Vec<(ReviewError, &str)> = vec![
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
                },
                "dbt compile",
            ),
            (
                ReviewError::StageFailed {
                    context: "producing the diff",
                    detail: "boom".to_owned(),
                },
                "--dry-run",
            ),
        ];
        for (err, needle) in samples {
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

    // ----- manifest / out resolution ---------------------------------

    #[test]
    fn target_dir_defaults_to_project_target() {
        assert_eq!(
            resolve_target_dir(Path::new("/p"), None),
            PathBuf::from("/p/target"),
        );
    }

    #[test]
    fn dbt_target_path_relative_joins_the_project_dir() {
        // fusion's in_out_dir semantics: relative → joined to project.
        assert_eq!(
            resolve_target_dir(Path::new("/p"), Some("build")),
            PathBuf::from("/p/build"),
        );
        assert_eq!(
            resolve_target_dir(Path::new("/p"), Some("/abs/out")),
            PathBuf::from("/abs/out"),
        );
        assert_eq!(
            resolve_target_dir(Path::new("/p"), Some("  ")),
            PathBuf::from("/p/target"),
            "a blank value falls back to the default",
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
        assert!(!review.no_open);
        assert!(review.config.is_none());
        assert!(!review.dry_run);
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
