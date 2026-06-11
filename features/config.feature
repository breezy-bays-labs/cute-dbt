# Maps: NEW v0.1 capability — operator-supplied `--config <PATH>` (cute-dbt#24)
Feature: An operator-supplied TOML config reflects in the rendered report
  As an operator running multiple cute-dbt reports
  I want optional report metadata in a TOML config file
  So that I can label per-PR / per-environment reports without renaming files

  Background:
    Given a compiled dbt 1.8+ manifest "current.json" with unit tests
    And a baseline manifest "baseline.json"

  Scenario: A custom report.title is reflected in the rendered HTML
    Given a config file "config-q3-review.toml"
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --out report.html --config config-q3-review.toml
    Then the exit code is 0
    And the file "report.html" exists
    And "report.html" contains a <title> element with "Q3 unit test review"
    And "report.html" contains an <h1> element with "Q3 unit test review"

  Scenario: A custom report.subtitle renders a new subtitle element
    Given a config file "config-q3-review.toml"
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --out report.html --config config-q3-review.toml
    Then the exit code is 0
    And "report.html" contains a <p class="report-subtitle"> element with "PR 1234 / staging diff"

  Scenario: An absent subtitle omits the subtitle element entirely
    Given a config file "config-title-only.toml"
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --out report.html --config config-title-only.toml
    Then the exit code is 0
    And "report.html" does NOT contain a <p class="report-subtitle"> element

  Scenario: A missing --config file is a usage error before parsing
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --out report.html --config does-not-exist.toml
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains the config file could not be read

  Scenario: An invalid-TOML --config file is a usage error before parsing
    Given a config file "config-broken.toml"
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --out report.html --config config-broken.toml
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains the config file could not be parsed
