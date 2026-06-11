//! The clap command-line surface.
//!
//! Since cute-dbt#100 the CLI is verb-structured: `cute-dbt report`
//! (the PR-review report — baseline-required, changed-scope,
//! fail-closed) and `cute-dbt explore` (the local-dev explorer —
//! full-manifest, no baseline, fail-open on uncompiled models). Bare
//! `cute-dbt` with no subcommand is a clap usage error
//! ([`clap::error::ErrorKind::MissingSubcommand`], exit 2) listing both
//! verbs — `subcommand_required` is set deliberately so the
//! help-on-missing default (which can exit 0) can never swallow the
//! error.
//!
//! `report` carries the pre-#100 flat surface verbatim. Three required
//! arguments; baseline-required is the locked v0.1 policy (ADR-2): a
//! missing `--baseline-manifest` is a clap usage error raised before
//! any manifest is read — never a `PreflightError`.
//!
//! One optional argument: `--config <PATH>` (PR 14, #24). The clap
//! value-parser opens + parses the TOML eagerly; a bad / unreadable
//! file is a clap usage error (exit 2), not a `PreflightError` variant
//! — the same baseline-missing precedent (ARCHITECTURE.md §3) applies.

use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

use crate::adapters::config_reader::load_config;
use crate::domain::{AnalysisConfig, ModifierKind, PrDiff};

/// cute-dbt — zero-compute dbt unit-test and lineage HTML visualizer.
#[derive(Debug, Parser)]
// `subcommand_required` is explicit and `arg_required_else_help` is
// explicitly OFF: the clap derive would otherwise turn a bare
// invocation into its help-on-missing display
// (`DisplayHelpOnMissingArgumentOrSubcommand`) instead of the genuine
// `MissingSubcommand` usage error this surface pins (cute-dbt#100).
#[command(
    name = "cute-dbt",
    version,
    about,
    subcommand_required = true,
    arg_required_else_help = false
)]
pub struct Cli {
    /// The selected verb: `report` (PR review) or `explore` (local dev).
    #[command(subcommand)]
    pub command: Command,
}

/// The two cute-dbt verbs (cute-dbt#100).
// large_enum_variant: deliberately NOT boxed — clap's derive requires
// the variant field to implement `Args` (Box<ReportArgs> does not), and
// exactly one `Command` exists per process, so the size asymmetry
// between the report surface and explore's two paths is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Render a diff-scoped, self-contained HTML report of a dbt
    /// project's unit tests (PR review; baseline-required, fail-closed).
    Report(ReportArgs),
    /// Render the full manifest to a self-contained two-page explorer:
    /// dag.html (model lineage) + tests.html (unit tests). No baseline;
    /// uncompiled models render as "not compiled" instead of failing.
    Explore(ExploreArgs),
}

/// Arguments for `cute-dbt report` — the pre-#100 flat surface,
/// carried verbatim.
#[derive(Debug, Args)]
// Exactly one scope source is required: `--baseline-manifest` (dbt
// `state:modified`) XOR `--pr-diff` (a raw `git diff --unified=0` patch).
// `required(true)` + `multiple(false)` makes "neither" a
// MissingRequiredArgument and "both" an ArgumentConflict — both clap
// usage errors (exit 2), never a `PreflightError` (cute-dbt#85, ADR-2
// precedent). This preserves the v0.1 baseline-required UX: a full
// unscoped run still diffs against an empty/genesis baseline.
#[command(group = ArgGroup::new("scope_source")
    .required(true)
    .multiple(false)
    .args(["baseline_manifest", "pr_diff"]))]
pub struct ReportArgs {
    /// Path to the compiled dbt `manifest.json` to visualise.
    #[arg(long, value_name = "PATH")]
    pub manifest: PathBuf,

    /// Path to the baseline `manifest.json` to diff against (dbt
    /// `state:modified` scope source).
    ///
    /// One of the two mutually-exclusive scope sources (the other is
    /// `--pr-diff`); exactly one must be supplied. cute-dbt
    /// v0.1 is PR-review-first, so the report is scoped to the unit
    /// tests whose model changed relative to this baseline. For a
    /// full-manifest report, diff against an empty or genesis baseline
    /// (or use `cute-dbt explore`).
    #[arg(long, value_name = "PATH")]
    pub baseline_manifest: Option<PathBuf>,

