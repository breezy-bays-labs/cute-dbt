# Maps: check selection + suppression (cute-dbt#171, epic cute-dbt#168) —
# the `[checks]` section of the `--config` TOML plus the inline SQL pragma.
#
# Three operator affordances, all display-layer ONLY (the cute-dbt#186
# pipeline invariant: selection/suppression never alters evaluation or
# supersedes resolution):
#   - selection: sqlfluff-style dual modes — `mode = "opt-out"` (default)
#     with `disable = [...]`, or `mode = "opt-in"` with `enable = [...]`;
#     entries are exact check ids or group globs (`grain.*`), resolved
#     FAIL-CLOSED against the registry (unknown id/glob = usage error with
#     remediation, exit 2, no report written);
#   - `[[checks.suppress]]` entries (check + model + REQUIRED reason): the
#     finding stays in the payload, marked suppressed, reason carried
#     through (suppression is "we know and don't care" — never a removal);
#   - the inline pragma `-- cute-dbt: ignore(check-id, "reason")` scanned
#     from the model's raw SQL (file-level granularity, applies
#     model-wide; reason optional — the surrounding source is its
#     justification surface).

Feature: Operators select and suppress coverage checks via config and pragma
  As a dbt analytics engineer tuning cute-dbt's coverage checks
  I want config-driven check selection plus targeted, reasoned suppression
  So that accepted findings stay visibly acknowledged instead of nagging or vanishing

  Background:
    Given a check-selection report scenario
    And the modified selection model "orders" declares unique_key "order_id" with no uniqueness test

  Scenario: Opt-out mode disabling the grain group removes the finding from the payload
    Given a checks config that disables "grain.*"
    When I render the check-selection report
    Then the exit code is 0
    And the payload carries no findings for "orders"

  Scenario: Opt-in mode enabling the check keeps its finding
    Given an opt-in checks config that enables "grain.unique-key-unbacked"
    When I render the check-selection report
    Then the exit code is 0
    And the payload carries a "grain.unique-key-unbacked" finding for "orders" with verdict "uncovered"

  Scenario: Opt-in mode with an empty enable list displays no findings
    Given an opt-in checks config with an empty enable list
    When I render the check-selection report
    Then the exit code is 0
    And the payload carries no findings for "orders"

  Scenario: A suppress entry keeps the finding and marks it with its reason
    Given a checks config that suppresses "grain.unique-key-unbacked" on "orders" because "duplicate grain accepted during backfill"
    When I render the check-selection report
    Then the exit code is 0
    And the payload carries a "grain.unique-key-unbacked" finding for "orders" with verdict "uncovered"
    And the "grain.unique-key-unbacked" finding for "orders" is suppressed by "config" with reason "duplicate grain accepted during backfill"

  Scenario: An inline SQL pragma suppresses the finding model-wide with its reason
    Given the model "orders" raw SQL carries the pragma -- cute-dbt: ignore(grain.unique-key-unbacked, "known dupes")
    When I render the check-selection report
    Then the exit code is 0
    And the "grain.unique-key-unbacked" finding for "orders" is suppressed by "pragma" with reason "known dupes"

  Scenario: An inline SQL pragma without a reason still suppresses
    Given the model "orders" raw SQL carries the pragma -- cute-dbt: ignore(grain.unique-key-unbacked)
    When I render the check-selection report
    Then the exit code is 0
    And the "grain.unique-key-unbacked" finding for "orders" is suppressed by "pragma" without a reason

  Scenario: An enable list in opt-out mode is a usage error before rendering
    Given a checks config that enables "grain.*" without opt-in mode
    When I render the check-selection report
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains that enable requires opt-in mode

  Scenario: An unknown check id is a usage error naming the known checks
    Given a checks config that disables "grain.nonexistent"
    When I render the check-selection report
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr names the unknown entry "grain.nonexistent" and lists the known check ids
