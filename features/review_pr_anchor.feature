# Maps: cute-dbt#303 — the `review` PR anchor + the fail-soft gh rung
# (epic #294 V4):
#   - bare `--pr` runs the review off the current branch's open PR (its
#     base branch becomes the review base); no open PR -> remediation;
#   - `--pr <n>` additionally asserts HEAD is that PR's head branch, and
#     on a mismatch tells you to `gh pr checkout <n>` first — review
#     NEVER checks out or mutates your working tree;
#   - the auto-ladder (no --pr) gains the gh rung after --base and the
#     persisted config: fail-soft, so a missing/failing gh falls through
#     silently and gh is never a hard dependency.
#
# The gh on PATH is always a test shim (the suite controls PATH
# completely); these scenarios never invoke `cute-dbt report` directly,
# so the baseline-required-grep trigger prose does not apply here.
Feature: review can anchor to an open pull request

  Scenario: The gh rung resolves the open PR's base in the auto-ladder
    Given a git repo with a compiled dbt project whose only branch is "main"
    And a feature branch that edits the "stg_customers" model
    And gh reports an open PR with base "main" and head "feature"
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And stderr announces the base came from the gh pr rung
    And the review report is written to the default target path

  Scenario: A missing gh falls through the auto-ladder silently
    Given a git repo with a compiled dbt project whose only branch is "main"
    And a feature branch that edits the "stg_customers" model
    And gh is not installed on PATH
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And stderr announces the base came from the local branch probe

  Scenario: Bare --pr uses the open PR's base
    Given a git repo with a compiled dbt project whose only branch is "main"
    And a feature branch that edits the "stg_customers" model
    And gh reports an open PR with base "main" and head "feature"
    When I run cute-dbt review with --pr in the repo
    Then the exit code is 0
    And the review report is written to the default target path

  Scenario: Bare --pr with no open PR is a remediated error
    Given a git repo with a compiled dbt project whose only branch is "main"
    And a feature branch that edits the "stg_customers" model
    And gh reports no open PR
    When I run cute-dbt review with --pr in the repo
    Then the exit code is 1
    And stderr tells me to open a PR with gh pr create
    And no review report is written

  Scenario: --pr <n> on the wrong branch remediates without mutating the tree
    Given a git repo with a compiled dbt project whose only branch is "main"
    And a branch "some-other-branch" that edits the "stg_customers" model
    And gh reports an open PR with base "main" and head "feature"
    When I run cute-dbt review with --pr 9 in the repo
    Then the exit code is 1
    And stderr tells me to run gh pr checkout 9 first
    And review never checked out the working tree
    And no review report is written