    /// PR-diff scope source (CI/PR-review path) — no baseline manifest
    /// needed.
    ///
    /// Takes a raw `git diff --unified=0` patch via `@file`
    /// (`--pr-diff @diff.patch`). The workflow / Action computes the diff
    /// — `git diff --unified=0 ${base.sha}...${head.sha} > diff.patch` —
    /// and passes the file here; cute-dbt parses it (the changed-file set
    /// from each `+++ b/<path>` header, plus the per-file hunks that drive
    /// block-precise `updated` detection — cute-dbt#96). cute-dbt does
    /// not shell out to `git` or read `GITHUB_EVENT_PATH`.
    ///
    /// Mutually exclusive with `--baseline-manifest`. A bad `@file`
    /// (missing / non-UTF-8) or a value that is not a unified diff is a
    /// clap usage error (exit 2).
    #[arg(long, value_name = "@FILE", value_parser = crate::cli::pr_diff::parse_diff)]
    pub pr_diff: Option<PrDiff>,

    /// Path the generated `report.html` is written to.
    #[arg(long, value_name = "PATH")]
    pub out: PathBuf,

    /// Optional TOML configuration. Currently exposes `[report].title`
    /// and `[report].subtitle`; both override the rendered HTML's
    /// `<title>`/`<h1>` and (subtitle only) inject a new
    /// `<p class="report-subtitle">` element.
    ///
    /// A missing, unreadable, or invalid file is a clap usage error
    /// (exit 2) — never a `PreflightError`.
    #[arg(long, value_name = "PATH", value_parser = parse_config_file)]
    pub config: Option<AnalysisConfig>,

    /// Optional dbt project root — the directory that contains
    /// `dbt_project.yml` and is the anchor for the manifest's
    /// `original_file_path` entries.
    ///
    /// When supplied, cute-dbt reads each in-scope `unit_test`'s source
    /// YAML and surfaces a "Model YAML" drawer in the report
    /// (cute-dbt#69; label per the pass-2 spec, cute-dbt#233). When
    /// absent, cute-dbt attempts to derive the
    /// project root from `--manifest` (by stripping a trailing
    /// `target/manifest.json`) before silently skipping the YAML
    /// extraction.
    ///
    /// An explicit `--project-root` that does not exist or is not a
    /// directory is a clap usage error (exit 2). The implicit-derive
    /// path is soft-failing: if no `dbt_project.yml` is found at the
    /// derived location, no error fires — the report still renders
    /// without the authoring-YAML drawer.
    #[arg(long, value_name = "PATH", value_parser = parse_project_root)]
    pub project_root: Option<PathBuf>,

    /// Opt-in `state:modified` sub-selectors that widen baseline
    /// diff-scoping beyond the body-only default (comma-separated).
    ///
    /// dbt's own bare `state:modified` ORs every sub-selector together;
    /// cute-dbt's default is deliberately the narrower
    /// `state:modified.body`. This flag composes the chosen
    /// sub-selectors ALONGSIDE the always-on body checksum (the same OR
    /// union dbt applies), so e.g. `--modified-selectors configs` also
    /// scopes a config-only change (say, an incremental-strategy edit in
    /// `dbt_project.yml`) that leaves the model body checksum identical.
    /// The selector tokens match dbt's `state:modified.<sub>` vocabulary;
    /// `body` is accepted but always active. dbt's
    /// `persisted_descriptions` sub-selector is not implemented. Note
    /// `configs` diffs the manifest's *resolved* config dict, which can
    /// over-report relative to dbt's unrendered-config diff (it never
    /// under-reports).
    ///
    /// Baseline arm only: the `--pr-diff` arm scopes by changed file
    /// paths and never consults a `state:modified` comparator, so
    /// combining this flag with `--pr-diff` is a clap usage error
    /// (exit 2) rather than a silently ignored no-op.
    #[arg(
        long,
        value_name = "SELECTORS",
        value_delimiter = ',',
        conflicts_with = "pr_diff"
    )]
    pub modified_selectors: Vec<ModifiedSelector>,
}

