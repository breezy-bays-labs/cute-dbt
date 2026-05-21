# Maps: SC1 (offline correctness), SC4 (auditability re-runnable by anyone)
Feature: The generated report makes zero outbound network requests
  As a developer running cute-dbt on private data
  I want to prove the report cannot exfiltrate data or call out
  So that I can trust the tool with my local manifest

  # This is the PRIMARY zero-egress proof. It is the artifact anyone can
  # re-run themselves, unchanged. The test asserts against a real
  # file:// origin (Chromium's stricter null-origin context), NEVER a
  # 127.0.0.1 loopback — the proof is invalid if it tests loopback.
  Scenario: Opening the report with networking denied issues no external requests
    Given a generated "report.html"
    When the report is opened in a headless browser from a real "file://" origin with all network access denied
    Then the browser issues zero requests to any external host
    And the Mermaid CTE diagram still renders to SVG
    And the DataTables panels still initialize with working sort and search

  # SECONDARY structured lint — targets real loading constructs, NOT raw
  # `grep http` (minified bundles carry hundreds of inert URL string
  # literals; raw grep is false-positive noise that gives a worse signal
  # than the headless test).
  Scenario: The report contains no real external-resource-loading constructs
    Given a generated "report.html"
    When the resource-reference lint scans it
    Then there are no "<script src>" attributes pointing off-document
    And there are no "<link href>" stylesheet references
    And there are no "<img src>" URL references
    And there are no CSS "@import" or "url()" external references
    And there are no protocol-relative "//" resource references
    And the favicon is a "data:" URI or absent

# Asset-manifest completeness (`assets/MANIFEST.toml`: pinned version +
# source URL + SHA-256 + SPDX, all MIT/BSD/permissive) is a repo/CI
# invariant verified by `cargo-deny` + a `cargo test` over the manifest —
# NOT a Gherkin scenario. It joins SC5/SC6 in the CI-invariant bucket
# (see features/README.md).
