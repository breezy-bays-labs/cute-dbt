# Maps: cute-dbt#301 — the `review` compile step (epic #294 V2): review
# runs the user's own dbt by default — engine detected from the
# `dbt --version` output SHAPE, full-project `dbt compile`, and the
# EXIT CODE as the only success signal (fusion writes manifest.json
# even on failed compiles, so artifact presence proves nothing).
#
# Contract under test:
#   - dbt missing from PATH => install remediation naming the
#     --no-compile escape hatch, exit 1;
#   - a failed compile => dbt's own stderr relayed verbatim, exit 1,
#     NO report — even though a manifest file exists (the fusion trap);
#   - --no-compile trusts the existing manifest; a stale one warns but
#     still renders (never a block), and dbt is never spawned;
#   - the engine shape discriminates: single-line banner = fusion,
#     multi-line Core:/installed: block = python dbt-core.
#
# The dbt on PATH is always a test shim (the suite controls PATH
# completely); these scenarios never invoke `cute-dbt report` directly,
# so the baseline-required-grep trigger prose does not apply here.
Feature: review runs your dbt compile with honest failure handling

  Scenario: dbt missing from PATH gets the install remediation
    Given a git repo with a compiled dbt project on branch "main"
    And a feature branch that edits the "stg_customers" model
    And dbt is not installed on PATH
    When I run cute-dbt review in the repo
    Then the exit code is 1
    And stderr says dbt was not found and names --no-compile
    And no review report is written

  Scenario: A failed compile is fatal even though a manifest file exists
    Given a git repo with a compiled dbt project on branch "main"
    And a feature branch that edits the "stg_customers" model
    And the dbt on PATH fails its compile with a compilation error
    When I run cute-dbt review in the repo
    Then the exit code is 1
    And stderr relays the dbt compilation error verbatim
    And a manifest file still exists in the project target directory
    And no review report is written

  Scenario: --no-compile with a stale manifest warns and still renders
    Given a git repo with a compiled dbt project on branch "main"
    And a feature branch that edits the "stg_customers" model
    And the manifest is older than the model sources
    When I run cute-dbt review with --no-compile in the repo
    Then the exit code is 0
    And stderr warns the manifest is stale
    And the review report is written to the default target path

  Scenario: The fusion engine is detected from its single-line version banner
    Given a git repo with a compiled dbt project on branch "main"
    And a feature branch that edits the "stg_customers" model
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And stderr announces the dbt engine as "fusion 2.0.0-preview.186"
    And the review report is written to the default target path

  Scenario: Python dbt-core is detected from its multi-line version block
    Given a git repo with a compiled dbt project on branch "main"
    And a feature branch that edits the "stg_customers" model
    And the dbt on PATH answers with the python core version block
    When I run cute-dbt review in the repo
    Then the exit code is 0
    And stderr announces the dbt engine as "dbt-core 1.10.2"
    And the review report is written to the default target path
