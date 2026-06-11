Feature: Explore external-drive JS contract (cute-dbt#105)
  The explorer's dag.html exposes a versioned external-drive surface so
  other tools — the in-tree VS Code extension (epic #210), a cmux
  browser pane — can drive it and react to deliberate selections: a
  server-rendered contract-version attribute on <body>, the
  focusModel/setView forward hooks, the dual-bound Space commit
  (data-selected-model attribute + host-bridge postMessage), and
  per-node file paths in the lineage payload so hosts can open files.

  These scenarios pin the SERVER-RENDERED halves through the real
  cute-dbt explore subprocess: the version attribute and the
  payload-paths carrier facts (the wire round-trip, package-URI strip
  included). The live hook/bridge behavior — focusModel's no-echo rule,
  setView switching, the dual-bound commit — is the headless Chromium
  suite's jurisdiction (tests/headless_zero_egress.rs).

  Background:
    Given an explore scenario

  Scenario: dag.html serves the contract version as a body attribute
    Given the explore manifest declares the model "dim_orders"
    When I run cute-dbt explore on the synthetic manifest
    Then dag.html carries the external-drive contract version "1" on its body

  Scenario: the lineage payload carries the model's SQL and schema YAML paths
    Given the explore manifest declares the model "dim_orders"
    And the explore model "dim_orders" has its SQL at "models/marts/dim_orders.sql"
    And the explore model "dim_orders" is patched by the wire schema YAML "jaffle_shop://models/marts/_core__models.yml"
    When I run cute-dbt explore on the synthetic manifest
    Then the paths payload for "dim_orders" carries sql "models/marts/dim_orders.sql"
    And the paths payload for "dim_orders" carries schema YAML "models/marts/_core__models.yml"

  Scenario: the lineage payload carries unit-test YAML and external fixture paths
    Given the explore manifest declares the model "dim_orders"
    And a pathed unit test "test_orders" on "dim_orders" declared in "models/marts/_core__models.yml"
    And the pathed unit test "test_orders" reads the external fixture "tests/fixtures/orders_given.csv"
    And the pathed unit test "test_orders" expects the external fixture "tests/fixtures/orders_expected.csv"
    When I run cute-dbt explore on the synthetic manifest
    Then the paths payload for "dim_orders" lists unit test "test_orders" declared in "models/marts/_core__models.yml"
    And the paths payload for "dim_orders" lists fixture "tests/fixtures/orders_given.csv" for unit test "test_orders"
    And the paths payload for "dim_orders" lists fixture "tests/fixtures/orders_expected.csv" for unit test "test_orders"

  Scenario: a bare dbt-core fixture name is carried verbatim
    Given the explore manifest declares the model "dim_orders"
    And a pathed unit test "test_orders" on "dim_orders" declared in "models/marts/_core__models.yml"
    And the pathed unit test "test_orders" reads the external fixture "orders_bare_name"
    When I run cute-dbt explore on the synthetic manifest
    Then the paths payload for "dim_orders" lists fixture "orders_bare_name" for unit test "test_orders"

  Scenario: a pathless model carries explicit empty paths
    Given the explore manifest declares the model "orphan"
    When I run cute-dbt explore on the synthetic manifest
    Then the paths payload for "orphan" is explicitly empty