/// Arguments for `cute-dbt explore` (cute-dbt#100) — full-manifest,
/// no baseline, two-page `--out-dir` output.
///
/// The explorer takes **no baseline manifest, ever** (the cute-dbt#106
/// founder respec): the developer-native diff signal is git — the
/// optional `--pr-diff` change context below — not environment
/// manifests, which remain a `report`-only environment-comparison
/// concern.
#[derive(Debug, Args)]
pub struct ExploreArgs {
    /// Path to the compiled dbt `manifest.json` to explore.
    ///
    /// Stage-1 pre-flight still fails CLOSED here (unreadable /
    /// pre-v12 manifests are rejected with a remediation hint), but
    /// Stage-2 fails OPEN: an uncompiled model renders as a
    /// "not compiled" node instead of aborting the run.
    #[arg(long, value_name = "PATH")]
    pub manifest: PathBuf,

    /// Directory the explorer pages are written into: `dag.html`
    /// (model lineage) and `tests.html` (unit tests). Created if it
    /// does not exist.
    #[arg(long, value_name = "DIR")]
    pub out_dir: PathBuf,

    /// Optional PR-diff **change context** (cute-dbt#106): highlight the
    /// models whose files changed on the developer's branch — on the
    /// FULL graph.
    ///
    /// Accepts exactly the `report --pr-diff` input shape (the same
    /// value-parser): a raw `git diff --unified=0` patch via `@file`,
    /// or literal diff text. Change context **never narrows scope** —
    /// every model still renders; the changed ones gain a visually
    /// distinct "changed" treatment. A bad `@file` (missing /
    /// non-UTF-8) or a value that is not a unified diff is a clap usage
    /// error (exit 2), the same error class as on `report`.
    #[arg(long, value_name = "@FILE", value_parser = crate::cli::pr_diff::parse_diff)]
    pub pr_diff: Option<PrDiff>,

    /// Optional dbt project root, stripped from the diff's
    /// repo-relative paths so they match the manifest's
    /// project-relative `original_file_path` entries (the same strip
    /// `report --pr-diff` applies via its `--project-root`; needed when
    /// the dbt project lives in a repo subdirectory).
    ///
    /// Only meaningful as the diff-side strip, so supplying it without
    /// `--pr-diff` is a clap usage error (exit 2) rather than a
    /// silently ignored no-op (the `--modified-selectors` precedent).
    #[arg(long, value_name = "PATH", value_parser = parse_project_root, requires = "pr_diff")]
    pub project_root: Option<PathBuf>,
}

/// One `--modified-selectors` token — the CLI-layer twin of the domain
/// [`ModifierKind`] (the domain stays clap-free; ADR-1).
///
/// The token set mirrors dbt's `state:modified.<sub>` sub-selector
/// vocabulary (dbt-fusion `node_selector.rs`
/// `StateModifiedSubType::from_str`, SHA `9977b6c`): `body`, `configs`,
/// `relation`, `macros`, `contract`. fusion's sixth token,
/// `persisted_descriptions`, has no cute-dbt modifier yet and is
/// rejected by clap's possible-values validation like any other unknown
/// token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModifiedSelector {
    /// `state:modified.body` — the model body checksum. Always active;
    /// accepted here so the full dbt vocabulary parses.
    Body,
    /// `state:modified.configs` — the resolved config dict changed.
    Configs,
    /// `state:modified.relation` — the fully-qualified relation name
    /// (database / schema / alias / identifier) changed.
    Relation,
    /// `state:modified.macros` — the set of upstream macros the node
    /// depends on changed.
    Macros,
    /// `state:modified.contract` — `config.contract.enforced` flipped or
    /// the declared column set changed.
    Contract,
}

impl ModifiedSelector {
    /// The domain [`ModifierKind`] this CLI token selects.
    #[must_use]
    pub fn kind(self) -> ModifierKind {
        match self {
            Self::Body => ModifierKind::Body,
            Self::Configs => ModifierKind::Configs,
            Self::Relation => ModifierKind::Relation,
            Self::Macros => ModifierKind::Macros,
            Self::Contract => ModifierKind::Contract,
        }
    }
}

/// clap value-parser: read + deserialize the TOML at `--config <PATH>`.
///
/// Errors are stringified for clap's usage-error path. The resolved
/// [`AnalysisConfig`] is stored in [`ReportArgs::config`].
fn parse_config_file(s: &str) -> Result<AnalysisConfig, String> {
    load_config(Path::new(s)).map_err(|err| err.to_string())
}

