# Maps: cute-dbt#106 — explore PR-diff change context (epic
# cute-dbt#99): `explore --pr-diff` accepts the report's input shape
# (@file / literal — the same value-parser) and highlights the models
# whose files changed on the developer's branch, on the FULL graph.
#
# The founder respec (2026-06-10) is binding:
#   - the explorer takes NO baseline manifest, ever — the
#     developer-native diff signal is git, not environment manifests
#     (those remain a `report`-only environment-comparison concern);
#   - change context NEVER narrows scope — the full graph always
#     renders every model; the diff only decorates the changed ones.
#
# The carrier contract: a changed node serializes `"changed": true`;
# an unchanged node serializes NO `changed` key at all, so the
# no-`--pr-diff` payload stays byte-identical to the pre-#106 shape
# (the committed explore goldens render without change context).
#
# The `@no-baseline-usage-error` tags are belt and braces: the
# baseline-required-grep gate's verb-aware rewrite already allows
# `explore` invocations structurally (no scope source by design).
Feature: explore highlights changed models on the full graph

  Background:
    Given an explore scenario

  @no-baseline-usage-error
  Scenario: a changed model file marks exactly that node as changed
    Given the explore manifest declares the model "stg_orders"
    And the explore model "stg_orders" has source path "models/staging/stg_orders.sql"
    And the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" has source path "models/marts/dim_orders.sql"
    And the PR diff changes the explore file "models/staging/stg_orders.sql"
    When I run cute-dbt explore on the synthetic manifest with the PR diff
    Then the exit code is 0
    And dag.html marks "stg_orders" as changed
    And dag.html does not mark "dim_orders" as changed
    And dag.html counts 1 changed in this diff

  @no-baseline-usage-error
  Scenario: change context never narrows scope — the full graph still renders
    Given the explore manifest declares the model "stg_orders"
    And the explore model "stg_orders" has source path "models/staging/stg_orders.sql"
    And the explore manifest declares the model "dim_orders"
    And the explore manifest declares the model "mart_orders"
    And the PR diff changes the explore file "models/staging/stg_orders.sql"
    When I run cute-dbt explore on the synthetic manifest with the PR diff
    Then the exit code is 0
    And the lineage payload carries exactly 3 models

  @no-baseline-usage-error
  Scenario: explore without a PR diff renders no change context
    Given the explore manifest declares the model "stg_orders"
    And the explore model "stg_orders" has source path "models/staging/stg_orders.sql"
    When I run cute-dbt explore on the synthetic manifest
    Then the exit code is 0
    And no lineage node carries a changed mark
    And dag.html shows no change-context legend

  @no-baseline-usage-error
  Scenario: a pure git rename marks the model changed at its new path
    Given the explore manifest declares the model "dim_b"
    And the explore model "dim_b" has source path "models/marts/dim_b.sql"
    And the PR diff purely renames "models/marts/dim_a.sql" to "models/marts/dim_b.sql"
    When I run cute-dbt explore on the synthetic manifest with the PR diff
    Then the exit code is 0
    And dag.html marks "dim_b" as changed

  @no-baseline-usage-error
  Scenario: a repo-subdirectory project root is stripped from the diff paths
    Given the explore manifest declares the model "dim_payers"
    And the explore model "dim_payers" has source path "models/marts/dim_payers.sql"
    And the PR diff changes the explore file "dbt_project/models/marts/dim_payers.sql"
    And the explore project root is "dbt_project"
    When I run cute-dbt explore on the synthetic manifest with the PR diff
    Then the exit code is 0
    And dag.html marks "dim_payers" as changed

  @no-baseline-usage-error
  Scenario: explore rejects a baseline manifest — the explorer takes no baseline, ever
    When I run cute-dbt explore with --manifest current.json --out-dir explore/ --baseline-manifest baseline.json
    Then the exit code is 2
    And stderr rejects the unknown argument "--baseline-manifest"

  @no-baseline-usage-error
  Scenario: a malformed pr-diff file is a usage error
    Given a pr-diff file that is not a unified diff
    When I run cute-dbt explore with --manifest current.json --out-dir explore/ and that pr-diff
    Then the exit code is 2
    And stderr explains the pr-diff value could not be parsed as a unified diff
