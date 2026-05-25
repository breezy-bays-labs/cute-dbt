# Maps: SC2 (per-test content) — covers dbt unit_test fixture-format diversity
Feature: cute-dbt renders unit tests authored in dict / csv / sql formats
  As a dbt analytics engineer authoring unit tests
  I want my unit tests to render in the report regardless of authored format
  So that I can use dict, csv, or sql fixture formats interchangeably

  Background:
    Given the committed playground fixture pair

  Scenario: A unit test authored with dict expect format renders
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains the unit test "test_dim_payers_injects_unknown_sentinel"
    And that unit test names the target model "dim_payers"

  Scenario: A unit test authored with csv expect format renders
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains the unit test "test_mart_dq_summary_combines_encounter_and_medication_metrics"
    And that unit test names the target model "mart_dq_summary"

  Scenario: A unit test authored with sql given format renders
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains the unit test "test_mart_dq_summary_zero_quarantined_when_all_valid"
    And that unit test names the target model "mart_dq_summary"

  Scenario: A modified model with zero unit tests in scope renders the empty-state card
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains a section for the model "int_dq_quarantine__encounters"
    And that model's section indicates zero unit tests are wired
