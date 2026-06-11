# Maps: cute-dbt#102 — the explore view toggle (CTE ⇄ model) on dag.html
# and the tests.html unit-test viewer (epic cute-dbt#99 V3).
#
# dag.html: the toolbar gains a two-arm view toggle. The lineage arm is
# the boot view; the CTE arm is GATED on a highlighted model (disabled at
# render time — selection is a runtime act) and renders that model's CTE
# DAG with the same vendored Cytoscape + dagre engine as the lineage
# view. The carrier's `cte_dags` map ships one DagPayload per
# CTE-bearing compiled model — the SAME role-classified, join-typed
# graph the report renders (the build_payload reuse seam). An uncompiled
# model ships NO entry: the client renders the labeled fail-open
# degraded view off the lineage node's not_compiled flag, never an
# error, and PreflightError keeps its four variants.
#
# tests.html: the unit-test viewer renders through the SHARED askama
# partial (templates/partials/test-card.html) — the exact test-card +
# Given/Expected markup report.html renders (the include is
# output-preserving there, gated by the byte-identity goldens). The page
# embeds NO Cytoscape, no dagre, no Mermaid, no DataTables and no
# jQuery; the explore-tests.js engine fills the card from the embedded
# cute-dbt-data payload, and each index row wires its test id so a click
# selects it in the viewer.
#
# Scenarios are self-contained subprocess wire round-trips (the
# explore_full_manifest.rs pattern): Givens accumulate a synthetic
# manifest plan, the When runs the real `cute-dbt explore` subprocess,
# and the Thens assert emitted-page + payload facts. The RUNTIME toggle
# interaction (click-to-enable, view flip, selection persistence) is
# exercised by the headless Chromium suite
# (tests/headless_zero_egress.rs), which these render-time assertions
# complement, not replace.
Feature: explore's dag.html toggles between model lineage and a CTE view, and tests.html serves the unit-test viewer

  Background:
    Given an explore scenario

  @no-baseline-usage-error
  Scenario: dag.html renders the view toggle with the CTE arm gated on selection
    Given the explore manifest declares the model "orders"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And dag.html renders the lineage and CTE view toggle arms
    And dag.html renders the CTE view host hidden at render time
    And dag.html embeds the explore CTE engine

  @no-baseline-usage-error
  Scenario: The dag carrier embeds a CTE DAG per CTE-bearing model and none for an uncompiled one
    Given the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" compiles to SQL with the CTE "src_orders"
    And the explore manifest declares the model "mart_orders"
    And the explore model "mart_orders" has no compiled SQL
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the dag carrier embeds a CTE DAG for "dim_orders" containing the node "src_orders"
    And the dag carrier embeds no CTE DAG for "mart_orders"
    And dag.html marks "mart_orders" as not compiled

  @no-baseline-usage-error
  Scenario: tests.html renders the shared unit-test card and wires the index to the viewer
    Given the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" declares unit test "test_dim_orders_grain"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And tests.html renders the shared unit-test card
    And tests.html embeds the explore tests viewer
    And tests.html wires the unit test "test_dim_orders_grain" to the viewer

  @no-baseline-usage-error
  Scenario: tests.html embeds no graph engine and keeps the zero-egress invariant
    Given the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" declares unit test "test_dim_orders_grain"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And tests.html embeds no Cytoscape engine
    And tests.html carries no external resource references
