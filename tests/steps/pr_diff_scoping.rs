//! Step definitions for `features/pr_diff_scoping.feature` — the
//! `--pr-diff` CI/PR-review path (cute-dbt#84; renamed from
//! `--scope-from-pr-diff` + reshaped to a raw `git diff --unified=0`
//! patch at cute-dbt#96).
//!
//! Each scenario builds a synthetic in-memory `Manifest` via the builders
//! (no committed fixture files — the synthetic-only-fixture invariant is
//! satisfied trivially), serializes it to a temp file, **synthesizes a
//! `git diff --unified=0` patch** (and the working-tree YAML it
//! references) from the changed-file Givens, runs the real `cute-dbt`
//! subprocess with `--pr-diff @<patch>`, and asserts against the embedded
//! `cute-dbt-data` JSON payload (the same parse strategy as
//! `consumer_report_contract.rs`).
//!
//! The diff + the working-tree YAML are generated together in the When
//! (the revision-alignment invariant enforced inside the harness): a
//! changed YAML file that declares tests gets multi-block content written
//! under `<workdir>/<project-root>/` and a **whole-file hunk** spanning
//! every block (the note-#7 footprint, so a migrated `updated` scenario
//! stays green once cute-dbt#96 Step 2's block-precision lands). A
//! changed SQL / non-dbt file gets a minimal hunk and no working-tree
//! file.
//!
//! Step partition (cucumber-rs has one global step namespace):
//! - REUSE — `the exit code is {0,non-zero}`, `no file "report.html" is
//!   written` (defined in `report_generation.rs`); `a baseline manifest
//!   "baseline.json"` (no-op in `fail_closed.rs`, harmless for the
//!   clap-conflict scenario).
//! - NEW/RENAME — every Given/When/Then below uses pr-diff-unique wording
//!   so it routes here, not to the baseline-comparison impls.

use std::path::{Path, PathBuf};

use cucumber::{given, then, when};
use serde_json::Value;

use cute_dbt::domain::{Manifest, normalize_path};

use super::super::common;
use super::World;
use super::builders::{
    empty_manifest, model_node_with_ofp_and_patch_path, model_node_with_original_file_path,
    serialize_to_tmp, unit_test_for, unit_test_with_path, with_node, with_unit_test,
};
use super::world::{
    BlockTarget, BlockTargetKind, ModelSqlTarget, ModelSqlTargetKind, RenameDirective,
};

/// Synthetic compiled SQL with one import CTE, so a rendered model card
/// carries a non-empty CTE DAG (the `contains a CTE diagram for …`
/// assertion).
const COMPILED_WITH_CTE: &str = "with src as (select 1 as id) select id from src";

/// Derive a bare model name from a `.sql` `original_file_path`
/// (`models/marts/core/dim_payers.sql` → `dim_payers`). Non-`.sql` paths
/// fall back to the final path segment.
fn bare_from_ofp(ofp: &str) -> String {
    let file = ofp.rsplit('/').next().unwrap_or(ofp);
    file.strip_suffix(".sql").unwrap_or(file).to_owned()
}

/// Take the current synthetic manifest, seeding an empty (valid v12) one
/// when no model-building Given has run yet (zero-scope scenarios).
fn take_current(world: &mut World) -> cute_dbt::domain::Manifest {
    world.current_manifest.take().unwrap_or_else(empty_manifest)
}

// --- PR-diff synthesis (the revision-aligned harness) ---------------

/// The project-root strip for matching changed (repo-relative) paths
/// against manifest (project-relative) `original_file_path`s. `"."` is
/// the identity strip.
fn strip_of(project_root: &str) -> Option<&Path> {
    if project_root == "." {
        None
    } else {
        Some(Path::new(project_root))
    }
}

/// The test names a changed YAML file declares, in stable (sorted) order
/// — found by matching the manifest's `original_file_path`s against the
/// changed path under the same normalization the binary uses.
fn tests_declared_in(manifest: &Manifest, changed: &str, project_root: &str) -> Vec<String> {
    let changed_norm = normalize_path(changed, strip_of(project_root));
    let mut names: Vec<String> = manifest
        .unit_tests()
        .values()
        .filter(|ut| {
            ut.original_file_path()
                .is_some_and(|p| normalize_path(p, None) == changed_norm)
        })
        .map(|ut| ut.name().to_owned())
        .collect();
    names.sort_unstable();
    names
}

/// Deterministic multi-block YAML for `tests` (declaration-ordered),
/// preceded by a top-level `models:` region so a hunk has somewhere
/// out-of-block to land (cute-dbt#96 Step 2's `outside` scenario). Each
/// block is multi-line so a hunk can fall inside vs outside it.
fn synth_yaml(tests: &[String]) -> String {
    let mut s = String::from(
        "version: 2\n\nmodels:\n  - name: synthetic_model\n    description: synthetic out-of-block region\n\nunit_tests:\n",
    );
    for t in tests {
        s.push_str(&format!(
            "  - name: {t}\n    model: synthetic_model\n    given: []\n    expect:\n      rows: []\n"
        ));
    }
    s
}

// [`synth_yaml`]'s layout: a fixed header then a fixed-size block per test
// (no blank lines between blocks). The block-targeting synthesizer places
// hunks by arithmetic on these, so a hunk lands exactly where the #69
// slicer computes the block span (the revision-alignment the whole feature
// rests on). KEEP IN SYNC with `synth_yaml`.
const SYNTH_HEADER_LINES: usize = 7; // version / "" / models / -name / desc / "" / unit_tests:
const SYNTH_BLOCK_LINES: usize = 5; // - name / model / given / expect / rows

/// 1-based inclusive `[block_start, block_end]` of the test at sorted index
/// `i` in a [`synth_yaml`]-shaped file — matches the slicer's span.
fn block_range(i: usize) -> (usize, usize) {
    let start = SYNTH_HEADER_LINES + 1 + i * SYNTH_BLOCK_LINES;
    (start, start + SYNTH_BLOCK_LINES - 1)
}

