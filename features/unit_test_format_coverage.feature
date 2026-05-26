# Maps: SC2 (per-test content) — covers dbt unit_test fixture-format diversity.
#
# dbt's unit_test fixtures can be authored in three formats: `dict`
# (the default), `csv`, and `sql`. Each format arrives in the manifest
# with a different `rows` shape:
#
#   - `dict` rows arrive as an array of dicts (both engines).
#   - `csv` rows arrive as an array of dicts (dbt-core 1.11+) OR as a
#     raw CSV string (dbt-fusion 2.0-preview). cute-dbt's JS renderer
#     contains a hand-rolled RFC 4180 parser for the fusion path
#     (cute-dbt#66; tested via `tests/headless_csv_parser.rs`).
#   - `sql` rows arrive as a raw SELECT string (both engines) — cannot
#     be tabulated without execution; rendered as a syntax-highlighted
#     code block.
#
# These scenarios pin the structural payload shape per format. The
# playground fixture pair is compiled by dbt-core 1.11.11 (see
# `tests/fixtures/MANIFEST.toml`), so csv rows arrive as arrays here.
# A future fusion-emitted fixture will exercise the csv-as-string
# path end-to-end (follow-up cute-dbt#85).
Feature: cute-dbt renders unit tests authored in dict / csv / sql formats
  As a dbt analytics engineer authoring unit tests
  I want my unit tests to render in the report regardless of authored format
  So that I can use dict, csv, or sql fixture formats interchangeably

  Background:
    Given the committed playground fixture pair

  Scenario: A unit test authored with dict given + dict expect renders as tables
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains the unit test "test_dim_payers_injects_unknown_sentinel"
    And that unit test names the target model "dim_payers"
    And the unit test's given fixture for input "ref('stg_synthea__payers')" has format "dict" with rows as an array
    And the unit test's expected fixture has format "dict" with rows as an array

  Scenario: A unit test authored with csv given + csv expect renders as tables
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains the unit test "test_mart_dq_summary_combines_encounter_and_medication_metrics"
    And that unit test names the target model "mart_dq_summary"
    And the unit test's given fixture for input "ref('stg_synthea__encounters')" has format "csv" with rows as an array
    And the unit test's given fixture for input "ref('stg_synthea__medications')" has format "csv" with rows as an array
    And the unit test's expected fixture has format "csv" with rows as an array

  Scenario: A unit test authored with sql given + dict expect renders sql as a code block
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains the unit test "test_mart_dq_summary_zero_quarantined_when_all_valid"
    And that unit test names the target model "mart_dq_summary"
    And the unit test's given fixture for input "ref('stg_synthea__encounters')" has format "sql" with rows as a string
    And the unit test's given fixture for input "ref('stg_synthea__medications')" has format "sql" with rows as a string
    And the unit test's expected fixture has format "dict" with rows as an array

  Scenario: A modified model with zero unit tests in scope renders the empty-state card
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains a section for the model "int_dq_quarantine__encounters"
    And that model's section indicates zero unit tests are wired
