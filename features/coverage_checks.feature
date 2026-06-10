# Maps: coverage intelligence (epic cute-dbt#168) — the check-engine walking
# skeleton at the PAYLOAD level (cute-dbt#169; the report findings SURFACE is
# the separate render-lane issue cute-dbt#170).
#
# The engine emits a verdict per (construct, check) — covered / uncovered /
# unknown — never just gap findings: COVERED carries the attribution (`by`,
# the satisfying test node ids), UNCOVERED carries the recommendation. The
# walking-skeleton check is `grain.unique-key-unbacked` (TOTAL tier,
# data-test instrument): a model declares `config.unique_key` (incremental
# merge / delete+insert semantics depend on it) but no ENABLED uniqueness
# data test covers a column set that is a subset of the key.
#
# Two semantics these scenarios pin against the wire:
#   - subset direction: a uniqueness test on a SUBSET of the key columns
#     proves the declared grain; a WIDER column set does not;
#   - `dbt_utils.unique_combination_of_columns` stays COMPOSITE — fusion's
#     primary-key inference flattens it per column, which would unsoundly
#     let a {a, b} combination "cover" a single-column key `a`.

Feature: cute-dbt surfaces unique-key coverage findings at the payload level
  As a dbt analytics engineer reviewing models in a PR
  I want each in-scope model's payload to carry per-check coverage verdicts
  So that a declared unique key without a backing uniqueness test is visible

  Background:
    Given a coverage-check report scenario

  Scenario: A unique key with no uniqueness test yields an uncovered grain finding
    Given the modified coverage model "order_rollup" declares unique_key ["customer_id", "order_date"]
    When I render the coverage report
    Then the payload carries a "grain.unique-key-unbacked" finding for "order_rollup" with verdict "uncovered"
    And the "grain.unique-key-unbacked" finding for "order_rollup" recommends adding a uniqueness test

  Scenario: An enabled unique data test on the key column satisfies the check with attribution
    Given the modified coverage model "orders" declares unique_key "order_id"
    And an enabled unique data test on column "order_id" of "orders"
    When I render the coverage report
    Then the payload carries a "grain.unique-key-unbacked" finding for "orders" with verdict "covered"
    And the finding for "orders" attributes coverage to the unique data test on "order_id"

  Scenario: A composite combination test on exactly the key satisfies the check
    Given the modified coverage model "order_days" declares unique_key ["customer_id", "order_date"]
    And an enabled unique_combination_of_columns data test on columns ["customer_id", "order_date"] of "order_days"
    When I render the coverage report
    Then the payload carries a "grain.unique-key-unbacked" finding for "order_days" with verdict "covered"

  Scenario: A combination test wider than the key does not satisfy the check
    Given the modified coverage model "orders" declares unique_key "order_id"
    And an enabled unique_combination_of_columns data test on columns ["order_id", "order_date"] of "orders"
    When I render the coverage report
    Then the payload carries a "grain.unique-key-unbacked" finding for "orders" with verdict "uncovered"

  Scenario: A disabled unique data test does not satisfy the check
    Given the modified coverage model "orders" declares unique_key "order_id"
    And a disabled unique data test on column "order_id" of "orders"
    When I render the coverage report
    Then the payload carries a "grain.unique-key-unbacked" finding for "orders" with verdict "uncovered"

  Scenario: A model without a unique key carries no findings
    Given the modified coverage model "plain_model" declares no unique_key
    When I render the coverage report
    Then the payload carries no findings for "plain_model"