/// The `diff --git` + `---`/`+++` file header real `git diff` precedes each
/// file's hunks with. Without it the parser, still in a prior file's hunk,
/// would eat this file's `--- a/…` line as a removed body line (Step 1 note).
fn push_file_header(patch: &mut String, changed: &str) {
    patch.push_str(&format!(
        "diff --git a/{changed} b/{changed}\n--- a/{changed}\n+++ b/{changed}\n"
    ));
}

/// Append one unified-diff hunk: the `@@` header then the `-`/`+` bodies.
/// `old_start` is cosmetic — refine reads only the new side.
fn push_hunk(
    patch: &mut String,
    old_start: usize,
    new_start: usize,
    removed: &[String],
    added: &[String],
) {
    patch.push_str(&format!(
        "@@ -{old_start},{} +{new_start},{} @@\n",
        removed.len(),
        added.len(),
    ));
    for r in removed {
        patch.push_str(&format!("-{r}\n"));
    }
    for a in added {
        patch.push_str(&format!("+{a}\n"));
    }
}

/// Emit block-targeted hunks for a YAML file (cute-dbt#96 Step 2). `tests`
/// is the sorted declared-test list `synth_yaml` wrote, so each target's
/// block index is its position in `tests`.
fn emit_targeted_hunks(
    patch: &mut String,
    content: &str,
    tests: &[String],
    targets: &[BlockTargetKind],
) {
    let lines: Vec<&str> = content.lines().collect();

    // Stale is exclusive: a whole-file hunk whose `+` lines drift from the
    // working tree → every block's N7b alignment fails → file-granular keep.
    if targets.iter().any(|k| matches!(k, BlockTargetKind::Stale)) {
        let removed: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
        let added: Vec<String> = lines
            .iter()
            .map(|l| format!("{l}  # STALE-DRIFT"))
            .collect();
        push_hunk(patch, 1, 1, &removed, &added);
        return;
    }

    // Collect (new_start, removed, added) per target, then emit ascending by
    // new_start (unified-diff hunks within a file are ordered).
    let block_index = |name: &str| {
        tests
            .iter()
            .position(|t| t == name)
            .expect("targeted test is declared in this YAML")
    };
    let mut hunks: Vec<(usize, Vec<String>, Vec<String>)> = Vec::new();
    for kind in targets {
        match kind {
            BlockTargetKind::EditsTest(name) => {
                let (bs, _be) = block_range(block_index(name));
                let model_line = bs + 1; // `    model: synthetic_model`
                let working = lines[model_line - 1].to_owned();
                // A real edit: old content differs; new == working tree (so
                // the block stays N7b-aligned and the hunk touches the block).
                hunks.push((model_line, vec![format!("{working}  # was")], vec![working]));
            }
            BlockTargetKind::DeletesFromTest(name) => {
                let (bs, _be) = block_range(block_index(name));
                let del_line = bs + 2; // the `    given: []` line, inside the block
                let working = lines[del_line - 1].to_owned();
                // Pure deletion: 1 line removed, 0 added; the new-side gap
                // sits after `del_line - 1` (a zero-count point-touch).
                hunks.push((del_line - 1, vec![working], Vec::new()));
            }
            BlockTargetKind::EditsOutside => {
                // The `description:` line in the `models:` region (line 5),
                // above every test block (block_start ≥ 8) → touches none.
                let outside_line = 5;
                let working = lines[outside_line - 1].to_owned();
                hunks.push((
                    outside_line,
                    vec![format!("{working}  # was")],
                    vec![working],
                ));
            }
            BlockTargetKind::Stale => unreachable!("handled above"),
        }
    }
    hunks.sort_by_key(|(new_start, _, _)| *new_start);
    for (new_start, removed, added) in &hunks {
        push_hunk(patch, *new_start, *new_start, removed, added);
    }
}

/// Emit a hunk over a changed model's `.sql` whose `+` lines match the
/// model's manifest `raw_code` (cute-dbt#111). The SQL diff reconstructs
/// from `raw_code` (not disk), so for N7b to align the hunk's new side
/// must equal the model's stripped `raw_code` frame.
///
/// All three kinds edit line 2 of `raw_code` (the
/// `select * from {{ ref('upstream') }}` line):
/// - `Edit` — a real value change: `-` differs substantively, `+` ==
///   working `raw_code` line 2 ⇒ a real SQL diff.
/// - `Whitespace` — a pure re-indent: `-` differs only in leading
///   whitespace, `+` == working line 2 ⇒ no SQL diff (plain view).
/// - `Stale` — the `+` drifts from `raw_code` ⇒ N7b fails ⇒ no SQL diff.
fn emit_model_sql_hunk(
    patch: &mut String,
    manifest: &Manifest,
    changed: &str,
    project_root: &str,
    kind: &ModelSqlTargetKind,
) {
    let changed_norm = normalize_path(changed, strip_of(project_root));
    // The model node whose original_file_path matches the changed .sql.
    let raw_code = manifest
        .nodes()
        .values()
        .find(|n| {
            n.original_file_path()
                .is_some_and(|p| normalize_path(p, None) == changed_norm)
        })
        .and_then(cute_dbt::domain::Node::raw_code)
        .expect("a model-SQL-diff scenario builds a model with raw_code at the changed .sql");
    // git's line frame: raw_code with a single trailing newline stripped
    // (dbt-core ships it already stripped; the production code strips one).
    let raw = raw_code.strip_suffix('\n').unwrap_or(raw_code);
    let lines: Vec<&str> = raw.split('\n').collect();
    // Edit line 2 (1-based), the `select * from {{ ref(...) }}` line.
    let new_start = 2;
    let working = lines[new_start - 1].to_owned(); // current raw_code line 2
    let (removed, added) = match kind {
        ModelSqlTargetKind::Edit => {
            // Real change: old line referenced a different ref; new == working.
            (
                vec![working.replace("upstream", "old_upstream")],
                vec![working],
            )
        }
        ModelSqlTargetKind::Whitespace => {
            // Pure re-indent: old line had different leading whitespace; the
            // non-whitespace content is identical to working ⇒ ws_equal.
            (
                vec![format!("        {}", working.trim_start())],
                vec![working],
            )
        }
        ModelSqlTargetKind::Stale => {
            // The `+` line drifts from raw_code ⇒ N7b alignment fails.
            (
                vec![working.clone()],
                vec![format!("{working}  -- DRIFTED")],
            )
        }
    };
    push_hunk(patch, new_start, new_start, &removed, &added);
}

