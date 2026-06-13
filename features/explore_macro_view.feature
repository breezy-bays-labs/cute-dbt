# Maps: cute-dbt#345 Slice 1 — the explorer macro view walking skeleton
# (epic cute-dbt#99). The explore verb gains a THIRD sub-page,
# `macro.html`, emitted ONLY when the `--pr-diff` changed a root-project
# macro. Slice 1 proves the seam end-to-end: the conditional emission,
# the nav round-trip, and the negative path that keeps the two-page
# output (and its byte-identity goldens) unchanged.
#
# The focused macro DAG (cute-dbt#345 Slice 3) and the filtered model +
# test directory (Slice 4) render INTO this shell in later slices; here
# the page is an honest empty placeholder.
#
# Founder respec pins inherited from cute-dbt#106: the explorer takes NO
# baseline manifest, ever — the changed-macro signal on `explore` is the
# `--pr-diff`. The `@no-baseline-usage-error` tag is belt-and-braces (the
# baseline-required-grep gate already allows explore invocations).
Feature: explore emits a macro-focus sub-page only when a root macro changed

  Background:
    Given an explore scenario

  @no-baseline-usage-error
  Scenario: a PR diff that changes a root-project macro emits macro.html
    Given the explore manifest declares the model "stg_claims"
    And the explore model "stg_claims" has source path "models/staging/stg_claims.sql"
    And the explore manifest carries the root-project macro "add_dq_flags" at "macros/add_dq_flags.sql"
    And the PR diff changes the explore file "macros/add_dq_flags.sql"
    When I run cute-dbt explore on the macro manifest with the PR diff
    Then the exit code is 0
    And the explore out directory contains "macro.html"
    And dag.html links to the macro-focus page
    And tests.html links to the macro-focus page
    And macro.html carries the macro-focus heading

  @no-baseline-usage-error
  Scenario: a PR diff that changes no root-project macro emits no macro.html
    Given the explore manifest declares the model "stg_claims"
    And the explore model "stg_claims" has source path "models/staging/stg_claims.sql"
    And the explore manifest carries the root-project macro "add_dq_flags" at "macros/add_dq_flags.sql"
    And the PR diff changes the explore file "models/staging/stg_claims.sql"
    When I run cute-dbt explore on the macro manifest with the PR diff
    Then the exit code is 0
    And no macro.html is written
    And dag.html does not link to the macro-focus page
    And tests.html does not link to the macro-focus page

  @no-baseline-usage-error
  Scenario: explore without a PR diff emits no macro.html
    Given the explore manifest declares the model "stg_claims"
    And the explore manifest carries the root-project macro "add_dq_flags" at "macros/add_dq_flags.sql"
    When I run cute-dbt explore on the macro manifest
    Then the exit code is 0
    And no macro.html is written
    And dag.html does not link to the macro-focus page
