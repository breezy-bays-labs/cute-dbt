# Maps: SC2 (per-test content) — covers dbt unit_test fixture-format diversity.
#
# dbt's unit_test fixtures can be authored in three formats: `dict`
# (the default), `csv`, and `sql`. Each format arrives in the manifest
# with a different `rows` shape:
#
#   - `dict` rows arrive as an array of dicts (both engines).
#   - `csv` rows arrive as an array of dicts (dbt-core 1.11+) OR as a
#     raw CSV string (dbt-fusion 2.0-preview). cute-dbt parses the fusion
#     path with a hand-rolled RFC 4180 parser in the domain
#     (cute-dbt#66; `parse_csv_rows`, unit-tested by `g22`-`g26` in
#     src/domain/unit_test_table.rs — the JS twin retired in cute-dbt#138).
#   - `sql` rows arrive as a raw SELECT string (both engines). cute-dbt#137
#     tabulates the LITERAL-ROW subset (`select <literal> as <col> union
#     all ...`) as a data table — identical to dict/csv — via a hand-rolled
#     conservative-reject parser in the domain (`parse_sql_literal_rows`).
#     A NON-literal sql (a real FROM/WHERE/operator/cast/function) is
#     rejected and rendered as a syntax-highlighted code block. The raw
#     `rows` string is always retained in the payload; the additive `table`
#     POD (when present) drives the data grid.
#
# These scenarios pin the structural payload shape per format. The
# playground fixture pair is compiled by dbt-core 1.11.11 (see
# `tests/fixtures/MANIFEST.toml`), so csv rows arrive as arrays here.
# A future fusion-emitted fixture will exercise the csv-as-string
# path end-to-end (follow-up cute-dbt#85).
#
# Quote-style note (cute-dbt#249): the `stg_synthea__medications` givens
# are authored DOUBLE-quoted (`ref("…")`) while their sibling
# `stg_synthea__encounters` givens stay single-quoted — dbt accepts both
# Python/Jinja string-literal styles and ships the authored text verbatim
# on the manifest wire (cute-dbt#245), so these scenarios also pin that a
# double-quoted given binds and tabulates identically to its
# single-quoted twin.
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
    And the unit test's given fixture for input "ref("stg_synthea__medications")" has format "csv" with rows as an array
    And the unit test's expected fixture has format "csv" with rows as an array

  Scenario: A unit test authored with literal-row sql givens tabulates them as data tables (cute-dbt#137)
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains the unit test "test_mart_dq_summary_zero_quarantined_when_all_valid"
    And that unit test names the target model "mart_dq_summary"
    And the unit test's given fixture for input "ref('stg_synthea__encounters')" has format "sql" with rows as a string
    And the unit test's given fixture for input "ref("stg_synthea__medications")" has format "sql" with rows as a string
    And the unit test's given fixture for input "ref('stg_synthea__encounters')" tabulates as a data table
    And the unit test's given fixture for input "ref("stg_synthea__medications")" tabulates as a data table
    And the unit test's expected fixture has format "dict" with rows as an array

  Scenario: A modified model with zero unit tests in scope renders the empty-state card
    When I run cute-dbt against the playground fixture pair
    Then the playground report contains a section for the model "int_dq_quarantine__encounters"
    And that model's section indicates zero unit tests are wired
