# Maps: SC1 (offline-correct report), SC2 (per-test content)
Feature: Generate a self-contained report from a compiled dbt manifest
  As a dbt analytics engineer reviewing unit tests
  I want one HTML file that visualizes the in-scope unit tests
  So that I can read Given/Expected fixtures and CTE structure offline

  Background:
    Given a compiled dbt 1.8+ manifest "current.json" with unit tests
    And a baseline manifest "baseline.json"

  Scenario: A diff-scoped report is produced
    When I run cute-dbt with --manifest current.json --baseline-manifest baseline.json --out report.html
    Then the exit code is 0
    And the file "report.html" exists
    And "report.html" is a single self-contained file with no external resource references
    And "report.html" contains a diff-scope banner naming the baseline reference

  Scenario: Each in-scope unit test renders its full block
    Given the model "stg_orders" was modified relative to the baseline
    And "stg_orders" has a unit test "test_stg_orders_dedup"
    When I run cute-dbt with --manifest current.json --baseline-manifest baseline.json --out report.html
    Then "report.html" contains a section for "test_stg_orders_dedup"
    And that section shows the unit test header and target model "stg_orders"
    And that section shows a Given data panel and an Expected data panel
    And that section shows a Mermaid "graph LR" CTE dependency diagram
    And the CTE diagram edges are colored by edge type with a visible legend

  Scenario: A change touching no models yields an empty but valid report
    Given every model has the same body checksum as the baseline
    When I run cute-dbt with --manifest current.json --baseline-manifest baseline.json --out report.html
    Then the exit code is 0
    And the file "report.html" exists
    And the diff-scope banner states "0 unit tests in scope"

  # cute-dbt#165 — column-header tooltips: the embedded payload carries each
  # expected-table column's column-level data tests (the th tooltip's data
  # source; resolved from manifest `test` nodes with `column_name` set and
  # `attached_node` = the target model). The committed fixture's in-scope
  # model declares unique + not_null on its key column, whose authored
  # description is EMPTY (fusion serializes an unset description as "") — so
  # the payload must carry tests-only metadata for that column, and NO entry
  # at all for the undescribed, untested name columns (no empty bubbles).
  Scenario: Expected-table columns carry their column-level data tests in the payload
    Given the model "stg_orders" was modified relative to the baseline
    And "stg_orders" has a unit test "test_stg_orders_dedup"
    When I run cute-dbt with --manifest current.json --baseline-manifest baseline.json --out report.html
    Then the report payload lists column tests "unique" and "not_null" for the expected column "customer_id"
    And the report payload carries no column-header metadata for the expected column "first_name"

  # The locked v0.1 policy: --baseline-manifest is REQUIRED. Omitting it is
  # a clap usage error raised BEFORE the manifest is read (NOT a
  # PreflightError; full-manifest reports are a documented trick — pass an
  # empty/genesis baseline). This scenario is the @no-baseline-usage-error
  # exception that the baseline-required-grep CI job tolerates.
  @no-baseline-usage-error
  Scenario: A missing --baseline-manifest is a usage error before parsing
    When I run cute-dbt with --manifest current.json --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr names the missing "--baseline-manifest" argument
    And stderr explains v0.1 is PR-review-first and a baseline is required