/// Synthesize a `git diff --unified=0` patch from `world.changed_files`,
/// writing it to a temp file and returning its path.
///
/// For a YAML file that declares tests, the working-tree YAML is written
/// under `<workdir>/<project-root>/` (= `workdir.join(changed)`, since the
/// changed path is repo-relative and mirrors the working tree) so the #69
/// slicer can compute block spans. If the file has no block-targeting
/// directives ([`World::block_targets`]) the synthesizer emits a whole-file
/// hunk spanning every declared block (note #7 — the slice-A footprint);
/// otherwise it emits block-targeted hunks (cute-dbt#96 Step 2). SQL /
/// non-dbt files get a minimal hunk and no working-tree file.
fn synthesize_pr_diff(
    manifest: &Manifest,
    world: &World,
    workdir: &Path,
    project_root: &str,
) -> PathBuf {
    let mut patch = String::new();
    // Git-rename header blocks (cute-dbt#80) — the exact shape `git diff`
    // emits with rename detection on (verified against git 2.51): a pure
    // rename is ONLY the extended headers; a rename-with-edit adds the
    // `---`/`+++` file headers and its hunks.
    for rename in &world.renames {
        let (from, to) = (&rename.from, &rename.to);
        if rename.edited {
            patch.push_str(&format!(
                "diff --git a/{from} b/{to}\nsimilarity index 76%\nrename from {from}\nrename to {to}\n--- a/{from}\n+++ b/{to}\n"
            ));
            push_hunk(
                &mut patch,
                1,
                1,
                &["old line".to_owned()],
                &["new line".to_owned()],
            );
        } else {
            patch.push_str(&format!(
                "diff --git a/{from} b/{to}\nsimilarity index 100%\nrename from {from}\nrename to {to}\n"
            ));
        }
    }
    for changed in &world.changed_files {
        let is_yaml = changed.ends_with(".yml") || changed.ends_with(".yaml");
        let tests = tests_declared_in(manifest, changed, project_root);
        let targets: Vec<BlockTargetKind> = world
            .block_targets
            .iter()
            .filter(|t| &t.yaml == changed)
            .map(|t| t.kind.clone())
            .collect();

        if is_yaml && !tests.is_empty() {
            let content = synth_yaml(&tests);
            let abs = workdir.join(changed);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).expect("create YAML parent dir");
            }
            std::fs::write(&abs, &content).expect("write working-tree YAML");

            push_file_header(&mut patch, changed);
            if targets.is_empty() {
                // Whole-file footprint: every block touched, every `+` line
                // == working-tree content (N7b-aligned), so file-granular
                // and block-level overlap coincide (slice-A scenarios).
                let lines: Vec<String> = content.lines().map(str::to_owned).collect();
                push_hunk(&mut patch, 1, 1, &lines, &lines);
            } else {
                emit_targeted_hunks(&mut patch, &content, &tests, &targets);
            }
        } else if let Some(target) = world.model_sql_targets.iter().find(|t| {
            normalize_path(&t.ofp, None) == normalize_path(changed, strip_of(project_root))
        }) {
            // cute-dbt#111: a changed model `.sql` whose model carries
            // `raw_code`. The SQL diff reads `raw_code` from the MANIFEST
            // (no working-tree file), so the synthesized hunk's `+` lines
            // must match the model's stripped `raw_code` frame.
            push_file_header(&mut patch, changed);
            emit_model_sql_hunk(&mut patch, manifest, changed, project_root, &target.kind);
        } else {
            push_file_header(&mut patch, changed);
            push_hunk(&mut patch, 1, 1, &["old".to_owned()], &["new".to_owned()]);
        }
    }
    let patch_path = common::tmp("pr_diff.patch");
    std::fs::write(&patch_path, &patch).expect("write synthesized patch");
    patch_path
}

// --- Given: the PR diff ---------------------------------------------

#[given(regex = r#"^a PR diff that changes (?:only )?"([^"]+)"$"#)]
fn pr_diff_changes_one(world: &mut World, path: String) {
    world.changed_files = vec![path];
}

#[given(regex = r#"^a PR diff that changes (?:only )?"([^"]+)" and "([^"]+)"$"#)]
fn pr_diff_changes_two(world: &mut World, a: String, b: String) {
    world.changed_files = vec![a, b];
}

// --- Given: git renames (cute-dbt#80) --------------------------------

#[given(regex = r#"^a PR diff that renames "([^"]+)" to "([^"]+)" with no content change$"#)]
fn pr_diff_pure_rename(world: &mut World, from: String, to: String) {
    world.renames.push(RenameDirective {
        from,
        to,
        edited: false,
    });
}

#[given(regex = r#"^a PR diff that renames "([^"]+)" to "([^"]+)" and edits it$"#)]
fn pr_diff_rename_with_edit(world: &mut World, from: String, to: String) {
    world.renames.push(RenameDirective {
        from,
        to,
        edited: true,
    });
}

#[given("a PR diff file whose contents are not a valid unified diff")]
fn pr_diff_malformed(world: &mut World) {
    let path = common::tmp("pr_diff_malformed.patch");
    std::fs::write(&path, "this is not a unified diff\njust some prose\n")
        .expect("write malformed patch");
    world.explicit_patch = Some(path);
}

