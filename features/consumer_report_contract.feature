# Maps: cute-dbt#71 — formal articulation of the consumer contract for
# the CI sticky-comment use case.
#
# This is a doc-feature: the recipe at
# `book/src/recipes/ci-sticky-comment.md` documents the CI workflow
# shape (artifact upload + sticky PR comment); this feature pins the
# structural properties of the rendered report that make that workflow
# useful. Every assertion delegates to existing test infrastructure
# (`common::assert_no_external_refs`, embedded-payload parsing,
# `std::fs::metadata`). The .feature file is the formal statement of
# the contract; the existing tests are the proof.
#
# The shared Given + When steps come from
# `tests/steps/unit_test_format_coverage.rs` (the rendered playground
# report is the same artifact regardless of which contract we're
# asserting).
Feature: Rendered report is suitable for CI sticky-comment delivery
  As a dbt practitioner integrating cute-dbt into my CI pipeline
  I want the rendered report to be a single self-contained HTML file
  carrying a structured payload of the in-scope diff
  So that posting it as a downloadable PR-comment artifact is a
  complete reviewer experience without extra browser-fetching

  Background:
    Given the committed playground fixture pair

  Scenario: Rendered report has zero external resource references
    When I run cute-dbt against the playground fixture pair
    Then the resulting HTML contains zero external resource references

  Scenario: Rendered report embeds the structured in-scope payload
    When I run cute-dbt against the playground fixture pair
    Then the resulting HTML embeds the "cute-dbt-data" payload with at least one model

  Scenario: Rendered report fits within the GitHub Actions artifact budget
    When I run cute-dbt against the playground fixture pair
    Then the resulting HTML file size is under 10 megabytes
