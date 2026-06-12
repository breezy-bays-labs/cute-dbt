# Maps: cute-dbt#300 — the `review` porcelain verb walking skeleton
# (epic #294 V1): one command from a checked-out branch to the rendered
# PR-review report, against an already-compiled manifest (the compile
# step is slice V2).
#
# Contract under test:
#   - zero flags on a branch with changes => a report scoped to those
#     changes at `<project>/target/cute-dbt-report.html`, path printed;
#   - the working-tree endpoint is the default (uncommitted edits
#     count); `--committed-only` is the PR-exact opt-out;
#   - an empty diff is said out loud (exit 0, no file) unless `--force`;
#   - review-stage failures (no repo / no detectable base) exit 1 with
#     remediation; the report's own fail-closed contract (NotCompiled)
#     passes through verbatim;
#   - `--dry-run` prints the exact planned commands and writes nothing.
#
# These scenarios never invoke `cute-dbt report` directly, so the
# baseline-required-grep gate's trigger prose does not apply here.
Feature: one-command review of the current branch

  Scenario: A branch with a model change yields a report scoped to that change
    Given a git repo with a compiled dbt project on branch "main"
    And a feature branch that edits the "stg_customers" model
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And the review report is written to the default target path
    And stdout prints the review report path
    And the review report includes the unit test "test_stg_customers_renames_columns"

  Scenario: A clean tree on the base branch has nothing to review
    Given a git repo with a compiled dbt project on branch "main"
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And stderr says there is nothing to review
    And no review report is written

  Scenario: --force renders the zero-scope report on an empty diff
    Given a git repo with a compiled dbt project on branch "main"
    When I run cute-dbt review with --force in the repo
    Then the exit code is 0
    And the review report is written to the default target path
    And the review report shows zero unit tests in scope

  Scenario: Outside a git repository review fails with remediation
    Given a dbt project directory that is not inside a git repository
    When I run cute-dbt review in that directory
    Then the exit code is 1
    And stderr explains review needs a git repository

  Scenario: No detectable base names the --base flag
    Given a git repo with a compiled dbt project whose only branch is "work"
    When I run cute-dbt review in the repo
    Then the exit code is 1
    And stderr tells me to pass --base

  Scenario: An uncommitted working-tree edit is included by default
    Given a git repo with a compiled dbt project on branch "main"
    And an uncommitted edit to the "stg_customers" model
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And the review report is written to the default target path
    And the review report includes the unit test "test_stg_customers_renames_columns"

  Scenario: --committed-only excludes the uncommitted edit
    Given a git repo with a compiled dbt project on branch "main"
    And an uncommitted edit to the "stg_customers" model
    When I run cute-dbt review with --committed-only in the repo
    Then the exit code is 0
    And stderr says there is nothing to review
    And no review report is written

  Scenario: An untracked model file is warned about with a git add -N hint
    Given a git repo with a compiled dbt project on branch "main"
    And an untracked new model file
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And stderr warns about untracked files naming "git add -N"

  Scenario: --dry-run prints the exact commands and writes nothing
    Given a git repo with a compiled dbt project on branch "main"
    And a feature branch that edits the "stg_customers" model
    When I run cute-dbt review with --dry-run in the repo
    Then the exit code is 0
    And stdout lists the planned git diff command with "--unified=0"
    And stdout lists the equivalent cute-dbt report invocation
    And no review report is written

  Scenario: An in-scope test on an uncompiled model fails closed through review
    Given a git repo whose dbt project manifest was produced by dbt parse
    And a feature branch that edits the "stg_customers" model
    When I run cute-dbt review in the repo
    Then the exit code is 1
    And stderr recommends running "dbt compile" or "dbt run"
    And no review report is written