// --- Given: block-targeting PR diffs (cute-dbt#96 Step 2) ------------
//
// These record where in a YAML file the synthesized diff places its hunks.
// The file is added to `changed_files` (so the When synthesizes it) and a
// `BlockTarget` directs hunk placement; the harness writes the full
// working-tree YAML either way, so the #69 slicer always finds block spans.

/// Mark `yaml` as a changed file (idempotent — a scenario may target the
/// same file twice, e.g. both-edited).
fn push_changed_yaml(world: &mut World, yaml: &str) {
    if !world.changed_files.iter().any(|f| f == yaml) {
        world.changed_files.push(yaml.to_owned());
    }
}

#[given(regex = r#"^a PR diff that edits the definition of "([^"]+)" in "([^"]+)"$"#)]
fn pr_diff_edits_test(world: &mut World, test: String, yaml: String) {
    push_changed_yaml(world, &yaml);
    world.block_targets.push(BlockTarget {
        yaml,
        kind: BlockTargetKind::EditsTest(test),
    });
}

#[given(
    regex = r#"^a PR diff that edits the definitions of "([^"]+)" and "([^"]+)" in "([^"]+)"$"#
)]
fn pr_diff_edits_two_tests(world: &mut World, a: String, b: String, yaml: String) {
    push_changed_yaml(world, &yaml);
    world.block_targets.push(BlockTarget {
        yaml: yaml.clone(),
        kind: BlockTargetKind::EditsTest(a),
    });
    world.block_targets.push(BlockTarget {
        yaml,
        kind: BlockTargetKind::EditsTest(b),
    });
}

#[given(regex = r#"^a PR diff that edits "([^"]+)" outside any test definition$"#)]
fn pr_diff_edits_outside(world: &mut World, yaml: String) {
    push_changed_yaml(world, &yaml);
    world.block_targets.push(BlockTarget {
        yaml,
        kind: BlockTargetKind::EditsOutside,
    });
}

#[given(regex = r#"^a PR diff that deletes lines from the definition of "([^"]+)" in "([^"]+)"$"#)]
fn pr_diff_deletes_from_test(world: &mut World, test: String, yaml: String) {
    push_changed_yaml(world, &yaml);
    world.block_targets.push(BlockTarget {
        yaml,
        kind: BlockTargetKind::DeletesFromTest(test),
    });
}

#[given(regex = r#"^a PR diff whose hunks no longer line up with "([^"]+)"$"#)]
fn pr_diff_stale(world: &mut World, yaml: String) {
    push_changed_yaml(world, &yaml);
    world.block_targets.push(BlockTarget {
        yaml,
        kind: BlockTargetKind::Stale,
    });
}

// --- Given: model-SQL-diff PR diffs (cute-dbt#111) ------------------
//
// These mark a model's `.sql` changed and record how the synthesized hunk
// edits the model's manifest `raw_code`. The model is built with `raw_code`
// (via `manifest_contains_model_with_sql`), and the synthesizer matches the
// hunk's `+` lines to that `raw_code` so N7b aligns.

/// Mark `sql` as a changed file (idempotent).
fn push_changed_sql(world: &mut World, sql: &str) {
    if !world.changed_files.iter().any(|f| f == sql) {
        world.changed_files.push(sql.to_owned());
    }
}

#[given(regex = r#"^a PR diff that changes the SQL of "([^"]+)"$"#)]
fn pr_diff_changes_model_sql(world: &mut World, sql: String) {
    push_changed_sql(world, &sql);
    world.model_sql_targets.push(ModelSqlTarget {
        ofp: sql,
        kind: ModelSqlTargetKind::Edit,
    });
}

#[given(regex = r#"^a PR diff that re-indents the SQL of "([^"]+)" \(whitespace only\)$"#)]
fn pr_diff_reindents_model_sql(world: &mut World, sql: String) {
    push_changed_sql(world, &sql);
    world.model_sql_targets.push(ModelSqlTarget {
        ofp: sql,
        kind: ModelSqlTargetKind::Whitespace,
    });
}

#[given(regex = r#"^a PR diff whose SQL hunks no longer line up with "([^"]+)"$"#)]
fn pr_diff_stale_model_sql(world: &mut World, sql: String) {
    push_changed_sql(world, &sql);
    world.model_sql_targets.push(ModelSqlTarget {
        ofp: sql,
        kind: ModelSqlTargetKind::Stale,
    });
}

