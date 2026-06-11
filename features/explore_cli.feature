# Maps: cute-dbt#100 — the `explore` subcommand walking skeleton (epic
# cute-dbt#99 V1): the verb-structured CLI surface and explore's
# output + fail behavior.
#
# CLI contract under test:
#   - bare `cute-dbt` (no subcommand) is a clap usage error (exit 2,
#     `MissingSubcommand` — pinned at the ErrorKind level in
#     src/cli/args.rs) whose stderr lists BOTH verbs;
#   - `explore --manifest M --out-dir D/` writes `dag.html` +
#     `tests.html` under D/ with no baseline and exits 0;
#   - Stage-1 stays fail-CLOSED on explore (unreadable / pre-v12
#     manifests abort with remediation and write NO pages) while
#     Stage-2 is fail-OPEN (covered in explore_full_manifest.feature).
#
# The `@no-baseline-usage-error` tag exempts these scenarios from the
# baseline-required-grep gate: `explore` has no scope source by design
# (the gate's verb-aware rewrite also allows `explore` structurally —
# belt and braces).
Feature: cute-dbt explore is a first-class verb beside report

  @no-baseline-usage-error
  Scenario: Bare cute-dbt without a subcommand is a usage error naming both verbs
    When I run cute-dbt with no arguments
    Then the exit code is 2
    And stderr lists the subcommands "report" and "explore"

  @no-baseline-usage-error
  Scenario: explore writes dag.html and tests.html under the out directory
    When I run cute-dbt explore with --manifest current.json --out-dir explore/
    Then the exit code is 0
    And the explore out directory contains "dag.html"
    And the explore out directory contains "tests.html"

  @no-baseline-usage-error
  Scenario: Both explore pages are self-contained with zero external references
    When I run cute-dbt explore with --manifest current.json --out-dir explore/
    Then the exit code is 0
    And neither explore page contains external resource references

  @no-baseline-usage-error
  Scenario: explore fails closed on an unreadable manifest
    Given an explore manifest file that is not valid JSON
    When I run cute-dbt explore against that manifest
    Then the exit code is non-zero
    And no explore pages are written
    And stderr explains the manifest could not be read

  @no-baseline-usage-error
  Scenario: explore fails closed on a pre-1.8 manifest schema
    Given an explore manifest whose dbt_schema_version is below the 1.8 floor
    When I run cute-dbt explore against that manifest
    Then the exit code is non-zero
    And no explore pages are written
    And stderr states the minimum supported dbt version
