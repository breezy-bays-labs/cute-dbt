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

  # cute-dbt#173 — the supersedes showcase (catalog C4 + the anti-join
  # refinement): on the LEFT JOIN + WHERE <right key> IS NULL construct,
  # the more specific join.anti-join check fires with the INVERTED
  # recommendation and SILENCES join.left-null-propagation — the rules
  # recognize the pattern instead of forcing the user to suppress a
  # false positive.
  Scenario: An anti-join supersedes the general left-join check on the same construct
    Given the modified coverage model "customers_with_no_orders" compiles to:
      """
      with customers as (select * from "db"."main"."stg_customers"),
      orders as (select * from "db"."main"."stg_orders"),
      final as (
          select *
          from customers
          left join orders on customers.customer_id = orders.customer_id
          where orders.customer_id is null
      )
      select * from final
      """
    When I render the coverage report
    Then the payload carries a "join.anti-join" finding for "customers_with_no_orders" with verdict "uncovered"
    And the payload carries no "join.left-null-propagation" finding for "customers_with_no_orders"
    And the "join.anti-join" finding for "customers_with_no_orders" suggests a given row that matches

  Scenario: A LEFT JOIN whose right columns reach the output flags untested null propagation
    Given the modified coverage model "order_emails" compiles to:
      """
      with orders as (select * from "db"."main"."stg_orders"),
      customers as (select * from "db"."main"."stg_customers"),
      final as (
          select orders.order_id, customers.email
          from orders
          left join customers on orders.customer_id = customers.id
      )
      select * from final
      """
    When I render the coverage report
    Then the payload carries a "join.left-null-propagation" finding for "order_emails" with verdict "uncovered"
    And the payload carries no "join.anti-join" finding for "order_emails"
    And the "join.left-null-propagation" finding for "order_emails" suggests a no-match given row

  # cute-dbt#164 — incremental.branch-coverage (coverage-intelligence
  # rule #1): an incremental model's unit tests must exercise BOTH
  # is_incremental() branches. A test overriding is_incremental to true
  # compiles the incremental branch; an explicit false override OR no
  # override at all compiles the initial full-build branch (dbt's
  # unit-test default). Microbatch-strategy models are the declared
  # rule-#1 exclusion: their batch-window replay is not the
  # is_incremental() fork, so they are never classified.
  Scenario: An incremental model tested on both branches is covered with attribution
    Given the modified incremental coverage model "order_events"
    And a unit test "test_order_events_full_build" on "order_events" with no is_incremental override
    And a unit test "test_order_events_incremental_run" on "order_events" overriding is_incremental to true
    When I render the coverage report
    Then the payload carries a "incremental.branch-coverage" finding for "order_events" with verdict "covered"
    And the "incremental.branch-coverage" finding for "order_events" classifies branch coverage as "both"
    And the "incremental.branch-coverage" finding for "order_events" attributes coverage to unit tests "test_order_events_full_build" and "test_order_events_incremental_run"

  Scenario: A no-override unit test exercises only the full-build branch
    Given the modified incremental coverage model "order_events"
    And a unit test "test_order_events_full_build" on "order_events" with no is_incremental override
    When I render the coverage report
    Then the payload carries a "incremental.branch-coverage" finding for "order_events" with verdict "uncovered"
    And the "incremental.branch-coverage" finding for "order_events" classifies branch coverage as "false-only"
    And the "incremental.branch-coverage" finding for "order_events" suggests the is_incremental true override

  Scenario: An incremental model with no unit tests has neither branch exercised
    Given the modified incremental coverage model "order_events"
    When I render the coverage report
    Then the payload carries a "incremental.branch-coverage" finding for "order_events" with verdict "uncovered"
    And the "incremental.branch-coverage" finding for "order_events" classifies branch coverage as "none"

  Scenario: A microbatch model is never classified by the branch rollup
    Given the modified microbatch coverage model "page_views"
    And a unit test "test_page_views_window" on "page_views" with no is_incremental override
    When I render the coverage report
    Then the payload carries no "incremental.branch-coverage" finding for "page_views"

  # cute-dbt#170 — the findings SURFACE consumes a payload-level spec
  # catalog (check_specs) so the inline rationale drawer renders fully
  # offline and the book reference stays a plain click-only anchor.
  Scenario: The payload carries the spec catalog for fired checks
    Given the modified coverage model "order_rollup" declares unique_key ["customer_id", "order_date"]
    When I render the coverage report
    Then the payload's check catalog describes "grain.unique-key-unbacked" with tier "total" and an inline rationale
    And the "grain.unique-key-unbacked" catalog entry links the book page "checks/grain.unique-key-unbacked.html"

  Scenario: A findings-free payload carries no check catalog
    Given the modified coverage model "plain_model" declares no unique_key
    When I render the coverage report
    Then the payload carries no check catalog