#[given(regex = r#"^the manifest contains a model with raw SQL at "([^"]+)"$"#)]
fn manifest_contains_model_with_sql(world: &mut World, ofp: String) {
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        super::builders::model_node_with_raw_code(
            &bare,
            "ck-sql",
            Some(COMPILED_WITH_CTE),
            &ofp,
            super::builders::MODEL_SQL_RAW_CODE,
        ),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

// --- Given: synthetic manifest construction -------------------------

#[given(regex = r#"^the manifest contains a model with original_file_path "([^"]+)"$"#)]
fn manifest_contains_model(world: &mut World, ofp: String) {
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(&bare, "ck-pr", Some(COMPILED_WITH_CTE), &ofp),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

#[given(
    regex = r#"^the manifest contains a model with original_file_path "([^"]+)" and schema file "([^"]+)"$"#
)]
fn manifest_contains_model_with_schema(world: &mut World, ofp: String, patch_path: String) {
    // cute-dbt#413 — the model carries a `patch_path` (its schema.yml), so a
    // diff that touches that file fires the `config` change-axis for the
    // model. The bare name is derived from the .sql stem as elsewhere.
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_ofp_and_patch_path(
            &bare,
            "ck-pr",
            Some(COMPILED_WITH_CTE),
            &ofp,
            &patch_path,
        ),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

#[given(regex = r#"^the manifest contains a model with no compiled SQL at "([^"]+)"$"#)]
fn manifest_contains_uncompiled_model(world: &mut World, ofp: String) {
    // compiled_code: null — Stage-2 fail-closes when this model is the
    // target of an in-scope unit test.
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(&bare, "ck-pr", None, &ofp),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

#[given(
    regex = r#"^the manifest \(compiled with project root "([^"]+)"\) contains a model with original_file_path "([^"]+)"$"#
)]
fn manifest_subdir_model(world: &mut World, _project_root: String, ofp: String) {
    // The manifest's original_file_path is project-relative; the changed
    // path is repo-relative (carries the project-root prefix). The
    // `--project-root` flag in the When step bridges the two. The
    // parenthetical project-root note is descriptive only here.
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(&bare, "ck-pr", Some(COMPILED_WITH_CTE), &ofp),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

#[given(regex = r#"^the model "([^"]+)" has a unit test "([^"]+)"$"#)]
fn model_has_unit_test(world: &mut World, model_bare: String, test: String) {
    let manifest = take_current(world);
    let manifest = with_unit_test(manifest, unit_test_for(&test, &model_bare));
    world.current_manifest = Some(manifest);
}

#[given(regex = r#"^the model "([^"]+)" has a unit test "([^"]+)" declared in "([^"]+)"$"#)]
fn model_has_unit_test_declared_in(
    world: &mut World,
    model_bare: String,
    test: String,
    ofp: String,
) {
    let manifest = take_current(world);
    let manifest = with_unit_test(manifest, unit_test_with_path(&test, &model_bare, &ofp));
    world.current_manifest = Some(manifest);
}

#[given(
    regex = r#"^the manifest also contains an unchanged model "([^"]+)" with a unit test "([^"]+)"$"#
)]
fn manifest_unchanged_model_with_test(world: &mut World, model_bare: String, test: String) {
    // "Unchanged" = its on-disk path is NOT in the changed-files set, so
    // it stays out of scope. A plausible staging path keeps it distinct
    // from the changed marts models.
    let ofp = format!("models/staging/{model_bare}.sql");
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(
            &model_bare,
            "ck-unchanged",
            Some(COMPILED_WITH_CTE),
            &ofp,
        ),
    );
    let manifest = with_unit_test(manifest, unit_test_for(&test, &model_bare));
    world.current_manifest = Some(manifest);
}

#[given(regex = r#"^the manifest has no node with original_file_path "([^"]+)"$"#)]
fn manifest_has_no_node(world: &mut World, _ofp: String) {
    // Intentionally builds nothing — the changed path maps to no node.
    // Ensure a valid (possibly empty) manifest exists for the When step.
    let manifest = take_current(world);
    world.current_manifest = Some(manifest);
}

// --- When: run the subprocess ---------------------------------------

fn capture(world: &mut World, output: std::process::Output, out: std::path::PathBuf) {
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --pr-diff @diff\.patch --project-root (\S+) --out report\.html$"#
)]
fn run_pr_diff(world: &mut World, project_root: String) {
    let manifest = take_current(world);
    let manifest_path = serialize_to_tmp(&manifest, "pr_diff_current");

    let out = common::tmp("pr_diff_report.html");
    common::clear(&out);

    // `--project-root` is existence-validated by clap (cute-dbt#69), so the
    // sub-directory project-root scenario needs the directory to exist on
    // disk. Run from a temp workdir and create the project-root sub-dir
    // there; `--manifest`/`--out` are absolute, so cwd doesn't affect them.
    // Create `workdir` itself first; only join the sub-dir for a non-"."
    // root — `workdir.join(".")` builds a `…/workdir/.` path whose trailing
    // `.` component makes `create_dir_all` fail with NotFound on Linux
    // (macOS tolerates it), which is why this passed locally but not in CI.
    let workdir = common::tmp("pr_diff_workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    if project_root != "." {
        std::fs::create_dir_all(workdir.join(&project_root)).expect("create project-root dir");
    }

    // Synthesize the patch (+ the working-tree YAML it references) from
    // the changed-file Givens, unless a Given wrote an explicit patch
    // (the malformed-diff case — and, cute-dbt#96 Step 2, the stale-diff
    // case).
    let patch_path = world
        .explicit_patch
        .clone()
        .unwrap_or_else(|| synthesize_pr_diff(&manifest, world, &workdir, &project_root));
    world.current_manifest = Some(manifest);
    let scope_arg = format!("@{}", common::s(&patch_path));

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args([
            "report",
            "--manifest",
            common::s(&manifest_path),
            "--pr-diff",
            &scope_arg,
            "--project-root",
            &project_root,
            "--out",
            common::s(&out),
        ])
        .current_dir(&workdir)
        .output()
        .expect("the cute-dbt binary spawns");
    capture(world, output, out);
}

/// cute-dbt#346 — the change-context banner PR link. Same as `run_pr_diff`
/// but also passes `--pr-url <url> --pr-title <title>`; the banner then
/// renders the linked `PR #<n> — <title>` clause.
#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --pr-diff @diff\.patch --project-root \. --pr-url "([^"]+)" --pr-title "([^"]*)" --out report\.html$"#
)]
fn run_pr_diff_with_pr_link(world: &mut World, pr_url: String, pr_title: String) {
    let manifest = take_current(world);
    let manifest_path = serialize_to_tmp(&manifest, "pr_diff_link_current");

    let out = common::tmp("pr_diff_link_report.html");
    common::clear(&out);

    let workdir = common::tmp("pr_diff_link_workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");

    let patch_path = world
        .explicit_patch
        .clone()
        .unwrap_or_else(|| synthesize_pr_diff(&manifest, world, &workdir, "."));
    world.current_manifest = Some(manifest);
    let scope_arg = format!("@{}", common::s(&patch_path));

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args([
            "report",
            "--manifest",
            common::s(&manifest_path),
            "--pr-diff",
            &scope_arg,
            "--project-root",
            ".",
            "--pr-url",
            &pr_url,
            "--pr-title",
            &pr_title,
            "--out",
            common::s(&out),
        ])
        .current_dir(&workdir)
        .output()
        .expect("the cute-dbt binary spawns");
    capture(world, output, out);
}

