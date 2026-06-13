# Maps: cute-dbt#265 (epic #265 — the macro perspective lens). A macro
# edit is invisible to cute-dbt's model-and-unit-test scope selection: a
# macros/*.sql file matches no model `original_file_path` and macros are
# not unit-test targets, so a macro change slips through entirely. Slice B
# gives the report a "macro changed" section — the changed macro's body
# diff, the count of root-project models it reaches (the reverse
# `macro_blast_radius`), and those models as a collapsible directory tree.
#
# cute-dbt#265 Slice B (epic #288 default-OFF posture): the whole macro
# lens is gated behind the `macro-lens` experiment. Every scenario
# asserting the section opts in via the experimental-switch Given
# (CUTE_DBT_EXPERIMENTAL on the subprocess); the switch-off scenario at
# the bottom pins the default posture (no section even with a macro edit,
# byte-stable non-macro goldens). The section copy says "macro changed",
# NEVER a `state:modified.macros` selector name (critique S2): the
# macro-body-changed scope is a synthesized class, not a dbt state
# selector.
Feature: A changed macro is called out, never silently invisible
  As a PR reviewer
  I want a macros/*.sql edit surfaced with the models it reaches
  So that a macro change is part of the review, not an invisible side door

  Scenario: A macro edit in pr-diff mode renders the section with a body diff and the impacted-model tree
    Given a current manifest with a root-project macro called by two models
    And the working tree carries that macro's source file
    And the PR diff edits the macro's body
    And the experimental switch enables macro-lens
    When I run cute-dbt report in pr-diff mode against the macro patch
    Then the exit code is 0
    And the report carries the macro-lens section
    And the macro-lens section names the changed macro
    And the macro-lens section carries the macro body diff
    And the macro-lens section reports the impacted-model count as 2
    And the macro-lens section lists both impacted models in the directory tree
    And the macro-lens fidelity chip reads "heuristic"
    And the macro-lens section never names a "state:modified.macros" selector

  Scenario: No macro edit leaves the section absent and the report byte-stable
    Given a current manifest with a root-project macro called by two models
    And the working tree carries that macro's source file
    And the PR diff edits only a model's SQL, not the macro
    And the experimental switch enables macro-lens
    When I run cute-dbt report in pr-diff mode against the macro patch
    Then the exit code is 0
    And the report carries no macro-lens section

  Scenario: With the macro-lens switch off a macro edit renders no section
    Given a current manifest with a root-project macro called by two models
    And the working tree carries that macro's source file
    And the PR diff edits the macro's body
    When I run cute-dbt report in pr-diff mode against the macro patch
    Then the exit code is 0
    And the report carries no macro-lens section

  Scenario: The macro lens offers a model-selector and first-order call sites
    Given a current manifest with a root-project macro called inline by two models
    And the working tree carries that macro's source file
    And the PR diff edits the macro's body
    And the experimental switch enables macro-lens
    When I run cute-dbt report in pr-diff mode against the macro patch
    Then the exit code is 0
    And the report carries the macro-lens section
    And the macro-lens section carries an impacted-model selector
    And the impacted-model selector offers both models
    And each impacted model carries a server-rendered SQL panel
    And the macro-lens section shows the macro's first-order call sites

  Scenario: A vendor-package macro edit is filtered out of the lens
    Given a current manifest with a vendor-package macro called by a root-project model
    And the working tree carries that macro's source file
    And the PR diff edits the macro's body
    And the experimental switch enables macro-lens
    When I run cute-dbt report in pr-diff mode against the macro patch
    Then the exit code is 0
    And the report carries no macro-lens section
