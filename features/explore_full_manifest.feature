# Maps: cute-dbt#100 — full-manifest scoping (the `all_models` domain
# seam) and the Stage-2 fail-OPEN contract (epic cute-dbt#99 V1).
#
# Scope contract under test (oracled CONCRETELY, not "no baseline
# required"): the rendered model set equals the manifest's model count —
# asserted on the server-rendered tests.html sections AND on the
# embedded `cute-dbt-data` payload (the reused engine-agnostic
# `build_payload` output). An uncompiled model renders as a
# "not compiled" node on both pages and never raises (`PreflightError`
# keeps its four variants; explore raises no fifth).
#
# Scenarios are self-contained (the incremental_models.rs pattern): the
# Givens accumulate a synthetic-manifest plan, the When serializes it
# and runs the real `cute-dbt explore` subprocess, and the Thens assert
# rendered-page + payload facts.
Feature: cute-dbt explore scopes to the full manifest and fails open on uncompiled models

  Background:
    Given an explore scenario

  @no-baseline-usage-error
  Scenario: Every manifest model renders, with no baseline manifest involved
    Given the explore manifest declares the model "stg_orders"
    And the explore manifest declares the model "dim_orders"
    And the explore manifest declares the model "mart_orders"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And tests.html renders exactly 3 model sections
    And the embedded explore payload carries exactly 3 models

  @no-baseline-usage-error
  Scenario: An uncompiled model renders as a not-compiled node instead of failing
    Given the explore manifest declares the model "stg_orders"
    And the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" has no compiled SQL
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And dag.html marks "dim_orders" as not compiled
    And tests.html badges "dim_orders" as not compiled
    And tests.html renders exactly 2 model sections

  @no-baseline-usage-error
  Scenario: A model with zero unit tests still renders on the explore pages
    Given the explore manifest declares the model "lonely_model"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And tests.html renders exactly 1 model section
    And tests.html shows "lonely_model" with zero unit tests wired

  @no-baseline-usage-error
  Scenario: A model's unit tests are listed under it
    Given the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" declares unit test "test_dim_orders"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And tests.html lists the unit test "test_dim_orders" under "dim_orders"

  @no-baseline-usage-error
  Scenario: The lineage page draws an edge between dependent models
    Given the explore manifest declares the model "stg_orders"
    And the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" depends on "stg_orders"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And dag.html carries a lineage edge from "stg_orders" to "dim_orders"