#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --baseline-manifest baseline\.json --pr-diff @diff\.patch --project-root \. --out report\.html$"#
)]
fn run_both_scope_sources(world: &mut World) {
    // clap rejects the conflicting scope sources at parse time. The
    // --pr-diff value must still PARSE (value-parser runs before group
    // validation), so synthesize a valid patch; the baseline path need
    // not exist (it is never read).
    let manifest = take_current(world);
    let manifest_path = serialize_to_tmp(&manifest, "pr_diff_both");

    let baseline = common::tmp("pr_diff_baseline_unread.json");
    let out = common::tmp("pr_diff_both_report.html");
    common::clear(&out);
    let workdir = common::tmp("pr_diff_both_workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    let patch_path = synthesize_pr_diff(&manifest, world, &workdir, ".");
    world.current_manifest = Some(manifest);
    let scope_arg = format!("@{}", common::s(&patch_path));

    let output = common::run_cli(&[
        "report",
        "--manifest",
        common::s(&manifest_path),
        "--baseline-manifest",
        common::s(&baseline),
        "--pr-diff",
        &scope_arg,
        "--project-root",
        ".",
        "--out",
        common::s(&out),
    ]);
    capture(world, output, out);
}

#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --project-root \. --out report\.html$"#
)]
fn run_no_scope_source(world: &mut World) {
    // Neither scope source — clap's required `scope_source` group fails.
    let manifest = take_current(world);
    let manifest_path = serialize_to_tmp(&manifest, "pr_diff_neither");
    world.current_manifest = Some(manifest);

    let out = common::tmp("pr_diff_neither_report.html");
    common::clear(&out);
    let output = common::run_cli(&[
        "report",
        "--manifest",
        common::s(&manifest_path),
        "--project-root",
        ".",
        "--out",
        common::s(&out),
    ]);
    capture(world, output, out);
}

// --- Then: payload-based assertions ---------------------------------

/// Parse the embedded `cute-dbt-data` JSON payload from the rendered
/// report (mirrors `consumer_report_contract.rs`).
fn payload(world: &World) -> Value {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report must include <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    serde_json::from_str(&node.inner_text(parser)).expect("embedded payload must be valid JSON")
}

/// Bare model names present in the rendered payload (`models[].name`).
fn model_names(payload: &Value) -> Vec<String> {
    payload["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Every unit-test name in the rendered payload (`models[].tests[].name`).
fn test_names(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(models) = payload["models"].as_array() {
        for model in models {
            if let Some(tests) = model["tests"].as_array() {
                for test in tests {
                    if let Some(name) = test["name"].as_str() {
                        out.push(name.to_owned());
                    }
                }
            }
        }
    }
    out
}

fn require_exit_0(world: &World) {
    assert_eq!(
        world.last_exit_code,
        Some(0),
        "cute-dbt failed; stderr={}",
        world.last_stderr,
    );
}

#[then(regex = r#"^the rendered report's models-in-scope listing contains "([^"]+)"$"#)]
fn models_listing_contains(world: &mut World, name: String) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.contains(&name),
        "expected model {name:?} in scope; got {names:?}",
    );
}

#[then(
    regex = r#"^the rendered report's models-in-scope listing contains both "([^"]+)" and "([^"]+)"$"#
)]
fn models_listing_contains_both(world: &mut World, a: String, b: String) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.contains(&a) && names.contains(&b),
        "expected both {a:?} and {b:?} in scope; got {names:?}",
    );
}

#[then(regex = r#"^the rendered report's models-in-scope listing does NOT contain "([^"]+)"$"#)]
fn models_listing_excludes(world: &mut World, name: String) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        !names.contains(&name),
        "{name:?} should be out of scope; got {names:?}",
    );
}

#[then(regex = r#"^the rendered report contains a CTE diagram for "([^"]+)"$"#)]
fn cte_diagram_for(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = p["models"]
        .as_array()
        .and_then(|models| {
            models
                .iter()
                .find(|m| m["name"].as_str() == Some(name.as_str()))
        })
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    let dag_nodes = model["dag"]["nodes"].as_array().map_or(0, Vec::len);
    assert!(
        dag_nodes > 0,
        "expected a non-empty CTE DAG for {name:?}; got {dag_nodes} nodes",
    );
}

#[then(regex = r#"^the rendered report's test rows include "([^"]+)"$"#)]
fn test_rows_include(world: &mut World, test: String) {
    require_exit_0(world);
    let names = test_names(&payload(world));
    assert!(
        names.contains(&test),
        "expected test {test:?} in the rendered report; got {names:?}",
    );
}

#[then(regex = r#"^the rendered report does NOT contain a test row for "([^"]+)"$"#)]
fn no_test_row_for(world: &mut World, test: String) {
    require_exit_0(world);
    let names = test_names(&payload(world));
    assert!(
        !names.contains(&test),
        "{test:?} should not render; got {names:?}",
    );
}

// --- cute-dbt#91: updated-vs-context classification (payload-asserted) ---

/// Find a test object by `name` across every model's `tests` array.
///
/// Fails loud on an ambiguous match: if two rendered models ever carry a
/// test with the same `name`, returning the first would silently assert
/// against the wrong row, so this panics instead (CodeRabbit on #97).
/// Today's scenarios put all of a scenario's tests on one model, so a
/// match is unique; the guard protects future multi-model scenarios.
fn find_test<'p>(payload: &'p Value, name: &str) -> Option<&'p Value> {
    let matches: Vec<&Value> = payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["tests"].as_array())
        .flatten()
        .filter(|t| t["name"].as_str() == Some(name))
        .collect();
    match matches.as_slice() {
        [] => None,
        [test] => Some(*test),
        _ => panic!(
            "test name {name:?} is ambiguous across models — key the BDD lookup by id or model+name"
        ),
    }
}

