# Maps: cute-dbt#101 — the interactive Cytoscape model-lineage DAG on
# explore's dag.html (epic cute-dbt#99 V2): the LineagePayload JSON
# carrier (nodes = models, edges = FORWARD dependency edges only — the
# client engine traverses both directions itself), the vendored
# Cytoscape core + cytoscape-dagre layout extension, the hand-rolled
# fuzzy-search affordance, the focusable canvas host the search-select
# hands focus to, and the highlight-vs-focus contract's render-time
# half: highlight is exploratory and writes NO data-selected-model —
# only the deliberate Space commit does, at runtime, never at render
# time.
#
# Scenarios are self-contained subprocess wire round-trips (the
# explore_full_manifest.rs pattern): Givens accumulate a synthetic
# manifest plan, the When runs the real `cute-dbt explore` subprocess,
# and the Thens assert emitted-page + payload facts. The RUNTIME
# interaction (real keyboard Space commit, search-select focus move,
# pointer-click highlight) is exercised by the headless Chromium suite
# (tests/headless_zero_egress.rs), which this feature's DOM-structure
# assertions complement, not replace.
Feature: explore's dag.html renders an interactive Cytoscape lineage with highlight/focus wiring

  Background:
    Given an explore scenario

  @no-baseline-usage-error
  Scenario: The lineage payload carries id-keyed nodes and forward edges only
    Given the explore manifest declares the model "stg_orders"
    And the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" depends on "stg_orders"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the lineage payload carries exactly 2 nodes and 1 edge
    And the lineage payload carries no reverse edge from "dim_orders" to "stg_orders"

  @no-baseline-usage-error
  Scenario: The dag page ships the Cytoscape engine with the dagre layout extension
    Given the explore manifest declares the model "orders"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And dag.html embeds the Cytoscape core and the dagre layout extension
    And dag.html embeds the explore lineage engine
    And dag.html no longer embeds a Mermaid lineage

  @no-baseline-usage-error
  Scenario: The search and focus affordances are wired into the page
    Given the explore manifest declares the model "orders"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And dag.html renders the model search combobox
    And dag.html renders the focusable lineage canvas host
    And dag.html writes no data-selected-model at render time

  @no-baseline-usage-error
  Scenario: A hostile model name rides the payload as data and never as markup
    Given the explore manifest declares the model "evil</script><img src=x onerror=alert(1)>"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And the lineage payload round-trips the name "evil</script><img src=x onerror=alert(1)>"
    And dag.html carries no unescaped script-closing markup in the payload carrier