/// clap value-parser: validate that an explicit `--project-root` points
/// at an existing directory. The implicit-derive path (when no flag is
/// passed) is handled in the run loop via [`resolve_project_root`];
/// this value-parser only runs when the operator typed the flag, so
/// silent-fallback semantics are intentionally absent here.
fn parse_project_root(s: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(s);
    if !p.exists() {
        return Err(format!("project root does not exist: {s}"));
    }
    if !p.is_dir() {
        return Err(format!("project root is not a directory: {s}"));
    }
    Ok(p)
}

/// Resolve the effective dbt project root.
///
/// Resolution policy:
/// 1. If `explicit` is `Some(p)`, return it unchanged. clap's
///    value-parser already validated that `p` exists and is a directory.
/// 2. Otherwise try to derive from `manifest_path` by stripping a
///    trailing `target/manifest.json` — the standard dbt layout. If the
///    derived directory exists, return it.
/// 3. Otherwise return `None` — cute-dbt continues silently without
///    the authoring-YAML drawer.
///
/// Returns the resolved root and a boolean: `true` if the result was
/// derived (rather than explicit). The caller may want to emit a
/// stderr breadcrumb noting that a derived root is being used.
#[must_use]
pub fn resolve_project_root(
    explicit: Option<&Path>,
    manifest_path: &Path,
) -> (Option<PathBuf>, bool) {
    if let Some(p) = explicit {
        return (Some(p.to_path_buf()), false);
    }
    if let Some(derived) = derive_project_root_from_manifest(manifest_path) {
        if derived.is_dir() {
            return (Some(derived), true);
        }
    }
    (None, false)
}

