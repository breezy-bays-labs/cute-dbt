# Maps: cute-dbt#302 — the `review` scope variants (epic #294 V3):
# `--staged` reviews only what is staged (HEAD -> index, `git diff
# --cached`); `--unstaged` only the unstaged edits (index -> working
# tree, bare `git diff`); both join the mutually-exclusive review_scope
# ArgGroup.
#
# Contract under test:
#   - --staged scopes the staged change and ignores a purely unstaged
#     edit; --unstaged is the mirror;
#   - --staged is the one variant whose diff endpoint (the index)
#     disagrees with the compiled working tree, so a file that is staged
#     AND further edited unstaged drifts: review warns (naming the
#     file), the drift-guard degrades that file's inline diff, and the
#     run still exits 0 (warn never blocks);
#   - --unstaged never drifts (it diffs exactly what the manifest
#     compiled).
#
# The dbt on PATH is the well-behaved fusion shim the scaffold installs;
# these scenarios never invoke `cute-dbt report` directly, so the
# baseline-required-grep trigger prose does not apply here.
Feature: review scopes to staged or unstaged changes on request

  Scenario: --staged reviews only the staged change
    Given a git repo with a compiled dbt project on branch "main"
    And a staged edit to the "stg_customers" model
    When I run cute-dbt review with --staged in the repo
    Then the exit code is 0
    And the review report is written to the default target path
    And the review report includes the unit test "test_stg_customers_renames_columns"

  Scenario: --staged ignores a purely unstaged edit
    Given a git repo with a compiled dbt project on branch "main"
    And an uncommitted edit to the "stg_customers" model
    When I run cute-dbt review with --staged in the repo
    Then the exit code is 0
    And stderr says there is nothing to review
    And no review report is written

  Scenario: --unstaged reviews only the unstaged edit
    Given a git repo with a compiled dbt project on branch "main"
    And an uncommitted edit to the "stg_customers" model
    When I run cute-dbt review with --unstaged in the repo
    Then the exit code is 0
    And the review report is written to the default target path
    And the review report includes the unit test "test_stg_customers_renames_columns"

  Scenario: --staged with further unstaged edits on the same file warns about drift
    Given a git repo with a compiled dbt project on branch "main"
    And a staged-then-further-unstaged edit to the "stg_customers" model
    When I run cute-dbt review with --staged in the repo
    Then the exit code is 0
    And stderr warns about the staged same-revision drift naming "stg_customers.sql"
    And the review report is written to the default target path
