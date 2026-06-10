# Maps: SC2 (state:modified.body scoping correctness)
Feature: Diff-scope unit tests and models via dbt state:modified (body subset)
  As a PR reviewer
  I want only the unit tests and models affected by this change
  So that the report is bounded to what the PR actually touches

  Background:
    Given a current manifest and a baseline manifest

  Scenario: A model whose body changed is in scope
    Given the model "stg_orders" has a different body checksum than the baseline
    And "stg_orders" has a unit test "test_stg_orders_dedup"
    When the in-scope set is computed
    Then "test_stg_orders_dedup" is in scope

  Scenario: A model unchanged in body is out of scope
    Given the model "stg_customers" has the same body checksum as the baseline
    And "stg_customers" has a unit test "test_stg_customers_unique"
    When the in-scope set is computed
    Then "test_stg_customers_unique" is not in scope

  Scenario: A newly added model is in scope
    Given the model "stg_returns" does not exist in the baseline
    And "stg_returns" has a unit test "test_stg_returns_nonneg"
    When the in-scope set is computed
    Then "test_stg_returns_nonneg" is in scope

  Scenario: A changed unit test on an unchanged model is in scope
    Given the model "stg_customers" is unchanged in body
    But its unit test "test_stg_customers_unique" was itself modified
    When the in-scope set is computed
    Then "test_stg_customers_unique" is in scope

  # Named fidelity limit of the DEFAULT scope (documented behavior, not a
  # defect). Since cute-dbt#160 the limit is liftable per run via the
  # opt-in --modified-selectors flag (next two scenarios); the default
  # stays body-only by design (README / ADR-3).
  Scenario: A config-only change is NOT detected by default (documented limit)
    Given the model "stg_orders" changed only in its config block
    And its body checksum is identical to the baseline
    When the in-scope set is computed
    Then "stg_orders" is not in scope

  # cute-dbt#160 — the README-promised CLI selector. The selector tokens
  # match dbt's own state:modified.<sub> vocabulary; the body checksum
  # stays always-on (dbt's OR-union across sub-selectors). These two
  # scenarios run the REAL cute-dbt subprocess over the same synthetic
  # config-only divergence: opted in, the change is scoped; without the
  # flag, the default behavior is unchanged.
  Scenario: A config-only change is in scope with --modified-selectors configs
    Given the model "stg_orders" changed only in its config block
    And "stg_orders" has a unit test "test_stg_orders_dedup"
    When I run cute-dbt on the synthetic pair with --modified-selectors "configs"
    Then the exit code is 0
    And the diff-scope banner states "Showing 1 unit test in scope"

  Scenario: The same config-only change stays out of scope without the flag
    Given the model "stg_orders" changed only in its config block
    And "stg_orders" has a unit test "test_stg_orders_dedup"
    When I run cute-dbt on the synthetic pair without --modified-selectors
    Then the exit code is 0
    And the diff-scope banner states "0 unit tests in scope"

  # Explorer mode (#30): modified models with zero unit tests appear in
  # models_in_scope so the render layer can show an "0 unit tests wired"
  # signal for them.
  Scenario: A modified model with zero unit tests is in models_in_scope
    Given the model "stg_payments" has a different body checksum than the baseline
    And "stg_payments" has no unit tests in the current manifest
    When the models-in-scope set is computed
    Then the model "stg_payments" is in models_in_scope