#[then(regex = r#"^the test "([^"]+)" is marked updated$"#)]
fn test_marked_updated(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let test = find_test(&p, &name)
        .unwrap_or_else(|| panic!("test {name:?} not in payload; got {:?}", test_names(&p)));
    assert_eq!(
        test["changed"].as_bool(),
        Some(true),
        "test {name:?} should be marked updated (changed:true); got {test:?}",
    );
}

#[then(regex = r#"^the test "([^"]+)" is marked context$"#)]
fn test_marked_context(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let test = find_test(&p, &name)
        .unwrap_or_else(|| panic!("test {name:?} not in payload; got {:?}", test_names(&p)));
    assert_eq!(
        test["changed"].as_bool(),
        Some(false),
        "test {name:?} should be marked context (changed:false); got {test:?}",
    );
}

#[then(
    regex = r#"^the test "([^"]+)" carries an inline YAML diff with a removed and an added line$"#
)]
fn test_carries_inline_diff(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let test = find_test(&p, &name)
        .unwrap_or_else(|| panic!("test {name:?} not in payload; got {:?}", test_names(&p)));
    let lines = test["yaml_diff"]["lines"].as_array().unwrap_or_else(|| {
        panic!("test {name:?} should carry a yaml_diff with lines; got {test:?}")
    });
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"removed"),
        "yaml_diff for {name:?} should include a removed line; got kinds {kinds:?}",
    );
    assert!(
        kinds.contains(&"added"),
        "yaml_diff for {name:?} should include an added line; got kinds {kinds:?}",
    );
}

#[then(regex = r#"^the test "([^"]+)" carries no inline YAML diff$"#)]
fn test_carries_no_inline_diff(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let test = find_test(&p, &name)
        .unwrap_or_else(|| panic!("test {name:?} not in payload; got {:?}", test_names(&p)));
    // `skip_serializing_if` omits the key entirely when there is no diff
    // (absent block / stale / untouched), so the drawer falls back to the
    // plain authored YAML.
    assert!(
        test.get("yaml_diff").is_none_or(Value::is_null),
        "test {name:?} should carry no yaml_diff; got {:?}",
        test.get("yaml_diff"),
    );
}

// --- cute-dbt#111: model SQL diff (payload-asserted) ----------------

/// Find a model object by `name` in the rendered payload.
fn find_model<'p>(payload: &'p Value, name: &str) -> Option<&'p Value> {
    payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"].as_str() == Some(name))
}

// --- cute-dbt#413: per-model change-axis attribution (payload-asserted) ---

/// Assert the model's `axes` payload object carries EXACTLY the named axes
/// set to `true` (and the others `false`). `axes` is a comma-separated
/// list of `body` / `config` / `unit_test`. The render payload only carries
/// `axes` in `--pr-diff` mode, so this also pins that the attribution
/// threaded all the way through to the report.
#[then(regex = r#"^the model "([^"]+)" is attributed to the axes "([^"]+)"$"#)]
fn model_attributed_to_axes(world: &mut World, name: String, axes_csv: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = find_model(&p, &name)
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    let axes = model["axes"].as_object().unwrap_or_else(|| {
        panic!("model {name:?} must carry an axes object in pr-diff mode; got {model:?}")
    });
    let expected: std::collections::BTreeSet<&str> = axes_csv.split(',').map(str::trim).collect();
    for axis in ["body", "config", "unit_test"] {
        let fired = axes.get(axis).and_then(Value::as_bool).unwrap_or_else(|| {
            panic!("axes object for {name:?} is missing key {axis:?}; got {axes:?}")
        });
        let should_fire = expected.contains(axis);
        assert_eq!(
            fired, should_fire,
            "model {name:?} axis {axis:?}: expected {should_fire}, got {fired} (full axes {axes:?})",
        );
    }
}

/// Assert the model's `config_file` payload field names the given
/// schema.yml — the optgroup grouping key + the non-interactive
/// config-file chip source.
#[then(regex = r#"^the model "([^"]+)" carries the config file "([^"]+)"$"#)]
fn model_carries_config_file(world: &mut World, name: String, config_file: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = find_model(&p, &name)
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    assert_eq!(
        model["config_file"].as_str(),
        Some(config_file.as_str()),
        "model {name:?} should carry config_file {config_file:?}; got {model:?}",
    );
}

#[then(
    regex = r#"^the model "([^"]+)" carries an inline SQL diff with a removed and an added line$"#
)]
fn model_carries_sql_diff(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = find_model(&p, &name)
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    let lines = model["sql_diff"]["lines"].as_array().unwrap_or_else(|| {
        panic!("model {name:?} should carry a sql_diff with lines; got {model:?}")
    });
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["kind"].as_str()).collect();
    // Content + ORDER oracle (not just presence): the synthesized Edit hunk
    // rewrites raw_code line 2 (`select * from {{ ref('upstream') }}`), so the
    // diff must show a Removed line (the old `..old_upstream..` body)
    // IMMEDIATELY followed by the Added working-tree line — a flipped
    // removed↔added or a stray reconstruction would fail here.
    let removed_idx = lines.iter().position(|l| l["kind"] == "removed");
    let added_idx = lines.iter().position(|l| l["kind"] == "added");
    let (Some(ri), Some(ai)) = (removed_idx, added_idx) else {
        panic!("sql_diff for {name:?} must carry a removed AND an added line; got kinds {kinds:?}");
    };
    assert_eq!(
        ai,
        ri + 1,
        "the added line must immediately follow the removed line (change pair); got kinds {kinds:?}",
    );
    let removed_text = lines[ri]["text"].as_str().unwrap_or_default();
    let added_text = lines[ai]["text"].as_str().unwrap_or_default();
    assert!(
        removed_text.contains("old_upstream"),
        "the removed line is the pre-edit `..old_upstream..` body; got {removed_text:?}",
    );
    assert!(
        added_text.contains("ref('upstream')") && !added_text.contains("old_upstream"),
        "the added line is the working-tree raw_code line; got {added_text:?}",
    );
    // And the surrounding raw_code lines are Context (line 1 `with src as (`).
    assert!(
        lines.iter().any(|l| l["kind"] == "context"
            && l["text"]
                .as_str()
                .is_some_and(|t| t.contains("with src as"))),
        "the unchanged raw_code lines render as Context; got {kinds:?}",
    );
}

