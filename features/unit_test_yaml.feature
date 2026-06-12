# Maps: cute-dbt#69 (authoring-YAML drawer pipeline).
#
# This feature pins the end-to-end wiring between the source YAML
# slicer (`domain::unit_test_yaml`), the source-YAML reader port,
# the `--project-root` flag + derive-from-manifest fallback, the
# `gather_authoring_yaml` run-loop stage, and the render payload's
# `authoring_yaml` field. The 20 inline domain tests in
# `src/domain/unit_test_yaml.rs` cover the slicer's leading/trailing
# bracketing, file-convention parity, and quoted-name handling
# exhaustively; this feature only exercises the pipeline-level
# integration where new wiring could regress without those unit
# tests catching it.
#
# All scenarios consume the committed
# `tests/fixtures/source-yaml/manifest-{current,baseline}.json` pair
# plus the paired source YAML at
# `tests/fixtures/source-yaml/project/models/_unit_tests.yml`.
#
# Assertions inspect the embedded JSON payload
# (`<script id="cute-dbt-data">`) rather than string-grepping the
# DOM — the renderer wires `test.authoring_yaml` from the payload
# at runtime so structural payload-level assertions are the source
# of truth for what the drawer actually displays.

@no-baseline-usage-error
Feature: cute-dbt surfaces the raw authoring YAML for each in-scope unit test
  As a dbt analytics engineer reviewing a cute-dbt report
  I want each rendered unit test to show the raw YAML block I authored
  So that the report doubles as a one-stop view of "what you wrote"
  and "what cute-dbt sees structurally"

  Background:
    Given the committed source-yaml fixture pair

  Scenario: --project-root resolves and the YAML drawer payload is populated
    When I run cute-dbt against the source-yaml fixture pair with --project-root pointing at the synthetic project
    Then the source-yaml report contains the unit test "test_dim_users_basic"
    And the unit test "test_dim_users_basic" carries authoring YAML containing "- name: test_dim_users_basic"
    And the unit test "test_dim_users_basic" carries authoring YAML containing "Leading comment for test_dim_users_basic"
    And the unit test "test_dim_users_basic" carries authoring YAML containing "Inside-the-body comment for test_dim_users_basic"
    And the unit test "test_dim_users_basic" carries authoring YAML containing "Trailing comment for test_dim_users_basic"

  Scenario: No --project-root and no derive-from-manifest fallback resolves
    When I run cute-dbt against the source-yaml fixture pair without --project-root
    Then the source-yaml report contains the unit test "test_dim_users_basic"
    And the unit test "test_dim_users_basic" carries no authoring YAML in the payload

  Scenario: --project-root resolves but the source file is unreadable
    When I run cute-dbt against the source-yaml fixture pair with --project-root pointing at an empty directory
    Then the source-yaml report contains the unit test "test_dim_users_basic"
    And the unit test "test_dim_users_basic" carries no authoring YAML in the payload

  # cute-dbt#247 — the Model-YAML drawer pipeline rides the same fixture
  # pair: the manifest's dim_users node carries
  # `patch_path: yaml_demo://models/_unit_tests.yml` (the package-URI wire
  # shape both engines emit), and the combined source file carries a
  # `models:` section. These scenarios pin the end-to-end wiring (scheme
  # strip -> ProjectFileReader -> extract_model_block -> ModelPayload
  # .model_yaml) plus the truthful degrade arms the section renders when
  # the file cannot be read.

  Scenario: --project-root resolves and the Model YAML payload carries the authored entry
    When I run cute-dbt against the source-yaml fixture pair with --project-root pointing at the synthetic project
    Then the model "dim_users" carries model YAML containing "- name: dim_users"
    And the model "dim_users" carries model YAML containing "Model-level comment for dim_users"
    And the model "dim_users" carries model YAML containing "data_tests:"
    And the model "dim_users" names "models/_unit_tests.yml" as its schema file

  Scenario: No --project-root degrades the Model YAML section truthfully
    When I run cute-dbt against the source-yaml fixture pair without --project-root
    Then the model "dim_users" carries a Model YAML placeholder naming "--project-root"

  Scenario: --project-root resolves but the schema file is missing
    When I run cute-dbt against the source-yaml fixture pair with --project-root pointing at an empty directory
    Then the model "dim_users" carries a Model YAML placeholder naming "models/_unit_tests.yml"
