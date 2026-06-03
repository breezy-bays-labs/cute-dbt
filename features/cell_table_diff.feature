# Maps: NEW v0.2 capability — cell-level unit-test data-table diff (cute-dbt#98)
# Pipeline: cute-dbt-20260527-team-pr-review-ergonomics
#
# The structured-table sibling of the cute-dbt#96 inline YAML *line* diff.
# When a PR edits a unit test's `given`/`expect` fixture data, the rendered
# report attaches a `data_diff` to that test (a cell-aligned old→new diff of
# the fixture rows), so a reviewer sees exactly which cell changed rather than
# a whole-block text diff. The given/expect grids then default to a Diff view
# with a per-table Current↔Diff toggle (the toggle's *visibility* is proven by
# the real-Chromium `tests/headless_toggle.rs`; these scenarios assert the
# `data_diff` payload the toggle is built from).
#
# Each scenario builds a synthetic in-memory Manifest (a model + a unit test
# carrying inline fixture rows + an `original_file_path`), synthesizes a
# `git diff --unified=0` patch (and the working-tree YAML it references) that
# edits the fixture in a chosen way, runs the real `cute-dbt` subprocess with
# `--pr-diff @diff.patch`, and asserts against the embedded `cute-dbt-data`
# JSON payload (the same parse strategy as `pr_diff_scoping.feature`). The
# fixture rows the Given declares are the NEW (working-tree) state; the When
# names how the OLD side differed. All data is synthetic; no committed fixture
# file is read (the synthetic-only-fixture invariant is satisfied trivially).
#
# The headline guarantee (cute-dbt#127): a *format-only* reformat of a cell
# (e.g. `1` → `1.00`, which value-inference converges) is NOT a real change,
# so NO `data_diff` is emitted and the grid stays the plain Current view.

Feature: Cell-level unit-test data-table diff in the PR-review report
  As an analytics engineer reviewing a PR that edits unit-test fixtures
  I want each changed fixture cell shown old → new
  So that I can review the data change without reading a whole-block text diff

  Scenario: A PR that edits one fixture cell attaches a cell-level data diff
    Given a unit test "test_dim_users" with a "dict" given row whose "name" is "bob"
    When the PR diff edited that cell's value
    Then the exit code is 0
    And the test "test_dim_users" carries a data diff with one changed cell from "alice" to "bob"

  Scenario: A format-only reformat of a fixture cell attaches NO data diff
    Given a unit test "test_dq_rollup" with a "dict" given row whose "quarantined_count" is "1"
    When the PR diff only reformatted that cell
    Then the exit code is 0
    And the test "test_dq_rollup" carries no data diff

  Scenario: A PR that adds a fixture row attaches a data diff with an added row
    Given a unit test "test_dim_users" with two "dict" given rows
    When the PR diff added the second row
    Then the exit code is 0
    And the test "test_dim_users" carries a data diff with an added row

  Scenario: A PR that removes a fixture row attaches a data diff with a removed row
    Given a unit test "test_dim_users" with a "dict" given row whose "id" is "1"
    When the PR diff removed a second row that existed before
    Then the exit code is 0
    And the test "test_dim_users" carries a data diff with a removed row

  Scenario: The changed test's data diff foregrounds exactly the changed table
    Given a unit test "test_dim_users" with a "dict" given row whose "name" is "bob"
    When the PR diff edited that cell's value
    Then the exit code is 0
    And the test "test_dim_users" data diff has exactly one given table with a Modified row