#[then(regex = r#"^the model "([^"]+)" carries no inline SQL diff$"#)]
fn model_carries_no_sql_diff(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = find_model(&p, &name)
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    // `skip_serializing_if` omits the key entirely when there is no diff
    // (in-scope-via-test-only, stale, whitespace-only) → plain SQL view.
    assert!(
        model.get("sql_diff").is_none_or(Value::is_null),
        "model {name:?} should carry no sql_diff; got {:?}",
        model.get("sql_diff"),
    );
}

#[then(regex = r#"^the model "([^"]+)" carries (\d+) unit tests$"#)]
fn model_carries_n_tests(world: &mut World, name: String, n: usize) {
    require_exit_0(world);
    let p = payload(world);
    let model = p["models"]
        .as_array()
        .and_then(|models| {
            models
                .iter()
                .find(|m| m["name"].as_str() == Some(name.as_str()))
        })
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    let count = model["tests"].as_array().map_or(0, Vec::len);
    assert_eq!(
        count, n,
        "model {name:?} should carry {n} unit tests; got {count}",
    );
}

#[then(
    regex = r#"^the rendered report shows "([^"]+)" with the "no unit tests wired" empty state$"#
)]
fn model_empty_state(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = p["models"]
        .as_array()
        .and_then(|models| {
            models
                .iter()
                .find(|m| m["name"].as_str() == Some(name.as_str()))
        })
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    let tests = model["tests"].as_array().map_or(0, Vec::len);
    assert_eq!(
        tests, 0,
        "expected {name:?} to show the no-unit-tests-wired empty state (zero in-scope tests)",
    );
}

#[then(regex = r#"^the rendered report shows the "0 unit tests in scope" banner$"#)]
fn zero_scope_banner(world: &mut World) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.is_empty(),
        "expected zero models in scope; got {names:?}",
    );
}

#[then("the rendered report contains no CTE diagrams")]
fn no_cte_diagrams(world: &mut World) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.is_empty(),
        "expected no models (hence no CTE diagrams); got {names:?}",
    );
}

/// cute-dbt#346 — the change-context banner links the PR number to its url
/// and shows the title beside it.
#[then(regex = r#"^the change-context banner links to "([^"]+)" as "PR #(\d+)"$"#)]
fn banner_links_pr(world: &mut World, url: String, number: String) {
    require_exit_0(world);
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let needle = format!(r#"<a class="diff-scope-pr-link" href="{url}">PR #{number}</a>"#);
    assert!(
        html.contains(&needle),
        "expected the banner to carry {needle:?}",
    );
}

/// The PR title renders as escaped text — a `<` in the title must not
/// survive as a raw tag opener.
#[then(regex = r#"^the banner shows the title "([^"]+)" as escaped text$"#)]
fn banner_shows_escaped_title(world: &mut World, raw_title: String) {
    require_exit_0(world);
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    // Negative: a `<`-bearing title must NOT survive verbatim — it would be
    // a raw tag opener; askama must have escaped it.
    if raw_title.contains('<') {
        assert!(
            !html.contains(&raw_title),
            "the raw title {raw_title:?} must be escaped, not rendered verbatim",
        );
    }
    // Positive (cute-dbt#363 review): the title must actually be *rendered*,
    // so a dropped `{{ pr.title }}` (or an omitted banner) fails this too.
    // Check the longest run of non-escapable chars — it survives verbatim,
    // which avoids replicating askama's exact escaping.
    let longest_unescaped = raw_title
        .split(['<', '>', '&', '"', '\''])
        .max_by_key(|seg| seg.len())
        .unwrap_or("")
        .trim();
    if !longest_unescaped.is_empty() {
        assert!(
            html.contains(longest_unescaped),
            "expected the banner to render the title segment {longest_unescaped:?}",
        );
    }
}

#[then("the change-context banner shows no PR link")]
fn banner_shows_no_pr_link(world: &mut World) {
    require_exit_0(world);
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    assert!(
        !html.contains("diff-scope-pr-link"),
        "expected no PR link in the banner",
    );
}

#[then("stderr explains exactly one of --pr-diff or --baseline-manifest must be provided")]
fn stderr_explains_one_scope_source(world: &mut World) {
    let stderr = &world.last_stderr;
    assert!(
        stderr.contains("--pr-diff") && stderr.contains("--baseline-manifest"),
        "expected stderr to name both scope-source flags; got: {stderr}",
    );
}

#[then("the exit code is 2")]
fn exit_code_is_2(world: &mut World) {
    assert_eq!(
        world.last_exit_code,
        Some(2),
        "expected exit 2 (clap usage error); stderr={}",
        world.last_stderr,
    );
}

#[then("stderr explains the --pr-diff argument could not be parsed as a unified diff")]
fn stderr_explains_malformed_pr_diff(world: &mut World) {
    let stderr = &world.last_stderr;
    assert!(
        stderr.contains("--pr-diff") && stderr.contains("could not be parsed as a unified diff"),
        "expected stderr to explain the unparseable --pr-diff; got: {stderr}",
    );
}

#[then(
    regex = r#"^stderr names "([^"]+)" as the offending node and recommends running "([^"]+)"$"#
)]
fn stderr_names_offending_node(world: &mut World, node: String, command: String) {
    let stderr = &world.last_stderr;
    assert!(
        stderr.contains(&node),
        "expected stderr to name the offending node {node:?}; got: {stderr}",
    );
    assert!(
        stderr.contains(&command),
        "expected stderr to recommend {command:?}; got: {stderr}",
    );
}
