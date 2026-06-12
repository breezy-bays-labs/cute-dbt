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
