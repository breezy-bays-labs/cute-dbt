# Maps: cute-dbt#304 — the `skill` verb (epic #294 V5): emit or install
# the agent-integration skill.
#
# Contract under test:
#   - `skill --print` writes the packaged SKILL.md to stdout, byte-
#     identical to the committed canonical file (the binary embeds it via
#     include_str! — zero drift by construction);
#   - `skill --install [--agent <a>]` writes the skill into the user's
#     repo at the agent's conventional path (.claude/skills for Claude
#     Code, .agents/skills for the cross-agent clients);
#   - `--install` refuses outside a git repository, and `--print` is the
#     no-write escape.
#
# These scenarios never invoke `cute-dbt report` with --manifest, so the
# baseline-required-grep trigger prose does not apply here.
Feature: cute-dbt ships its agent skill

  Scenario: skill --print emits the packaged SKILL.md verbatim
    When I run cute-dbt skill --print
    Then the exit code is 0
    And stdout is byte-identical to the packaged SKILL.md
    And stdout carries the skill frontmatter name "dbt-pr-review"

  Scenario: skill --install writes the Claude Code skill into the repo
    Given a git repo
    When I run cute-dbt skill --install --agent claude-code in the repo
    Then the exit code is 0
    And the file ".claude/skills/dbt-pr-review/SKILL.md" exists in the repo
    And the installed skill is byte-identical to the packaged SKILL.md

  Scenario: skill --install for a cross-agent client uses the .agents path
    Given a git repo
    When I run cute-dbt skill --install --agent cursor in the repo
    Then the exit code is 0
    And the file ".agents/skills/dbt-pr-review/SKILL.md" exists in the repo

  Scenario: skill --install outside a git repository is refused
    Given a directory that is not a git repository
    When I run cute-dbt skill --install in that directory
    Then the exit code is 1
    And stderr explains skill install needs a git repository
    And no skill file is written
