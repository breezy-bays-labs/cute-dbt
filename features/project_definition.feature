# Maps: cute-dbt#266 (epic #262 — dbt_project.yml ingestion + the
# categorized project-change panel). R1: a PR editing dbt_project.yml is
# never silently invisible — the report categorizes the edit (vars /
# config tree / dispatch / hooks / paths / identity / other) or shows the
# explicit raw-diff fallback; report generation never fails on this file.
#
# cute-dbt#291 (epic #288): the whole project-state family is gated
# behind the `project-state` experiment, default OFF — every scenario
# asserting its surfaces opts in via the experimental-switch Given
# (CUTE_DBT_EXPERIMENTAL on the subprocess); the switch-off scenarios at
# the bottom pin the default posture (no panel, no chips, no standing
# metadata, no widening).
Feature: Project-definition changes are categorized, never silently invisible
  As a PR reviewer
  I want a dbt_project.yml edit called out and categorized in the report
  So that a project-level change is part of the review, not an invisible side door

  Background:
    Given an empty current manifest

  Scenario: A vars edit renders a categorized vars row with the honest blast-radius copy
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff edits the project var "dq_threshold" from 10 to 5
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the report carries the project-definition panel
    And the panel carries a "vars" row for "dq_threshold" showing "10 → 5"
    And that row's note contains "Blast radius is not fully attributable statically"
    And that row's note contains "SQL and configs plus 0 macro bodies"
    And the vars row notes "no referencing models found by the static scan"
    And the payload carries the parsed project definition

  # cute-dbt#268 — vars attribution tiers: per changed var the panel
  # resolves old→new by fusion's precedence (CLI --vars > package vars >
  # global vars > inline default) and lists affected models at honest
  # evidence tiers — DIRECT (raw_code scan) / CONFIG (unrendered_config)
  # / MACRO (depends_on.macros closure) — with "at least N" framing and
  # the enumerated UNKNOWN residual in-row. Contextualize-don't-widen:
  # a vars edit NEVER widens the in-scope set.
  Scenario: A vars edit renders tiered attribution and never widens scope
    Given the current manifest carries models referencing the project var at every tier
    And the working tree carries the canonical dbt_project.yml
    And the PR diff edits the project var "dq_threshold" from 10 to 5
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the panel's vars row lists "at least 1 model reads this var directly in SQL: mart_dq"
    And the panel's vars row lists "at least 1 model carries config driven by this var: grid_model"
    And the panel's vars row lists "at least 1 model reads this var through its macro closure: stg_enc (via add_dq_flags)"
    And that row's note contains "SQL and configs plus 1 macro body"
    And that row's note contains "never widened into report scope"
    And the payload carries no model "mart_dq"
    And the payload carries no model "grid_model"
    And the payload carries no model "stg_enc"

  Scenario: An in-scope model referencing the edited var carries the reference chip
    Given the current manifest carries models referencing the project var at every tier
    And the working tree carries the canonical dbt_project.yml
    And the PR diff edits the project var "dq_threshold" from 10 to 5 and the direct reader's SQL
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the payload carries the model "mart_dq" in scope
    And the payload model "mart_dq" carries the var reference "dq_threshold" at tier "direct"
    And the payload carries no model "grid_model"

  Scenario: A unit test pinning the edited var in overrides is reported insulated
    Given the current manifest carries models referencing the project var at every tier
    And the manifest unit test "test_mart_dq_rows" pins the var "dq_threshold" in overrides
    And the working tree carries the canonical dbt_project.yml
    And the PR diff edits the project var "dq_threshold" from 10 to 5
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the vars row notes "1 unit test pins this var in overrides.vars and is insulated from this edit (the override always wins): test_mart_dq_rows"

  Scenario: A folder config edit renders a categorized config-tree row
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff edits the marts folder materialization from "view" to "table"
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the panel carries a "config tree" row for "models.bdd_project.marts: +materialized" showing "view" then "table"

  Scenario: A stale dbt_project.yml hunk degrades to the explicit raw-diff fallback
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff claims a dbt_project.yml line that does not match the working tree
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the report carries the project-definition panel
    And the panel shows the raw-diff fallback stating "Could not reconstruct the previous version"

  Scenario: dbt_project.yml in the diff but absent from the project root renders the absence note
    Given the working tree has no dbt_project.yml
    And the PR diff edits the project var "dq_threshold" from 10 to 5
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the report carries the project-definition panel
    And the panel shows the raw-diff fallback stating "could not be read from the project root"

  Scenario: Baseline mode with the switch on parses standing metadata and renders no panel
    Given the working tree carries the canonical dbt_project.yml
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --project-root . --out report.html
    Then the exit code is 0
    And the payload carries the parsed project definition
    And the report carries no project-definition panel

  # cute-dbt#267 — config-tree change attribution: the ONE widening
  # category of epic #262 (by-DEFINITION change, TOTAL tier). A +config
  # subtree edit selects the models whose fqn falls under the edited tree
  # path (fusion's get_config_for_fqn prefix descent) into scope, with
  # provenance chips; vars edits keep contextualize-don't-widen.
  Scenario: A folder config edit widens the models under that subtree into scope with provenance chips
    Given the current manifest carries a marts model "fct_daily" and a staging model "stg_raw" with fqns
    And the working tree carries the canonical dbt_project.yml
    And the PR diff edits the marts folder materialization from "view" to "table"
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the payload carries the model "fct_daily" in scope
    And the payload model "fct_daily" carries the config attribution "materialized" via "models.bdd_project.marts"
    And the payload carries no model "stg_raw"
    And the payload carries the unit test "test_fct_daily_rows" as context, not changed
    And the panel's config-tree row states "affects 1 model — widened into report scope: fct_daily"

  Scenario: A project-level config edit honors deepest-match-wins and skips shadowed models
    Given the current manifest carries a marts model "fct_daily" and a staging model "stg_raw" with fqns
    And the working tree carries the canonical dbt_project.yml
    And the PR diff edits the project-level materialization from "ephemeral" to "view"
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the payload carries the model "stg_raw" in scope
    And the payload model "stg_raw" carries the config attribution "materialized" via "models.bdd_project"
    And the payload carries no model "fct_daily"
    And the panel's config-tree row states "affects 1 model — widened into report scope: stg_raw"

  # cute-dbt#269 — hooks + dispatch get purpose-built rows: the hook SQL
  # diff renders from the manifest's operation.* nodes (TOTAL-tier text);
  # dispatch gets the honest UNKNOWN-tier banner (project-wide effect,
  # not statically attributable). Contextualize, never widen scope.
  Scenario: A hook edit renders a hooks row with the operation-node SQL diff
    Given the working tree carries the canonical dbt_project.yml
    And the current manifest carries the matching on-run-start operation node
    And the PR diff rewrites the on-run-start hook from a revoke statement
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the panel carries a "hooks" row for "on-run-start" with the hook-diff slot
    And that row's note contains "runs in the manifest as operation.bdd_project.bdd_project-on-run-start-0"
    And the payload hooks row is matched and its sql diff adds "grant usage on schema reporting to role analyst"

  Scenario: A hook edit with no operation nodes states the absent-manifest note
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff rewrites the on-run-start hook from a revoke statement
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And that row's note contains "no matching operation.* nodes in the manifest"
    And that row's note contains "the diff is read from dbt_project.yml itself"

  Scenario: A dispatch reorder renders the UNKNOWN-tier banner row
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff reorders the dispatch search order
    And the experimental switch enables project-state
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the panel carries the dispatch banner row at the UNKNOWN tier
    And that row's note contains "macro search order changed"
    And that row's note contains "no call-site resolution was attempted"

  # cute-dbt#291 — the default posture (epic #288): without the
  # experimental switch the SAME inputs render no panel, no provenance
  # chips, no standing metadata — and crucially widen NOTHING into
  # scope. The standing `definition` metadata is gated too (the
  # gate-everything Discovery call): dbt_project.yml contributes zero
  # bytes to the default report.
  Scenario: With the switch off a config-tree edit renders no panel and widens nothing
    Given the current manifest carries a marts model "fct_daily" and a staging model "stg_raw" with fqns
    And the working tree carries the canonical dbt_project.yml
    And the PR diff edits the marts folder materialization from "view" to "table"
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the report carries no project-definition panel
    And the payload carries no model "fct_daily"
    And the payload carries no model "stg_raw"
    And the payload carries no parsed project definition

  Scenario: With the switch off baseline mode embeds no standing metadata
    Given the working tree carries the canonical dbt_project.yml
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --project-root . --out report.html
    Then the exit code is 0
    And the report carries no project-definition panel
    And the payload carries no parsed project definition
