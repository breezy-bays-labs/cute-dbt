# Maps: NEW v0.1.x capability — `--scope-from-pr-diff` CLI flag
# Pipeline: cute-dbt-20260527-team-pr-review-ergonomics (Shape E, Phase 1)
# Companion to features/diff_scoping.feature (baseline-manifest path)
#
# Gherkin convention: `<changed-files>` in a When clause is a harness
# placeholder that resolves to the literal list configured by the prior
# `Given a list of changed files containing …` step. It is NOT a CLI
# argument literal. Scenario 14 uses the `@changed.txt` form instead — a
# real `@file` argument resolved to a temp file written by its Given.
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
    Given a list of changed files containing "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the rendered report contains a CTE diagram for "dim_payers"
    And the rendered report's test rows include "test_dim_payers_injects_unknown_sentinel"

  Scenario: A PR that changes only unit-test YAML puts that test in scope
    Given a list of changed files containing "models/marts/core/_core__models.yml"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel" declared in "models/marts/core/_core__models.yml"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's test rows include "test_dim_payers_injects_unknown_sentinel"

  Scenario: A PR with multiple changed files puts all matching nodes in scope
    Given a list of changed files containing "models/marts/core/dim_payers.sql" and "models/marts/analytics/mart_dq_summary.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/analytics/mart_dq_summary.sql"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains both "dim_payers" and "mart_dq_summary"
    And the rendered report contains a CTE diagram for "dim_payers"
    And the rendered report contains a CTE diagram for "mart_dq_summary"

  # CPO finding — proves SCOPING, not just selection.
  Scenario: An unchanged sibling model is NOT in the rendered report
    Given a list of changed files containing "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the manifest also contains an unchanged model "stg_customers" with a unit test "test_stg_customers_unique"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"
    And the rendered report's models-in-scope listing does NOT contain "stg_customers"
    And the rendered report does NOT contain a test row for "test_stg_customers_unique"

  Scenario: A modified model with zero unit tests is in models_in_scope (explorer mode)
    Given a list of changed files containing "models/staging/stg_payments.sql"
    And the manifest contains a model with original_file_path "models/staging/stg_payments.sql"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows "stg_payments" with the "no unit tests wired" empty state

  # --- Project-root path rewriting (load-bearing for the Action wrapper) ---

  Scenario: --project-root rewrites PR-diff paths so a sub-directory dbt project is in scope
    Given a list of changed files containing "dbt_project/models/marts/core/dim_payers.sql"
    And the manifest (compiled with project root "dbt_project") contains a model with original_file_path "models/marts/core/dim_payers.sql"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root dbt_project --out report.html
    Then the exit code is 0
    And the rendered report's models-in-scope listing contains "dim_payers"

  # --- Zero-scope and non-mapping paths ---

  Scenario: A PR with no dbt-relevant changes produces an empty in-scope report
    Given a list of changed files containing only "README.md" and ".github/workflows/ci.yml"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner
    And the rendered report contains no CTE diagrams

  Scenario: A changed path that doesn't map to any manifest node is silently skipped
    Given a list of changed files containing "models/deleted_model.sql"
    And the manifest has no node with original_file_path "models/deleted_model.sql"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner

  # Renamed-model behavior is a documented fidelity limit (PR-diff sees the
  # rename as one deleted path + one added path; the deleted path maps to no
  # current-manifest node, the added path maps to the new node).
  # tracked: cute-dbt#80 — restoration condition: if rename detection becomes
  # load-bearing, layer a git-rename signal on top of `git diff --name-only`.
  # Not blocking v0.1.x.

  # --- Mutual exclusivity at the clap usage layer ---

  Scenario: Passing both --scope-from-pr-diff and --baseline-manifest is a clap usage error
    Given a baseline manifest "baseline.json"
    And a list of changed files containing "models/marts/core/dim_payers.sql"
    When I run cute-dbt with --manifest current.json --baseline-manifest baseline.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains exactly one of --scope-from-pr-diff or --baseline-manifest must be provided

  @no-baseline-usage-error
  Scenario: Passing neither --scope-from-pr-diff nor --baseline-manifest is a clap usage error
    When I run cute-dbt with --manifest current.json --project-root . --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains exactly one of --scope-from-pr-diff or --baseline-manifest must be provided

  # --- Fail-closed contract (Stage 2 unchanged) ---

  Scenario: An in-scope model with compiled_code:null fail-closes with NotCompiled
    Given a list of changed files containing "models/marts/core/dim_payers.sql"
    And the manifest contains a model with no compiled SQL at "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr names "dim_payers" as the offending node and recommends running "dbt compile"

  # --- v0.1 fidelity limits (documented behavior, not defects) ---

  Scenario: A change to only the YAML config block is NOT in scope via PR-diff (documented limit)
    # PR-diff sees only the YAML file path; cute-dbt does not parse the YAML
    # diff to distinguish a config-block change from a unit_tests-block change.
    # Adopters needing config-aware scoping use --baseline-manifest instead.
    Given a list of changed files containing only "dbt_project.yml"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner

  Scenario: A `dbt deps` change that updates package checksums is NOT in scope via PR-diff (documented limit)
    # PR-diff sees only packages.yml change; no dbt SQL files change.
    # Adopters needing dependency-aware scoping use --baseline-manifest instead.
    Given a list of changed files containing only "packages.yml"
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff <changed-files> --project-root . --out report.html
    Then the exit code is 0
    And the rendered report shows the "0 unit tests in scope" banner

  # --- Source-of-diff: @file argument (CI path; Fix A1) ---
  #
  # The original scenario tested a GITHUB_EVENT_PATH JSON contract; GitHub's
  # PR `event.json` does not carry the changed-file list in production, so
  # the workflow computes the diff and hands cute-dbt a file (one path per
  # line) via the real `@file` argument form. See observations.md (Fix A1).

  Scenario: cute-dbt reads the changed file list from a file argument
    Given a list of changed files containing "models/marts/core/dim_payers.sql"
    And the manifest contains a model with original_file_path "models/marts/core/dim_payers.sql"
    And the model "dim_payers" has a unit test "test_dim_payers_injects_unknown_sentinel"
    And the changed-files list is written to a file "changed.txt" one path per line
    When I run cute-dbt with --manifest current.json --scope-from-pr-diff @changed.txt --project-root . --out report.html
    Then the exit code is 0
    And the rendered report's test rows include "test_dim_payers_injects_unknown_sentinel"

  # Scenario: `git diff --name-only origin/main...HEAD` fallback when GITHUB_EVENT_PATH is unset
  # → Exercised by integration test `tests/changed_files_provider.rs`
  # (cucumber-rs subprocess harness cannot cleanly stage a throwaway git
  # repo with two commits per scenario; the provider's @file path is
  # unit-tested in Rust against a controlled fixture instead). This
  # comment preserves the contract; the BDD does not duplicate it.

  # --- Path-normalization mutation kill ---
  # The file-path → manifest-node mapping must handle leading "./" and
  # path-separator normalization (Windows-style separators are NOT
  # supported in v0.1; document and unit-test in Rust).
  # `tests/path_matching.rs` is the Rust unit suite covering the exact-match
  # / leading-"./" / project-root-strip cases (cute-dbt#81). The BDD asserts
  # the user-visible behavior (model X appears in scope); the Rust suite
  # kills mutants on the path-matching function.