/// Strip the conventional `target/manifest.json` suffix from a manifest
/// path. Returns the parent of `target/`, which is the dbt project root
/// in the standard layout. Returns `None` for any other shape.
fn derive_project_root_from_manifest(manifest_path: &Path) -> Option<PathBuf> {
    // The suffix we recognize is exactly: <root>/target/manifest.json.
    if manifest_path.file_name()? != "manifest.json" {
        return None;
    }
    let target_dir = manifest_path.parent()?;
    if target_dir.file_name()? != "target" {
        return None;
    }
    target_dir.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_path(stem: &str) -> std::path::PathBuf {
        let nonce = COUNTER.fetch_add(1, Ordering::SeqCst);
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros());
        let pid = std::process::id();
        std::env::temp_dir().join(format!("cute-dbt-args-{pid}-{micros}-{nonce}-{stem}.toml"))
    }

    fn write_fixture(stem: &str, content: &str) -> std::path::PathBuf {
        let path = unique_temp_path(stem);
        let mut f = std::fs::File::create(&path).expect("create temp fixture");
        f.write_all(content.as_bytes()).expect("write temp fixture");
        path
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    /// Parse and unwrap the `report` arm (panics on any other arm — the
    /// report-focused tests below always pass the `report` verb).
    fn parse_report(args: &[&str]) -> Result<ReportArgs, clap::Error> {
        parse(args).map(|cli| match cli.command {
            Command::Report(report) => report,
            Command::Explore(_) => panic!("expected the report arm"),
        })
    }

    // ----- subcommand restructure (cute-dbt#100) -----

    #[test]
    fn bare_invocation_is_a_missing_subcommand_usage_error_listing_both_verbs() {
        // The locked CLI-restructure contract: bare `cute-dbt` (no
        // subcommand) is a usage error — `subcommand_required` is set
        // deliberately, never clap's help-on-missing default (which can
        // exit 0). `use_stderr` must hold so cli::run maps it to exit 2.
        let err = parse(&["cute-dbt"]).expect_err("a subcommand is required");
        assert_eq!(err.kind(), ErrorKind::MissingSubcommand);
        assert!(err.use_stderr(), "a missing subcommand is a usage error");
        let msg = err.to_string();
        assert!(msg.contains("report"), "lists the report verb: {msg}");
        assert!(msg.contains("explore"), "lists the explore verb: {msg}");
    }

    #[test]
    fn an_unknown_subcommand_is_a_usage_error() {
        let err = parse(&["cute-dbt", "frobnicate"]).expect_err("unknown verb");
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn flat_pre_verb_invocation_is_a_usage_error() {
        // The pre-#100 flat surface (no verb, flags directly) must not
        // silently keep working — the verb restructure is a deliberate
        // v0.x break surfaced as a usage error.
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
        ])
        .expect_err("flat invocation has no subcommand");
        assert!(
            err.use_stderr(),
            "the flat shape is rejected as a usage error: {err}"
        );
    }

    #[test]
    fn explore_parses_manifest_and_out_dir() {
        let cli = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "target/manifest.json",
            "--out-dir",
            "explore-out",
        ])
        .expect("a complete explore argument set parses");
        let Command::Explore(explore) = cli.command else {
            panic!("expected the explore arm");
        };
        assert_eq!(explore.manifest, PathBuf::from("target/manifest.json"));
        assert_eq!(explore.out_dir, PathBuf::from("explore-out"));
    }

    #[test]
    fn explore_requires_out_dir() {
        let err = parse(&["cute-dbt", "explore", "--manifest", "m.json"])
            .expect_err("--out-dir is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains("--out-dir"),
            "names the missing argument: {err}"
        );
    }

    #[test]
    fn explore_requires_manifest() {
        let err =
            parse(&["cute-dbt", "explore", "--out-dir", "d"]).expect_err("--manifest is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn explore_rejects_a_baseline_manifest() {
        // The explorer takes NO baseline manifest, ever (the cute-dbt#106
        // founder respec, superseding the original V7 `--baseline` cut
        // line): the developer-native diff signal is git (`--pr-diff`),
        // not environment manifests. The flag must be rejected, not
        // silently ignored.
        let err = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
            "--baseline-manifest",
            "b.json",
        ])
        .expect_err("explore takes no baseline");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    // ----- explore --pr-diff change context (cute-dbt#106) -----

    #[test]
    fn explore_without_pr_diff_carries_no_change_context() {
        let cli = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
        ])
        .expect("explore without --pr-diff parses");
        let Command::Explore(explore) = cli.command else {
            panic!("expected the explore arm");
        };
        assert!(explore.pr_diff.is_none(), "--pr-diff is optional");
        assert!(explore.project_root.is_none());
    }

    #[test]
    fn explore_pr_diff_accepts_the_report_at_file_shape() {
        // The cute-dbt#106 AC: `explore --pr-diff` accepts EXACTLY the
        // `report --pr-diff` input shape (@file / literal) — the same
        // value-parser, so the two verbs cannot drift.
        let diff = write_fixture("explore-prdiff", VALID_DIFF);
        let arg = format!("@{}", diff.display());
        let cli = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
            "--pr-diff",
            &arg,
        ])
        .expect("explore --pr-diff @file parses");
        let Command::Explore(explore) = cli.command else {
            panic!("expected the explore arm");
        };
        let parsed = explore.pr_diff.expect("pr_diff is Some");
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].path, "models/marts/core/_core__models.yml");
        let _ = std::fs::remove_file(&diff);
    }

    #[test]
    fn explore_pr_diff_at_missing_file_is_a_value_validation_error() {
        // The same error class report raises for a bad @file (reuse,
        // never a new PreflightError variant — the enum stays at four).
        let path = unique_temp_path("explore-missing-diff");
        let arg = format!("@{}", path.display());
        let err = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
            "--pr-diff",
            &arg,
        ])
        .expect_err("a missing @file is a usage error on explore too");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("could not read"),
            "error explains the read failure: {err}"
        );
    }

    #[test]
    fn explore_pr_diff_with_malformed_contents_is_a_value_validation_error() {
        let path = write_fixture("explore-malformed", "this is not a unified diff\n");
        let arg = format!("@{}", path.display());
        let err = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
            "--pr-diff",
            &arg,
        ])
        .expect_err("a non-diff @file is a usage error on explore too");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string()
                .contains("could not be parsed as a unified diff"),
            "error explains the parse failure: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn explore_project_root_requires_pr_diff() {
        // On explore the project root exists ONLY as the diff-side path
        // strip; without --pr-diff it would be a silent no-op, so it is
        // rejected at parse time (the --modified-selectors precedent).
        let dir = unique_temp_dir("explore-root-alone");
        let err = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
            "--project-root",
            dir.to_str().expect("temp dir utf-8"),
        ])
        .expect_err("--project-root without --pr-diff is a usage error");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains("--pr-diff"),
            "the error names the missing companion flag: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explore_pr_diff_with_project_root_parses() {
        let diff = write_fixture("explore-prdiff-root", VALID_DIFF);
        let arg = format!("@{}", diff.display());
        let dir = unique_temp_dir("explore-root");
        let cli = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
            "--pr-diff",
            &arg,
            "--project-root",
            dir.to_str().expect("temp dir utf-8"),
        ])
        .expect("explore --pr-diff with --project-root parses");
        let Command::Explore(explore) = cli.command else {
            panic!("expected the explore arm");
        };
        assert_eq!(explore.project_root, Some(dir.clone()));
        let _ = std::fs::remove_file(&diff);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explore_rejects_modified_selectors() {
        // state:modified sub-selectors are a baseline-arm concept; the
        // explorer has no baseline, so the flag stays off this verb.
        let err = parse(&[
            "cute-dbt",
            "explore",
            "--manifest",
            "m.json",
            "--out-dir",
            "d",
            "--modified-selectors",
            "configs",
        ])
        .expect_err("explore takes no state:modified sub-selectors");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    // ----- report arm (the pre-#100 surface, carried verbatim) -----

    #[test]
    fn all_three_arguments_parse_into_their_paths() {
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "current.json",
            "--baseline-manifest",
            "baseline.json",
            "--out",
            "report.html",
        ])
        .expect("a complete argument set parses");
        assert_eq!(report.manifest, PathBuf::from("current.json"));
        assert_eq!(
            report.baseline_manifest,
            Some(PathBuf::from("baseline.json"))
        );
        assert_eq!(report.out, PathBuf::from("report.html"));
        // --config absent: the field is None.
        assert!(report.config.is_none());
        // --pr-diff absent: the field is None (baseline path).
        assert!(report.pr_diff.is_none());
    }

    /// A minimal valid `git diff --unified=0` patch for the @file-form
    /// tests (a multi-line diff cannot be a clap value — it would parse
    /// as flags — so the CLI surface always reads `@file`).
    const VALID_DIFF: &str = "--- a/models/marts/core/_core__models.yml\n\
+++ b/models/marts/core/_core__models.yml\n\
@@ -5 +5 @@\n\
-      rows: []\n\
+      rows: [{id: 1}]\n";

    #[test]
    fn a_missing_baseline_manifest_is_a_usage_error() {
        // Passing NEITHER scope source: the `scope_source` ArgGroup is
        // required, so omitting both --baseline-manifest and
        // --pr-diff is a clap usage error, never a
        // PreflightError (cute-dbt#85).
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--out",
            "o.html",
        ])
        .expect_err("a scope source is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains("--baseline-manifest"),
            "the error names the missing scope source: {err}"
        );
    }

    #[test]
    fn passing_both_scope_sources_is_an_argument_conflict() {
        // The `scope_source` group is `multiple(false)` — supplying both
        // --baseline-manifest and --pr-diff is a conflict. The --pr-diff
        // value must PARSE (clap runs the value-parser before group
        // validation), so it points at a valid diff @file.
        let diff = write_fixture("both-conflict", VALID_DIFF);
        let arg = format!("@{}", diff.display());
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "current.json",
            "--baseline-manifest",
            "baseline.json",
            "--pr-diff",
            &arg,
            "--out",
            "report.html",
        ])
        .expect_err("both scope sources is a conflict");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
        let _ = std::fs::remove_file(&diff);
    }

    #[test]
    fn pr_diff_alone_parses_without_a_baseline() {
        let diff = write_fixture("alone", VALID_DIFF);
        let arg = format!("@{}", diff.display());
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "current.json",
            "--pr-diff",
            &arg,
            "--out",
            "report.html",
        ])
        .expect("pr-diff-only is a complete argument set");
        assert!(report.baseline_manifest.is_none());
        let parsed = report.pr_diff.expect("pr_diff is Some");
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].path, "models/marts/core/_core__models.yml");
        let _ = std::fs::remove_file(&diff);
    }

    #[test]
    fn pr_diff_at_missing_file_is_a_value_validation_error() {
        let path = unique_temp_path("missing-diff");
        // Deliberately do NOT create the file.
        let arg = format!("@{}", path.display());
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "current.json",
            "--pr-diff",
            &arg,
            "--out",
            "report.html",
        ])
        .expect_err("a missing @file is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("could not read"),
            "error explains the read failure: {err}"
        );
    }

    #[test]
    fn pr_diff_with_malformed_contents_is_a_value_validation_error() {
        let path = write_fixture("malformed", "this is not a unified diff\n");
        let arg = format!("@{}", path.display());
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "current.json",
            "--pr-diff",
            &arg,
            "--out",
            "report.html",
        ])
        .expect_err("a non-diff @file is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string()
                .contains("could not be parsed as a unified diff"),
            "error explains the parse failure: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_manifest_is_a_usage_error() {
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
        ])
        .expect_err("--manifest is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_missing_out_is_a_usage_error() {
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
        ])
        .expect_err("--out is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn help_is_a_display_help_error_kind() {
        let err = parse(&["cute-dbt", "--help"]).expect_err("--help short-circuits parsing");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn an_unknown_argument_is_a_usage_error() {
        // clap rejects any flag not on the report surface. PR 14
        // (cute-dbt#24) added --config to the surface, so the test uses a
        // different genuinely-unknown flag to pin clap's behavior.
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--frobnitz",
            "value",
        ])
        .expect_err("--frobnitz is not a cute-dbt argument");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn a_valid_config_file_parses_into_some() {
        let path = write_fixture(
            "valid",
            r#"
[report]
title = "Q3 review"
subtitle = "PR 1234"
"#,
        );
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect("a valid config parses");
        let cfg = report.config.expect("config is Some");
        assert_eq!(cfg.report.title.as_deref(), Some("Q3 review"));
        assert_eq!(cfg.report.subtitle.as_deref(), Some("PR 1234"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_config_file_is_a_value_validation_error() {
        let path = unique_temp_path("does-not-exist");
        // Deliberately do NOT create the file.
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("missing config file is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("could not read config file"),
            "error explains the read failure: {err}"
        );
    }

    #[test]
    fn an_invalid_toml_config_is_a_value_validation_error() {
        let path = write_fixture("broken", "not valid toml { = =");
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("invalid TOML is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("invalid TOML in config file"),
            "error explains the parse failure: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_config_field_is_a_value_validation_error() {
        // deny_unknown_fields rejects typo'd keys; surfaces as the same
        // clap usage error path as wholesale-invalid TOML.
        let path = write_fixture(
            "typo",
            r#"
[report]
tilte = "typo'd"
"#,
        );
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("typo'd config key is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        let _ = std::fs::remove_file(&path);
    }

    fn unique_temp_dir(stem: &str) -> std::path::PathBuf {
        let nonce = COUNTER.fetch_add(1, Ordering::SeqCst);
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros());
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("cute-dbt-args-{pid}-{micros}-{nonce}-{stem}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        p
    }

    #[test]
    fn project_root_is_optional_when_omitted() {
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
        ])
        .expect("no --project-root parses");
        assert!(report.project_root.is_none());
    }

    #[test]
    fn explicit_project_root_is_validated_to_exist_and_be_a_dir() {
        let dir = unique_temp_dir("valid-root");
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--project-root",
            dir.to_str().expect("temp dir utf-8"),
        ])
        .expect("an existing directory parses");
        assert_eq!(report.project_root, Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_project_root_directory_is_a_usage_error() {
        let path = unique_temp_path("missing-root");
        // Deliberately do NOT create the directory.
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--project-root",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("missing --project-root is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("does not exist"),
            "error names the missing directory: {err}"
        );
    }

    #[test]
    fn a_file_supplied_as_project_root_is_a_usage_error() {
        // A non-directory path that DOES exist (a file) is still
        // wrong — the project root must be a directory.
        let file = write_fixture("not-a-dir", "irrelevant");
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--project-root",
            file.to_str().expect("temp path utf-8"),
        ])
        .expect_err("non-dir --project-root is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("not a directory"),
            "error names the not-a-directory condition: {err}"
        );
        let _ = std::fs::remove_file(&file);
    }

    // ----- --modified-selectors (cute-dbt#160) -----

    #[test]
    fn modified_selectors_defaults_to_empty() {
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
        ])
        .expect("no --modified-selectors parses");
        assert!(
            report.modified_selectors.is_empty(),
            "the default is the body-only scope — no opt-in selectors",
        );
    }

    #[test]
    fn modified_selectors_parses_comma_separated_tokens() {
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--modified-selectors",
            "configs,relation",
        ])
        .expect("comma-separated selectors parse");
        assert_eq!(
            report.modified_selectors,
            vec![ModifiedSelector::Configs, ModifiedSelector::Relation],
        );
    }

    #[test]
    fn modified_selectors_accepts_the_full_state_modified_vocabulary() {
        // The token set mirrors dbt's state:modified.<sub> vocabulary
        // (fusion `StateModifiedSubType::from_str` @ 9977b6c), `body`
        // included — minus the unimplemented `persisted_descriptions`.
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--modified-selectors",
            "body,configs,relation,macros,contract",
        ])
        .expect("every implemented dbt sub-selector token parses");
        assert_eq!(report.modified_selectors.len(), 5);
    }

    #[test]
    fn modified_selectors_maps_each_token_to_its_domain_kind() {
        use crate::domain::ModifierKind;
        let pairs = [
            (ModifiedSelector::Body, ModifierKind::Body),
            (ModifiedSelector::Configs, ModifierKind::Configs),
            (ModifiedSelector::Relation, ModifierKind::Relation),
            (ModifiedSelector::Macros, ModifierKind::Macros),
            (ModifiedSelector::Contract, ModifierKind::Contract),
        ];
        for (selector, kind) in pairs {
            assert_eq!(selector.kind(), kind);
        }
    }

    #[test]
    fn modified_selectors_repeated_flag_accumulates() {
        let report = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--modified-selectors",
            "configs",
            "--modified-selectors",
            "macros",
        ])
        .expect("a repeated flag accumulates values");
        assert_eq!(
            report.modified_selectors,
            vec![ModifiedSelector::Configs, ModifiedSelector::Macros],
        );
    }

    #[test]
    fn an_unknown_modified_selector_is_a_usage_error_naming_the_vocabulary() {
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--modified-selectors",
            "frobnitz",
        ])
        .expect_err("an unknown selector token is a usage error");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
        let msg = err.to_string();
        for token in ["body", "configs", "relation", "macros", "contract"] {
            assert!(
                msg.contains(token),
                "the remediation lists the valid token {token:?}: {msg}"
            );
        }
    }

    #[test]
    fn persisted_descriptions_is_rejected_until_a_modifier_exists() {
        // fusion's sixth sub-selector token; cute-dbt has no modifier
        // for it, so it must fail with the same possible-values
        // remediation — never silently parse as a no-op.
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--modified-selectors",
            "persisted_descriptions",
        ])
        .expect_err("persisted_descriptions is not implemented");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn modified_selectors_with_pr_diff_is_an_argument_conflict() {
        // The PrDiff arm scopes by file paths and never consults a
        // StateComparator — the flag would be a silent no-op there, so
        // it is rejected at parse time instead.
        let diff = write_fixture("selectors-prdiff-conflict", VALID_DIFF);
        let arg = format!("@{}", diff.display());
        let err = parse_report(&[
            "cute-dbt",
            "report",
            "--manifest",
            "m.json",
            "--pr-diff",
            &arg,
            "--out",
            "o.html",
            "--modified-selectors",
            "configs",
        ])
        .expect_err("--modified-selectors is baseline-arm only");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
        let msg = err.to_string();
        assert!(
            msg.contains("--modified-selectors") && msg.contains("--pr-diff"),
            "the conflict names both flags: {msg}"
        );
        let _ = std::fs::remove_file(&diff);
    }

    #[test]
    fn resolve_uses_explicit_root_when_supplied() {
        let dir = unique_temp_dir("explicit");
        let (resolved, derived) = resolve_project_root(Some(&dir), Path::new("/tmp/no.json"));
        assert_eq!(resolved.as_deref(), Some(dir.as_path()));
        assert!(!derived, "explicit is not derived");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_derives_from_manifest_path_with_target_layout() {
        // Set up a synthetic <root>/target/manifest.json layout.
        let project_root = unique_temp_dir("project-with-target");
        let target = project_root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let manifest = target.join("manifest.json");
        std::fs::write(&manifest, "{}").unwrap();

        let (resolved, derived) = resolve_project_root(None, &manifest);
        assert_eq!(resolved.as_deref(), Some(project_root.as_path()));
        assert!(derived, "resolved-via-derive is flagged");

        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn resolve_returns_none_when_manifest_path_is_unconventional() {
        // A manifest not under `target/manifest.json` — no derive
        // is possible. The result is `None` for both fields.
        let resolved = resolve_project_root(None, Path::new("/tmp/arbitrary/foo.json"));
        assert_eq!(resolved.0, None);
        assert!(!resolved.1);
    }
}
