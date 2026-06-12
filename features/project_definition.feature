# Maps: cute-dbt#266 (epic #262 — dbt_project.yml ingestion + the
# categorized project-change panel). R1: a PR editing dbt_project.yml is
# never silently invisible — the report categorizes the edit (vars /
# config tree / dispatch / hooks / paths / identity / other) or shows the
# explicit raw-diff fallback; report generation never fails on this file.
Feature: Project-definition changes are categorized, never silently invisible
  As a PR reviewer
  I want a dbt_project.yml edit called out and categorized in the report
  So that a project-level change is part of the review, not an invisible side door

  Background:
    Given an empty current manifest

  Scenario: A vars edit renders a categorized vars row with the honest blast-radius note
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff edits the project var "dq_threshold" from 10 to 5
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the report carries the project-definition panel
    And the panel carries a "vars" row for "dq_threshold" showing "10 → 5"
    And that row states "blast radius not attributed"
    And the payload carries the parsed project definition

  Scenario: A folder config edit renders a categorized config-tree row
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff edits the marts folder materialization from "view" to "table"
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the panel carries a "config tree" row for "models.bdd_project.marts: +materialized" showing "view" then "table"

  Scenario: A stale dbt_project.yml hunk degrades to the explicit raw-diff fallback
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff claims a dbt_project.yml line that does not match the working tree
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the report carries the project-definition panel
    And the panel shows the raw-diff fallback stating "Could not reconstruct the previous version"

  Scenario: dbt_project.yml in the diff but absent from the project root renders the absence note
    Given the working tree has no dbt_project.yml
    And the PR diff edits the project var "dq_threshold" from 10 to 5
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the report carries the project-definition panel
    And the panel shows the raw-diff fallback stating "could not be read from the project root"

  Scenario: Baseline mode parses standing metadata and renders no panel
    Given the working tree carries the canonical dbt_project.yml
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --project-root . --out report.html
    Then the exit code is 0
    And the payload carries the parsed project definition
    And the report carries no project-definition panel

  # cute-dbt#269 — hooks + dispatch get purpose-built rows: the hook SQL
  # diff renders from the manifest's operation.* nodes (TOTAL-tier text);
  # dispatch gets the honest UNKNOWN-tier banner (project-wide effect,
  # not statically attributable). Contextualize, never widen scope.
  Scenario: A hook edit renders a hooks row with the operation-node SQL diff
    Given the working tree carries the canonical dbt_project.yml
    And the current manifest carries the matching on-run-start operation node
    And the PR diff rewrites the on-run-start hook from a revoke statement
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the panel carries a "hooks" row for "on-run-start" with the hook-diff slot
    And that row's note contains "runs in the manifest as operation.bdd_project.bdd_project-on-run-start-0"
    And the payload hooks row is matched and its sql diff adds "grant usage on schema reporting to role analyst"

  Scenario: A hook edit with no operation nodes states the absent-manifest note
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff rewrites the on-run-start hook from a revoke statement
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And that row's note contains "no matching operation.* nodes in the manifest"
    And that row's note contains "the diff is read from dbt_project.yml itself"

  Scenario: A dispatch reorder renders the UNKNOWN-tier banner row
    Given the working tree carries the canonical dbt_project.yml
    And the PR diff reorders the dispatch search order
    When I run cute-dbt report with --manifest current.json --pr-diff @project.patch --project-root . --out report.html
    Then the exit code is 0
    And the panel carries the dispatch banner row at the UNKNOWN tier
    And that row's note contains "macro search order changed"
    And that row's note contains "no call-site resolution was attempted"
