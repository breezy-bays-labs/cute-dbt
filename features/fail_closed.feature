# Maps: SC3 (fail-closed on unusable manifests)
Feature: Fail closed on manifests that cannot be honestly visualized
  As a risk-conscious adopter
  I want cute-dbt to refuse partial input loudly
  So that a report is never silently incomplete or misleading

  Background:
    Given a baseline manifest "baseline.json"

  Scenario: A parse-only manifest is rejected (in-scope model has no compiled SQL)
    Given a manifest "parsed.json" produced by "dbt parse"
    And it has a unit test whose in-scope target model has compiled_code null
    When I run cute-dbt report with --manifest parsed.json --baseline-manifest baseline.json --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr names the offending node id
    And stderr recommends running "dbt compile" or "dbt run"

  Scenario: A pre-1.8 manifest is rejected at the schema gate
    Given a manifest "old.json" whose dbt_schema_version is below the 1.8 floor
    When I run cute-dbt report with --manifest old.json --baseline-manifest baseline.json --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr states the minimum supported dbt version

  Scenario: An unreadable manifest is rejected before any parsing
    Given a file "broken.json" that is not valid JSON
    When I run cute-dbt report with --manifest broken.json --baseline-manifest baseline.json --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains the manifest could not be read

  Scenario: An unreadable baseline manifest is rejected (no partial report)
    Given a valid compiled manifest "current.json"
    And a "--baseline-manifest" path "missing-baseline.json" that cannot be read
    When I run cute-dbt report with --manifest current.json --baseline-manifest missing-baseline.json --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr explains the baseline manifest could not be used

  Scenario: An out-of-scope uncompiled model does NOT trigger fail-closed
    Given a compiled manifest where an out-of-scope model has compiled_code null
    And all in-scope models have compiled SQL
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --out report.html
    Then the exit code is 0
    And the file "report.html" exists

  Scenario: A modified model with zero unit tests and no compiled_code is rejected
    Given a baseline manifest "baseline.json"
    And the current manifest has a model "model.jaffle_shop.stg_orders" that is modified
    And the current manifest has zero unit tests targeting "model.jaffle_shop.stg_orders"
    And the current manifest has compiled_code null for "model.jaffle_shop.stg_orders"
    When I run cute-dbt report with --manifest current.json --baseline-manifest baseline.json --out report.html
    Then the exit code is non-zero
    And no file "report.html" is written
    And stderr names "model.jaffle_shop.stg_orders" as the not-compiled node
    And stderr does not name a unit test
