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

  # v0.1 named fidelity limit (documented behavior, not a defect; tracking
  # issue filed at the StateComparator site with PR 5).
  Scenario: A config-only change is NOT detected in v0.1 (documented limit)
    Given the model "stg_orders" changed only in its config block
    And its body checksum is identical to the baseline
    When the in-scope set is computed
    Then "stg_orders" is not in scope

  # Explorer mode (#30): modified models with zero unit tests appear in
  # models_in_scope so the render layer can show an "0 unit tests wired"
  # signal for them.
  Scenario: A modified model with zero unit tests is in models_in_scope
    Given the model "stg_payments" has a different body checksum than the baseline
    And "stg_payments" has no unit tests in the current manifest
    When the models-in-scope set is computed
    Then the model "stg_payments" is in models_in_scope
