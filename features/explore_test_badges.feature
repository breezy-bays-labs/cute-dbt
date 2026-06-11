# Maps: cute-dbt#103 — per-node test-count badges on the explore
# lineage DAG (epic cute-dbt#99 V4).
#
# Every lineage node carries a badge of its test counts —
# "N data-tests · M unit-tests", EXPLICIT at 0/0 — all manifest-derived:
#
# - YAML data-tests attribute by the test's TARGET model via the
#   manifest `attached_node` linkage (fusion mirrors dbt-core's
#   `_lookup_attached_node` — dbt-fusion
#   `dbt-parser/src/resolve/resolve_tests/resolve_data_tests.rs`,
#   `9977b6cb…`). The declaring YAML file is irrelevant (tests for one
#   model may be declared in another model's schema file), and a
#   relationships test's `to:` target rides `depends_on.nodes` WITHOUT
#   attributing — counting by depends_on would double-count. Singular
#   (SQL-file) tests carry `attached_node: null` on real fusion
#   manifests and count toward no model.
# - unit tests attribute by resolving the manifest's BARE `model:`
#   reference (the report renderer's `resolve_target_model` bridge —
#   the two surfaces cannot disagree on a test's target).
#
# The badge string is composed in Rust and rides the lineage carrier
# (the JS engine stays a pure renderer); the counts ride alongside as
# explicit numeric fields — never skip-serialized, so 0/0 is a visible
# fact, not an omitted key. These are TEST-COUNT facts straight off the
# manifest — never check-engine (coverage-intelligence) output.
#
# Scenarios are self-contained subprocess wire round-trips (the
# explore_full_manifest.rs pattern): Givens accumulate a synthetic
# manifest plan (data-test nodes are spliced in the REAL fusion wire
# shape — `attached_node`, `depends_on`, `original_file_path`), the
# When runs the real `cute-dbt explore` subprocess, and the Thens
# assert lineage-carrier facts. The RENDERED badge (the canvas label's
# second line) is the headless Chromium suite's job
# (tests/headless_zero_egress.rs).
Feature: explore lineage nodes carry per-model test-count badges

  Background:
    Given an explore scenario

  @no-baseline-usage-error
  Scenario: data-tests attribute by their target model, never the declaring file
    Given the explore manifest declares the model "dim_orders"
    And the explore manifest declares the model "stg_orders"
    And a data test attached to "dim_orders" is declared in "models/staging/stg_orders.yml"
    And a relationships data test attached to "dim_orders" reaches "stg_orders"
    And the explore model "dim_orders" declares unit test "test_dim_orders_grain"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the lineage carrier counts 2 data-tests and 1 unit-test for "dim_orders"
    And the lineage carrier badges "dim_orders" with "2 data-tests · 1 unit-test"
    And the lineage carrier counts 0 data-tests and 0 unit-tests for "stg_orders"

  @no-baseline-usage-error
  Scenario: a model with no tests still carries the explicit 0/0 badge
    Given the explore manifest declares the model "lonely"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the lineage carrier badges "lonely" with "0 data-tests · 0 unit-tests"
    And the lineage carrier counts 0 data-tests and 0 unit-tests for "lonely"

  @no-baseline-usage-error
  Scenario: a singular test without an attached node counts toward no model
    Given the explore manifest declares the model "dim_orders"
    And a singular data test depending on "dim_orders" carries no attached node
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the lineage carrier counts 0 data-tests and 0 unit-tests for "dim_orders"
