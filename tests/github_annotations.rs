//! End-to-end integration of the finding→anchor resolver + the GitHub
//! workflow-command annotation formatter (cute-dbt#393).
//!
//! The unit tests in `src/domain/finding_anchor.rs` and
//! `src/adapters/github_annotations.rs` drive each half over synthetic
//! inputs; this suite runs the SAME pipeline the consumer-facing
//! `--annotations` emit will run — over the committed fusion-compiled
//! `playground-current.json` fixture, through the real Stage-1 manifest
//! adapter and the real check engine — so the resolver + formatter are
//! proven against a real wire shape, not just hand-built findings.
//!
//! The CLI flag/emit wiring that calls this pipeline lands in a fast
//! follow-up (it touches `src/cli/args.rs` + `src/cli/mod.rs`, which were
//! being amended concurrently by cute-dbt#388 — the findings envelope —
//! when this slice was built). This integration test is the standing proof
//! that once wired, the emit produces correct annotations.

use std::path::{Path, PathBuf};

use cute_dbt::adapters::cte_engine::parse_cte_graph;
use cute_dbt::adapters::github_annotations::{
    AnnotationLevels, DEFAULT_ANNOTATION_CAP, FindingsSummary, emit_annotations, summary_markdown,
};
use cute_dbt::domain::{
    CheckPolicy, FileHunks, Finding, HeuristicId, Hunk, Manifest, NodeId, NormalizedDiffIndex,
    PrDiff, apply_check_policy, model_findings, resolve_finding_anchor,
};
use cute_dbt::ports::ManifestSource;

const MONTHLY: &str = "model.healthcare_analytics.fct_encounters_monthly";
const MONTHLY_FILE: &str = "models/marts/core/fct_encounters_monthly.sql";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn load(name: &str) -> Manifest {
    cute_dbt::adapters::manifest::FileManifestSource
        .load(&fixture(name))
        .expect("committed fixture passes Stage-1 preflight")
}

/// The default-policy findings for one model — the same `model_findings`
/// → `apply_check_policy` pipeline the renderer runs.
fn findings_for(manifest: &Manifest, model_id: &str) -> Vec<Finding<HeuristicId>> {
    let model = manifest
        .node(&NodeId::new(model_id))
        .expect("model exists in fixture");
    let graph = parse_cte_graph(model.compiled_code().unwrap_or_default()).unwrap_or_default();
    let findings = model_findings(manifest, model, Some(&graph));
    apply_check_policy(findings, &CheckPolicy::<HeuristicId>::default())
}

/// A one-file diff touching `path` (a single edited hunk at `line`).
fn diff_touching(path: &str, line: usize) -> NormalizedDiffIndex {
    let diff = PrDiff {
        files: vec![FileHunks {
            path: path.to_owned(),
            hunks: vec![Hunk {
                new_start: line,
                new_len: 1,
                removed_lines: vec!["old".to_owned()],
                added_lines: vec!["new".to_owned()],
            }],
        }],
        renames: Vec::new(),
        deleted: Vec::new(),
    };
    NormalizedDiffIndex::new(&diff, None)
}

#[test]
fn uncovered_finding_on_a_changed_model_emits_an_inline_annotation() {
    let manifest = load("playground-current.json");
    let findings = findings_for(&manifest, MONTHLY);
    // The composite-key model carries a real Total-tier uncovered finding.
    assert!(
        findings
            .iter()
            .any(|f| matches!(f.verdict, cute_dbt::domain::Verdict::Uncovered)),
        "fct_encounters_monthly should carry an uncovered finding on real data"
    );

    let index = diff_touching(MONTHLY_FILE, 7);
    let emit = emit_annotations(
        &findings,
        AnnotationLevels::enforcing(),
        DEFAULT_ANNOTATION_CAP,
        &|finding| resolve_finding_anchor(finding, &manifest, &index),
    );

    assert!(emit.total >= 1, "at least one annotatable finding");
    let first = emit
        .lines
        .first()
        .expect("an uncovered finding emits a command");
    // Total-tier under the enforcing posture → ::error, anchored at the
    // model file's first changed line (7), titled with the check id.
    assert!(
        first.starts_with(
            "::error file=models/marts/core/fct_encounters_monthly.sql,line=7,title=cute-dbt%3A "
        ),
        "unexpected annotation line: {first}"
    );
}

#[test]
fn finding_on_an_untouched_model_is_summary_only() {
    let manifest = load("playground-current.json");
    let findings = findings_for(&manifest, MONTHLY);
    // The diff touches a DIFFERENT file, so the model's first-changed-line
    // resolution finds nothing → no inline annotation (summary-only).
    let index = diff_touching("models/marts/core/some_other_model.sql", 3);
    let emit = emit_annotations(
        &findings,
        AnnotationLevels::advisory(),
        DEFAULT_ANNOTATION_CAP,
        &|finding| resolve_finding_anchor(finding, &manifest, &index),
    );
    assert!(
        emit.lines.is_empty(),
        "no annotation when the model file is not in the diff: {:?}",
        emit.lines
    );
}

#[test]
fn summary_rollup_counts_the_models_findings() {
    let manifest = load("playground-current.json");
    let findings = findings_for(&manifest, MONTHLY);
    let summary = FindingsSummary::tally(&findings);
    // The composite-key gap is a Total-tier uncovered finding.
    assert!(
        summary.total_uncovered >= 1,
        "expected a total-tier uncovered gap, got {summary:?}"
    );
    let md = summary_markdown(&summary, Some("https://example/report.html"));
    assert!(md.contains("uncovered"));
    assert!(md.contains("[Full report](https://example/report.html)"));
}
