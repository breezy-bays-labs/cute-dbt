# Maps: cute-dbt#104 — explore model-detail card (description, config,
# grain, columns) + hover tooltip (epic cute-dbt#99 V5).
#
# The lineage carrier grows a per-node `detail` block — description,
# config.materialized, tags (the authoritative deduplicated TOP-LEVEL
# wire list, the cute-dbt#200 decision), config.meta entries, declared
# columns with their descriptions, and the model's GRAIN — all
# manifest-derived and pre-rendered in Rust (the JS engine stays a pure
# renderer, the cute-dbt#138 posture).
#
# Grain resolves by the locked precedence ladder:
#   1. explicit config.meta.grain (surfaced verbatim)
#   2. a primary-key-class data test    (dbt_constraints.primary_key)
#   3. a compound-unique data test      (>= 2 columns proven together)
#   4. a single unique data test        (1 column proven unique)
#   5. rendered explicitly as "unknown" — never silently guessed.
# Every detected signal is surfaced alongside the winner.
#
# Inference matches tests to models by `attached_node` (the cute-dbt#103
# counting linkage; fusion mirrors dbt-core's `_lookup_attached_node` —
# dbt-fusion `dbt-parser/src/resolve/resolve_tests/
# resolve_data_tests.rs`, `9977b6cb…`) and recognizes the five
# uniqueness signatures on the `(namespace, name)` TUPLE of
# `test_metadata`. Kwarg keys are byte-confirmed against the committed
# playground fixture for native `unique` (`column_name`) and
# `dbt_utils.unique_combination_of_columns` (`combination_of_columns`);
# the `dbt_constraints.{primary_key,unique_key}` (`column_names` /
# `column_name`) and `dbt_expectations.{expect_compound_columns_to_be_
# unique,expect_column_values_to_be_unique}` (`column_list` /
# `column_name`) signatures do NOT occur in the fixture, so these
# scenarios verify them via the synthetic wire splice (the
# coverage_checks.rs / cute-dbt#103 precedent), with the kwarg keys
# pinned from fusion `dbt-parser/src/resolve/primary_key_inference.rs`
# (`9977b6cb…`) and the providing packages' own test macros.
#
# Scenarios are self-contained subprocess wire round-trips (the
# explore_full_manifest.rs pattern); the RENDERED card and the hover
# tooltip (which must NEVER steal the highlight or write the
# focus-commit attribute) are the headless Chromium suite's job
# (tests/headless_zero_egress.rs).
Feature: explore lineage nodes carry a model-detail payload with the resolved grain

  Background:
    Given an explore scenario

  @no-baseline-usage-error
  Scenario: the detail payload carries description, materialization, tags, meta and columns
    Given the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" is described as "One row per order."
    And the explore model "dim_orders" is tagged "marts" and "core"
    And the explore model "dim_orders" is materialized as "table"
    And the explore model "dim_orders" carries meta "owner" = "analytics"
    And the explore model "dim_orders" declares column "order_id" typed "varchar" described as "Primary key."
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the detail payload describes "dim_orders" as "One row per order."
    And the detail payload materializes "dim_orders" as "table"
    And the detail payload tags "dim_orders" with "marts" and "core"
    And the detail payload carries meta "owner" = "analytics" for "dim_orders"
    And the detail payload lists column "order_id" typed "varchar" described as "Primary key." for "dim_orders"

  @no-baseline-usage-error
  Scenario: an explicit config.meta.grain wins over every inferred signal and all detected grains surface
    Given the explore manifest declares the model "fct_orders"
    And the explore model "fct_orders" declares meta grain "order_id + order_date"
    And a native unique test on "fct_orders" column "order_id"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "fct_orders" is "order_id + order_date" sourced from "config.meta.grain"
    And the grain detection for "fct_orders" also surfaces a "unique test" of "order_id"

  @no-baseline-usage-error
  Scenario: a dbt_constraints primary_key test beats a compound-unique test (synthetic wire splice)
    Given the explore manifest declares the model "fct_orders"
    And a "primary_key" test from "dbt_constraints" on "fct_orders" with column names "order_id"
    And a "unique_combination_of_columns" test from "dbt_utils" on "fct_orders" combining "customer_id" and "order_date"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "fct_orders" is "order_id" sourced from "primary-key test"
    And the grain detection for "fct_orders" also surfaces a "compound-unique test" of "customer_id, order_date"

  @no-baseline-usage-error
  Scenario: a compound-unique test beats a single unique test
    Given the explore manifest declares the model "fct_orders"
    And a "unique_combination_of_columns" test from "dbt_utils" on "fct_orders" combining "customer_id" and "order_date"
    And a native unique test on "fct_orders" column "surrogate_key"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "fct_orders" is "customer_id, order_date" sourced from "compound-unique test"
    And the grain detection for "fct_orders" also surfaces a "unique test" of "surrogate_key"

  @no-baseline-usage-error
  Scenario: a single native unique test infers the grain
    Given the explore manifest declares the model "stg_orders"
    And a native unique test on "stg_orders" column "order_id"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "stg_orders" is "order_id" sourced from "unique test"

  @no-baseline-usage-error
  Scenario: a dbt_constraints unique_key test classifies by arity (synthetic wire splice)
    Given the explore manifest declares the model "fct_orders"
    And a "unique_key" test from "dbt_constraints" on "fct_orders" with column names "customer_id" and "order_date"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "fct_orders" is "customer_id, order_date" sourced from "compound-unique test"

  @no-baseline-usage-error
  Scenario: dbt_expectations compound and single signatures are recognized (synthetic wire splice)
    Given the explore manifest declares the model "fct_orders"
    And the explore manifest declares the model "stg_orders"
    And an "expect_compound_columns_to_be_unique" test from "dbt_expectations" on "fct_orders" listing "customer_id" and "order_date"
    And an "expect_column_values_to_be_unique" test from "dbt_expectations" on "stg_orders" column "order_id"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "fct_orders" is "customer_id, order_date" sourced from "compound-unique test"
    And the grain of "stg_orders" is "order_id" sourced from "unique test"

  @no-baseline-usage-error
  Scenario: with no signal the grain renders explicitly as unknown — never silently guessed
    Given the explore manifest declares the model "lonely"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "lonely" is explicitly unknown

  @no-baseline-usage-error
  Scenario: signatures match on the (namespace, name) tuple — a foreign namespace never counts
    Given the explore manifest declares the model "fct_orders"
    And a "unique_combination_of_columns" test from "acme_utils" on "fct_orders" combining "customer_id" and "order_date"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "fct_orders" is explicitly unknown

  @no-baseline-usage-error
  Scenario: a disabled uniqueness test never infers a grain
    Given the explore manifest declares the model "stg_orders"
    And a disabled native unique test on "stg_orders" column "order_id"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the grain of "stg_orders" is explicitly unknown
