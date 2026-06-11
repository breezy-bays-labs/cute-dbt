# Maps: NEW v0.1.x capability — `--pr-diff` CLI flag
# Pipeline: cute-dbt-20260527-team-pr-review-ergonomics (Shape E, Phase 1)
# Companion to features/diff_scoping.feature (baseline-manifest path)
#
# `--pr-diff @diff.patch` takes a raw `git diff --unified=0` patch (renamed
# from `--scope-from-pr-diff` at cute-dbt#96, which took a changed-file
# list). `@diff.patch` in a When clause is a fixed token: the harness
# SYNTHESIZES the patch (and the working-tree YAML it references) from the
# prior `Given a PR diff that changes …` step — generating both together so
# the diff and the file are revision-aligned. For a YAML file that declares
# tests the synthesized hunk spans every declared block (whole-file
# footprint), so file-level and block-level overlap coincide for these
# migrated scenarios; cute-dbt#96 Step 2 adds block-targeting Givens.
#
# Manifest construction: every model / unit test a scenario needs is
# built in-memory by its own Given steps (no committed fixture files —
# the synthetic-only-fixture invariant is satisfied trivially). A bare
# model name is derived from the `.sql` file stem of its
# original_file_path (`models/marts/core/dim_payers.sql` → `dim_payers`).
Feature: Diff-scope unit tests and models via PR file diff (CI path)
  As an analytics engineer setting up cute-dbt in a PR-review workflow
  I want cute-dbt to derive in-scope models from the PR's file diff
  So that no baseline-manifest publishing job is needed in CI

  Background:
    Given a compiled dbt 1.8+ manifest "current.json" with unit tests

  # --- Happy paths ---

  Scenario: A PR that changes one model file puts that model in scope
    Given a PR diff that changes "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the rendered report contains a CTE diagram for "dim_payers"
    And the rendered report's test rows include "test_dim_payers_injects_unknown_sentinel"

  Scenario: A PR that changes only unit-test YAML puts that test in scope
    Given a PR diff that changes "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's test rows include "test_dim_payers_injects_unknown_sentinel"

  Scenario: A PR with multiple changed files puts all matching nodes in scope
    Given a PR diff that changes "models/marts/core/dim_payers.sql" and "models/marts/analytics/mart_dq_summary.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/analytics/mart_dq_summary.sql"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains both "dim_payers" and "mart_dq_summary"
    And the rendered report contains a CTE diagram for "dim_payers"
    And the rendered report contains a CTE diagram for "mart_dq_summary"

  # CPO finding — proves SCOPING, not just selection.
  Scenario: An unchanged sibling model is NOT in the rendered report
    Given a PR diff that changes "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the manifest also contains an unchanged model "stg_customers" with a unit test "test_stg_customers_unique"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the rendered report's models-in-scope listing does NOT contain "stg_customers"
    And the rendered report does NOT contain a test row for "test_stg_customers_unique"

  Scenario: A modified model with zero unit tests is in models_in_scope (explorer mode)
    Given a PR diff that changes "models/staging/stg_payments.sql"
    And the manifest contains a model with original_file_path "models/staging/stg_payments.sql"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows "stg_payments" with the "no unit tests wired" empty state

  # --- Project-root path rewriting (load-bearing for the Action wrapper) ---

  Scenario: --project-root rewrites PR-diff paths so a sub-directory dbt project is in scope
    Given a PR diff that changes "dbt_project/models/marts/core/dim_payers.sql"
    And the manifest (compiled with project root "dbt_project") contains a model with original_file_path "models/marts/core/dim_payers.sql"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root dbt_project --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"

  # --- Zero-scope and non-mapping paths ---

  Scenario: A PR with no dbt-relevant changes produces an empty in-scope report
    Given a PR diff that changes only "README.md" and ".github/workflows/ci.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner
    And the rendered report contains no CTE diagrams

  Scenario: A changed path that doesn't map to any manifest node is silently skipped
    Given a PR diff that changes "models/deleted_model.sql"
    And the manifest has no node with original_file_path "models/deleted_model.sql"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner

  # --- Renamed models (cute-dbt#80) ---
  # `git diff` detects renames by default (since git 2.9) and emits a
  # `rename from`/`rename to` header pair. A PURE rename (100% similarity)
  # carries no `+++ b/` header and no hunks — before cute-dbt#80 it scoped
  # nothing. cute-dbt now maps BOTH rename sides onto the scope match; the
  # current manifest (compiled at the PR head) resolves the new path to the
  # renamed node, so the model scopes under its new name.

  Scenario: A pure model rename puts the renamed model in scope under its new name
    Given a PR diff that renames "models/marts/core/dim_payers.sql" to "models/marts/core/payer_dimensions.sql" with no content change
    And the manifest contains a model with original_file_path "models/marts/core/payer_dimensions.sql"
    And the model "payer_dimensions" has a unit test "test_payer_dimensions_injects_unknown_sentinel"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "payer_dimensions"
    And the rendered report's models-in-scope listing does NOT contain "dim_payers"
    And the rendered report's test rows include "test_payer_dimensions_injects_unknown_sentinel"

  Scenario: A rename with edits puts the renamed model in scope under its new name
    Given a PR diff that renames "models/marts/core/dim_payers.sql" to "models/marts/core/payer_dimensions.sql" and edits it
    And the manifest contains a model with original_file_path "models/marts/core/payer_dimensions.sql"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "payer_dimensions"

  # --- Malformed input (clap usage error; cute-dbt#96) ---

  Scenario: A malformed PR diff is a usage error
    Given a PR diff file whose contents are not a valid unified diff
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 2
    And no file "report.html" is written
    And stderr explains the --pr-diff argument could not be parsed as a unified diff

  # --- Mutual exclusivity at the clap usage layer ---

  Scenario: Passing both --pr-diff and --baseline-manifest is a clap usage error
    Given a baseline manifest "baseline.json"
    And a PR diff that changes "models/marts/core/dim_payers.sql"
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains exactly one of --pr-diff or --baseline-manifest must be provided

  @no-baseline-usage-error
  Scenario: Passing neither --pr-diff nor --baseline-manifest is a clap usage error
    When I run cute-dbt report with --manifest current.json --project-root . --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains exactly one of --pr-diff or --baseline-manifest must be provided

  # --- Fail-closed contract (Stage 2 unchanged) ---

  Scenario: An in-scope model with compiled_code:null fail-closes with NotCompiled
    Given a PR diff that changes "models/marts/core/dim_payers.sql"
    And the manifest contains a model with no compiled SQL at "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr names "dim_payers" as the offending node and recommends running "dbt compile"

  # --- v0.1 fidelity limits (documented behavior, not defects) ---

  Scenario: A change to only the YAML config block is NOT in scope via PR-diff (documented limit)
    # PR-diff sees only the YAML file path; cute-dbt does not parse the YAML
    # diff to distinguish a config-block change from a unit_tests-block change.
    # Adopters needing config-aware scoping use --baseline-manifest instead.
    Given a PR diff that changes only "dbt_project.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner

  Scenario: A `dbt deps` change that updates package checksums is NOT in scope via PR-diff (documented limit)
    # PR-diff sees only packages.yml change; no dbt SQL files change.
    # Adopters needing dependency-aware scoping use --baseline-manifest instead.
    Given a PR diff that changes only "packages.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner

  # --- Source-of-diff: @file argument (CI path) ---
  #
  # Every scenario here passes `--pr-diff @diff.patch` (the workflow writes
  # the patch to a file). The `@file` read + the diff parse (which paths
  # survive, hunk extraction, malformed/empty handling) are pinned by the
  # unit suite in `cli::pr_diff` + the exit-contract suite in
  # `tests/changed_files_provider.rs`; scope-selection correctness (which
  # models land in scope) is pinned here + by `src/domain/scope.rs`.

  # --- Path-normalization mutation kill ---
  # The file-path → manifest-node mapping must handle leading "./" and
  # path-separator normalization (Windows-style separators are NOT
  # supported in v0.1; document and unit-test in Rust).
  # `tests/path_matching.rs` is the Rust unit suite covering the exact-match
  # / leading-"./" / project-root-strip cases (cute-dbt#81). The BDD asserts
  # the user-visible behavior (model X appears in scope); the Rust suite
  # kills mutants on the path-matching function.

  # --- cute-dbt#91 (slice A): foreground updated unit tests + toggle ---
  #
  # Classification rides on the existing in-scope selection — selection is
  # unchanged; each in-scope test is additionally labeled updated vs context,
  # and the report foregrounds the updated ones. In PR-diff mode "updated" is
  # file-granular here (a changed YAML marks every test it declares as
  # updated); slice B (cute-dbt#96) makes this block-precise via diff-hunk
  # overlap. Baseline-mode precise classification is covered by the
  # `changed_unit_tests` / `test_changed` unit tests in src/domain/state.rs
  # plus the `changed ⊆ in_scope` tests in src/domain/{state,scope}.rs.

  Scenario: A test whose declaring YAML changed is marked updated
    Given a PR diff that changes "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_dim_payers_injects_unknown_sentinel" is marked updated

  Scenario: A test in scope only because its model's SQL changed is marked context
    Given a PR diff that changes "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_dim_payers_injects_unknown_sentinel" is marked context

  Scenario: A changed model carries its tests with updated/context marks
    # dim_payers.sql changed (model in scope; its tests are context unless their
    # own YAML changed); test_a's YAML changed (updated); test_b's YAML untouched
    # (context). The payload carries both → the toggle-dependent count
    # (1 updated / 2 total) is derivable in JS.
    Given a PR diff that changes "models/marts/core/dim_payers.sql" and "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_extra_tests.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_a" is marked updated
    And the test "test_b" is marked context
    And the model "dim_payers" carries 2 unit tests

  # The interactive layer — the global "Updated only ↔ All tests" toggle, the
  # default-visibility, the toggle-dependent model-selector count, and the
  # 0-updated inline hint — is JS over the inlined payload. It is verified by
  # tests/headless_toggle.rs in a real browser, NOT as cucumber payload
  # scenarios (those would need a browser). The payload facts the interactive
  # layer relies on (per-test `changed`, all-tests-per-model present) are
  # pinned by the scenarios here.

  Scenario: A changed model with no updated tests is in scope with all tests marked context
    # The common SQL-only PR: model edited, no test YAML touched → every test is
    # context (0 updated). The model is still in scope and carries its tests (it
    # shows selectable with count (0) in Updated mode — verified at headless).
    Given a PR diff that changes "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the test "test_a" is marked context
    And the test "test_b" is marked context

  Scenario: A model in scope only via a changed test still carries its non-updated siblings
    # Render-all widening: dim_payers.sql is NOT changed; the model is in scope
    # only because test_a's YAML changed. test_b (declared in an untouched YAML)
    # is carried into the report so All-tests mode + the total count work — but
    # it is marked context (non-updated).
    Given a PR diff that changes "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_other_unchanged.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's test rows include "test_b"
    And the test "test_a" is marked updated
    And the test "test_b" is marked context

  # --- cute-dbt#96 (slice B): block-precise `updated` via diff-hunk overlap ---
  #
  # Slice A marked `updated` file-granular: a changed multi-test YAML marked
  # EVERY test it declares. Slice B narrows that to block precision — a test is
  # `updated` iff a changed diff hunk overlaps its YAML block span. The harness
  # places hunks at specific blocks (computing each block's line range from the
  # synthesized YAML layout, which mirrors the #69 slicer's spans). When the
  # diff has drifted from the working tree (hunks no longer line up), cute-dbt
  # degrades to the slice-A file-granular label rather than misclassify. The
  # interior-hunk arithmetic (exact edges, off-by-one, zero-count point-touch)
  # is mutation-killed by the `hunk_touches_block` / `block_aligns_with_hunks`
  # boundary tables in src/domain/pr_diff.rs; these scenarios pin the
  # user-visible updated/context outcome end-to-end.

  Scenario: Editing one test's block marks only that test updated
    Given a PR diff that edits the definition of "test_a" in "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_a" is marked updated
    And the test "test_b" is marked context

  Scenario: Editing two tests' blocks marks both updated (no over-narrowing)
    Given a PR diff that edits the definitions of "test_a" and "test_b" in "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_a" is marked updated
    And the test "test_b" is marked updated

  Scenario: A change outside any test definition marks every test context
    # The narrowing that proves block-precision does something: only the
    # surrounding `models:` region changed, so no test's block is touched.
    Given a PR diff that edits "models/marts/core/_core__models.yml" outside any test definition
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's test rows include "test_a"
    And the test "test_a" is marked context
    And the test "test_b" is marked context

  Scenario: Deleting lines from a test's block marks that test updated
    # The zero-count point-touch path: a pure deletion inside a block still
    # counts as touching it.
    Given a PR diff that deletes lines from the definition of "test_a" in "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_a" is marked updated
    And the test "test_b" is marked context

  Scenario: Block-precise updated detection works under a sub-directory project root
    Given a PR diff that edits the definition of "test_a" in "dbt_project/models/marts/core/_core__models.yml"
    And the manifest (compiled with project root "dbt_project") contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root dbt_project --out report.html
    Then the exit code is 0
    And the test "test_a" is marked updated
    And the test "test_b" is marked context

  Scenario: A diff that no longer lines up with the working tree degrades to file-granular updated
    # Revision drift (N7b): the hunks' added lines don't match the working-tree
    # block, so cute-dbt can't trust block-precision and falls back to marking
    # every declared test updated (and Step 3 drops the inline diff).
    Given a PR diff whose hunks no longer line up with "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_a" is marked updated
    And the test "test_b" is marked updated
    And the test "test_a" carries no inline YAML diff

  # cute-dbt#96 concern 2 — the inline YAML diff drawer. The edited test's
  # payload carries a reconstructed diff (a removed + an added line, the
  # change pair); the untouched sibling carries none, so its drawer shows the
  # plain authored YAML. Content (not just presence) is asserted because a
  # flipped removed↔added or empty reconstruction would still be "present".
  Scenario: An updated test carries an inline YAML diff of its block; a context sibling does not
    Given a PR diff that edits the definition of "test_a" in "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    And the model "dim_payers" has a unit test "test_b" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the test "test_a" carries an inline YAML diff with a removed and an added line
    And the test "test_b" carries no inline YAML diff

  # --- cute-dbt#111: inline SQL diff for a changed model's raw_code ---
  #
  # The same line-diff substrate (#96) applied to a model's RAW SQL
  # (`raw_code`), rendered in the Model SQL section with a Raw↔Diff toggle.
  # `raw_code` is read from the MANIFEST (not the filesystem), so the SQL
  # diff fires on a changed `.sql` without a `--project-root` source read.
  # Reconstruction reuses the N7b drift guard + the whitespace-as-standard
  # rule, so a stale diff or a pure re-indent shows the plain SQL view.
  # The interior reconstruction arithmetic is mutation-killed by the
  # `reconstruct_model_sql_diffs` / `reconstruct_one` boundary tables in
  # src/domain/pr_diff.rs; these scenarios pin the user-visible outcome.

  Scenario: A changed model's SQL carries an inline diff in the Model SQL section
    Given a PR diff that changes the SQL of "models/marts/core/dim_payers.sql"
    And the manifest contains a model with raw SQL at "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the model "dim_payers" carries an inline SQL diff with a removed and an added line

  Scenario: A model in scope only via a changed test shows the plain SQL view (no diff)
    # dim_payers.sql is NOT changed; the model is in scope only because its
    # test's YAML changed. Its raw_code is untouched ⇒ no SQL hunk ⇒ no diff.
    Given a PR diff that changes "models/marts/core/_core__models.yml"
    And the manifest contains a model with raw SQL at "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_a" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the model "dim_payers" carries no inline SQL diff

  Scenario: A stale SQL diff degrades to the plain SQL view
    # Revision drift: the hunk's `+` lines don't match the model's raw_code,
    # so N7b fails and cute-dbt shows the plain SQL rather than a wrong diff.
    Given a PR diff whose SQL hunks no longer line up with "models/marts/core/dim_payers.sql"
    And the manifest contains a model with raw SQL at "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the model "dim_payers" carries no inline SQL diff

  Scenario: A whitespace-only SQL change shows the plain SQL view (no diff)
    # Re-indenting the model SQL with no substantive change: whitespace is
    # ignored as standard, so the change-pair is suppressed ⇒ no SQL diff.
    Given a PR diff that re-indents the SQL of "models/marts/core/dim_payers.sql" (whitespace only)
    And the manifest contains a model with raw SQL at "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    When I run cute-dbt report with --manifest current.json --pr-diff @diff.patch --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the model "dim_payers" carries no inline SQL diff
