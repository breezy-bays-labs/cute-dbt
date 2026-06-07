# Maps: SC2 (per-test content) — incremental-model unit-test semantics (cute-dbt#145).
#
# An incremental model's unit tests behave differently from a table/view model's,
# and dbt's docs flag a specific gotcha: for an INCREMENTAL-MODE unit test, the
# `expect` block is the RESULT OF THE MATERIALIZATION (rows that will be
# merged/inserted), NOT the resulting table after the merge. These scenarios pin
# the three report affordances that make that legible:
#
#   1. an "incremental" badge on the model (config.materialized == "incremental");
#   2. a per-test mode badge (incremental branch vs full-refresh branch), keyed
#      off overrides.macros.is_incremental (true => incremental; false/absent =>
#      full refresh);
#   3. an expect-semantics tooltip ("merged/inserted rows, not the final table"),
#      shown ONLY on incremental-mode tests — NEVER on full-refresh-mode tests,
#      where `expect` IS the final table (the tooltip there would be wrong-help);
#   plus a "prior model state" marker on a `given` whose input is `this`.
#
# Mode is read from the structured override value (cute-dbt#145 ingests
# overrides.macros.is_incremental), NOT from the presence of a `this` given —
# lookback-window incrementals set is_incremental: true without mocking `this`,
# and the gotcha still applies.

Feature: cute-dbt surfaces incremental-model unit-test semantics
  As a dbt analytics engineer reviewing unit tests on an incremental model
  I want the report to mark the incremental context and explain the expect gotcha
  So that I do not misread `expect` as the final table after the merge

  Background:
    Given an incremental-model report scenario

  Scenario: An incremental model is marked with an incremental badge
    Given the model "order_events" is materialized "incremental"
    And "order_events" was modified relative to the baseline
    And "order_events" declares unit test "test_order_events_incremental"
    When I render the incremental report
    Then "report.html" marks the model "order_events" as incremental

  Scenario: A table-materialized model carries no incremental badge
    Given the model "orders" is materialized "table"
    And "orders" was modified relative to the baseline
    And "orders" declares unit test "test_orders"
    When I render the incremental report
    Then "report.html" does not mark the model "orders" as incremental

  Scenario: An incremental-mode unit test shows the expect-semantics tooltip and an incremental-branch badge
    Given the model "order_events" is materialized "incremental"
    And "order_events" was modified relative to the baseline
    And "order_events" declares unit test "test_order_events_incremental"
    And the unit test "test_order_events_incremental" overrides is_incremental to true
    When I render the incremental report
    Then the section for "test_order_events_incremental" marks the test as exercising the incremental branch
    And the section for "test_order_events_incremental" explains that Expected is the rows merged or inserted, not the final table

  Scenario: A full-refresh-mode unit test shows a full-refresh badge and no expect-semantics tooltip
    Given the model "order_events" is materialized "incremental"
    And "order_events" was modified relative to the baseline
    And "order_events" declares unit test "test_order_events_full_refresh"
    And the unit test "test_order_events_full_refresh" overrides is_incremental to false
    When I render the incremental report
    Then the section for "test_order_events_full_refresh" marks the test as exercising the full-refresh branch
    And the section for "test_order_events_full_refresh" does not explain the merged-rows expect semantics

  Scenario: A given input of `this` is marked as the prior model state
    Given the model "order_events" is materialized "incremental"
    And "order_events" was modified relative to the baseline
    And "order_events" declares unit test "test_order_events_incremental"
    And the unit test "test_order_events_incremental" has a given input "this"
    And the unit test "test_order_events_incremental" has a given input "ref('stg_orders')"
    When I render the incremental report
    Then the section for "test_order_events_incremental" marks the given "this" as the prior model state
    And the section for "test_order_events_incremental" does not mark the given "ref('stg_orders')" as the prior model state

  Scenario: A unit test on a non-incremental model has no mode badge and no expect-semantics tooltip
    Given the model "orders" is materialized "table"
    And "orders" was modified relative to the baseline
    And "orders" declares unit test "test_orders"
    When I render the incremental report
    Then the section for "test_orders" does not mark the test with an incremental or full-refresh branch
    And the section for "test_orders" does not explain the merged-rows expect semantics
