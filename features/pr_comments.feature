# Maps: cute-dbt#419 / #420 / #421 / #422 (epic #353 — the report's inline
# PR review-comments surface). A reviewer's GitHub review comments are an
# input cute-dbt can surface alongside the diff it already renders. The
# ingested review threads (cute-dbt#395) are anchored onto the rendered
# diff (the shipped comment→diff-line anchoring, cute-dbt#418) and grouped
# per model, then rendered three ways: inline diff-line comment threads
# (#419), a top-of-report total-count navigation button (#420), and a
# per-model comment-count tooltip (#421).
#
# The whole surface is EXPERIMENTAL-gated behind `pr-comments` and is a
# `--pr-diff`-arm affordance (a comment anchors to a *rendered diff*; the
# baseline arm has none). Every scenario asserting the surface opts in via
# the experimental-switch Given (CUTE_DBT_EXPERIMENTAL on the subprocess)
# AND supplies the synthetic review-threads payload via `--pr-comments`
# (the deterministic injection seam standing in for the live `gh` fetch).
# The degrade scenarios at the bottom pin the gated-off + no-PR-context
# postures (no surface, byte-stable default goldens).
#
# Zero-egress is preserved: the comments are baked into the HTML at
# generation time and the report makes zero outbound requests when opened
# offline (any navigate-to-comment is in-page JS, never a fetch) — proven
# executably by the headless zero-egress gate over the committed
# comments-showcase golden.
Feature: A PR's review comments are surfaced inline, anchored to the diff
  As a PR reviewer
  I want the PR's review comments shown beside the lines they refer to
  So that the conversation is part of the review, not a separate tab

  Scenario: An anchored review thread is inlined and the counts are present
    Given the comments-showcase manifest and PR diff
    And the experimental switch enables pr-comments
    And the PR carries synthetic review comments
    When I run cute-dbt report with the PR comments
    Then the exit code is 0
    And the report carries the PR review-comments payload
    And the comment payload reports a total of 3
    And the model fct_orders carries a comment count of 2
    And the model stg_orders carries a comment count of 1

  Scenario: An outdated comment is labeled, never mis-anchored
    Given the comments-showcase manifest and PR diff
    And the experimental switch enables pr-comments
    And the PR carries synthetic review comments
    When I run cute-dbt report with the PR comments
    Then the exit code is 0
    And the comment payload carries an outdated thread with no live line

  Scenario: The report-wide count drives the top navigation button
    Given the comments-showcase manifest and PR diff
    And the experimental switch enables pr-comments
    And the PR carries synthetic review comments
    When I run cute-dbt report with the PR comments
    Then the exit code is 0
    And the report carries the top comment-count navigation container
    And the report carries the per-model comment-count container

  Scenario: With the experiment off, no comment surface is rendered
    Given the comments-showcase manifest and PR diff
    And the PR carries synthetic review comments
    When I run cute-dbt report with the PR comments
    Then the exit code is 0
    And the report carries no PR review-comments payload

  Scenario: With no PR comments supplied, no comment surface is rendered
    Given the comments-showcase manifest and PR diff
    And the experimental switch enables pr-comments
    When I run cute-dbt report without PR comments
    Then the exit code is 0
    And the report carries no PR review-comments payload
