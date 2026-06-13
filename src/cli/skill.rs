//! The `skill` verb (cute-dbt#304, epic #294 slice V5): emit or install
//! the agent-integration skill.
//!
//! One file, two zero/low-drift channels. The canonical copy lives at
//! `skills/dbt-pr-review/SKILL.md` in this repo (Agent Skills base
//! spec, consumable by `npx skills add` / `gh skill install`), and the
//! **same file** is `include_str!`-embedded into the binary here. So
//! `cute-dbt skill --print` emits skill text that compiled from the
//! same source tree as the flags it documents — zero drift by
//! construction (the Laravel Boost pattern).
//!
//! - `--print` writes the embedded SKILL.md verbatim to stdout (the
//!   no-write escape; safe anywhere).
//! - `--install [--agent <a>]` writes it into the user's repo at the
//!   agent's conventional location — `.claude/skills/dbt-pr-review/`
//!   for Claude Code, `.agents/skills/dbt-pr-review/` for the
//!   cross-agent clients (Cursor / Codex / Copilot). Refuses outside a
//!   git repository (the skill belongs to a repo, not a stray cwd).
//!
//! `metadata.version` in the embedded file mirrors the crate version;
//! a `#[cfg(test)]` assertion pins `metadata.version == CARGO_PKG_VERSION`
//! so the two cannot silently drift.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};

/// The packaged skill, embedded from the repo's canonical copy at
/// compile time — the single source of truth for both the on-disk file
/// and `--print`/`--install`.
pub const SKILL_MD: &str = include_str!("../../skills/dbt-pr-review/SKILL.md");

/// The skill's directory / `name` slug. Must match the
/// `skills/<DIR>/SKILL.md` layout and the frontmatter `name` (the Agent
/// Skills base spec requires `name` == directory name).
const SKILL_NAME: &str = "dbt-pr-review";

/// Arguments for `cute-dbt skill`.
#[derive(Debug, Args)]
#[command(group = clap::ArgGroup::new("skill_action")
    .required(true)
    .multiple(false)
    .args(["print", "install"]))]
pub struct SkillArgs {
    /// Print the packaged SKILL.md to stdout and exit (writes nothing).
    ///
    /// The agent-skill text ships inside this binary, so what is printed
    /// always matches the flags this binary defines.
    #[arg(long)]
    pub print: bool,

    /// Install the skill into the current repository for an agent
    /// (default: Claude Code).
    ///
    /// Writes `.claude/skills/dbt-pr-review/SKILL.md` (Claude Code) or
    /// `.agents/skills/dbt-pr-review/SKILL.md` (Cursor / Codex /
    /// Copilot) at the repository root. Refuses outside a git
    /// repository; use `--print` to inspect the skill without writing.
    #[arg(long)]
    pub install: bool,

    /// Which agent the `--install` layout targets.
    #[arg(long, value_name = "AGENT", default_value = "claude-code")]
    pub agent: Agent,
}

/// The agents `--install` knows a layout for. Claude Code reads
/// `.claude/skills/`; the cross-agent clients read `.agents/skills/`
/// (the skills-CLI / `gh skill` project-scope convention,
/// research-294 sweep-skill-distribution §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Agent {
    /// Claude Code — `.claude/skills/<name>/SKILL.md`.
    ClaudeCode,
    /// `OpenAI` Codex — `.agents/skills/<name>/SKILL.md`.
    Codex,
    /// Cursor — `.agents/skills/<name>/SKILL.md`.
    Cursor,
    /// GitHub Copilot — `.agents/skills/<name>/SKILL.md`.
    Copilot,
}

impl Agent {
    /// The skills root directory this agent reads, relative to the repo
    /// root.
    fn skills_root(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude/skills",
            Self::Codex | Self::Cursor | Self::Copilot => ".agents/skills",
        }
    }

    /// The agent's human label for messages.
    fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::Copilot => "GitHub Copilot",
        }
    }
}

/// A `skill`-stage failure, each with a remediation. Exit 1 (mapped in
/// `cli::run`), never a `PreflightError`.
#[derive(Debug)]
pub enum SkillError {
    /// `--install` outside a git repository.
    NotInGitRepo,
    /// The skill file (or a parent dir) could not be written.
    WriteFailed {
        /// The path being written.
        path: PathBuf,
        /// The underlying io failure.
        detail: String,
    },
    /// A stage failure (reading the working dir, spawning git).
    StageFailed {
        /// What was being attempted.
        context: &'static str,
        /// The underlying detail.
        detail: String,
    },
}

impl SkillError {
    /// The operator-facing stderr message.
    #[must_use]
    pub fn message(&self) -> String {
        let (what, fix) = match self {
            Self::NotInGitRepo => (
                "`skill --install` must run inside a git repository.".to_owned(),
                "Run it from your project checkout — or use `cute-dbt skill --print` to \
                 inspect the skill without writing."
                    .to_owned(),
            ),
            Self::WriteFailed { path, detail } => (
                format!(
                    "could not write the skill to `{}`: {detail}",
                    path.display()
                ),
                "Check the path is writable, then re-run.".to_owned(),
            ),
            Self::StageFailed { context, detail } => (
                format!("failed while {context}: {detail}"),
                "Re-run from your repository checkout.".to_owned(),
            ),
        };
        format!("cute-dbt skill: {what}\n{fix}")
    }
}

/// Execute `cute-dbt skill`: dispatch `--print` (stdout) or `--install`
/// (write into the repo). The `skill_action` `ArgGroup` guarantees
/// exactly one is set.
///
/// # Errors
///
/// [`SkillError`] on an install outside a git repo, a write failure, or
/// a stage failure. `--print` never errors on the skill content
/// (writing to stdout that fails is reported as a stage failure).
pub fn execute_skill(args: &SkillArgs) -> Result<(), SkillError> {
    if args.print {
        return print_skill();
    }
    install_skill(args.agent)
}

/// Write the embedded SKILL.md verbatim to stdout.
fn print_skill() -> Result<(), SkillError> {
    io::stdout()
        .write_all(SKILL_MD.as_bytes())
        .map_err(|err| SkillError::StageFailed {
            context: "writing the skill to stdout",
            detail: err.to_string(),
        })
}

/// Install the skill at the agent's conventional location under the
/// repository root.
fn install_skill(agent: Agent) -> Result<(), SkillError> {
    let toplevel = git_toplevel()?;
    let dest = skill_install_path(&toplevel, agent);
    write_skill_file(&dest)?;
    eprintln!(
        "cute-dbt: installed the `{SKILL_NAME}` skill for {} at {}",
        agent.label(),
        dest.display(),
    );
    Ok(())
}

/// The absolute install path for an agent: `<toplevel>/<skills-root>/
/// <name>/SKILL.md`.
fn skill_install_path(toplevel: &Path, agent: Agent) -> PathBuf {
    toplevel
        .join(agent.skills_root())
        .join(SKILL_NAME)
        .join("SKILL.md")
}

/// Write the embedded skill to `dest`, creating parent directories.
fn write_skill_file(dest: &Path) -> Result<(), SkillError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| SkillError::WriteFailed {
            path: parent.to_path_buf(),
            detail: err.to_string(),
        })?;
    }
    std::fs::write(dest, SKILL_MD).map_err(|err| SkillError::WriteFailed {
        path: dest.to_path_buf(),
        detail: err.to_string(),
    })
}

/// Resolve the git repository toplevel from the current directory, or
/// [`SkillError::NotInGitRepo`] when there is none.
fn git_toplevel() -> Result<PathBuf, SkillError> {
    let cwd = std::env::current_dir().map_err(|err| SkillError::StageFailed {
        context: "reading the working directory",
        detail: err.to_string(),
    })?;
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| SkillError::StageFailed {
            context: "spawning git",
            detail: err.to_string(),
        })?;
    if !output.status.success() {
        return Err(SkillError::NotInGitRepo);
    }
    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if toplevel.is_empty() {
        return Err(SkillError::NotInGitRepo);
    }
    Ok(PathBuf::from(toplevel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skill_version_mirrors_the_crate_version() {
        // The Discovery gotcha (cute-dbt#304): metadata.version in the
        // packaged SKILL.md must equal the crate version, or installed
        // copies advertise a wrong version. Pinned so a crate bump that
        // forgets the SKILL.md fails the build.
        let crate_version = env!("CARGO_PKG_VERSION");
        let needle = format!("version: {crate_version}");
        assert!(
            SKILL_MD.contains(&needle),
            "SKILL.md frontmatter must carry `{needle}` (mirror the crate version): \
             update skills/dbt-pr-review/SKILL.md when bumping Cargo.toml",
        );
    }

    #[test]
    fn embedded_skill_frontmatter_is_base_spec_only() {
        // name matches the dir; required fields present; no Claude-only
        // extensions (research-294: a cross-agent skill stays on the
        // base spec — name/description/compatibility/metadata only).
        assert!(
            SKILL_MD.starts_with("---\n"),
            "the file opens with YAML frontmatter",
        );
        let frontmatter = SKILL_MD
            .split("---\n")
            .nth(1)
            .expect("frontmatter block present");
        assert!(
            frontmatter.contains(&format!("name: {SKILL_NAME}")),
            "name matches the directory slug",
        );
        assert!(frontmatter.contains("description:"), "description present");
        assert!(
            frontmatter.contains("compatibility:"),
            "compatibility present",
        );
        for forbidden in [
            "allowed-tools:",
            "context:",
            "hooks:",
            "disable-model-invocation:",
        ] {
            assert!(
                !frontmatter.contains(forbidden),
                "no Claude-only extension `{forbidden}` in a base-spec skill",
            );
        }
    }

    #[test]
    fn skill_body_names_the_review_command_and_self_heal() {
        assert!(
            SKILL_MD.contains("cute-dbt review --no-open"),
            "the body runs the agent-safe review command",
        );
        assert!(
            SKILL_MD.contains("cute-dbt --version") && SKILL_MD.contains("cute-dbt review --help"),
            "the body carries the self-heal re-grounding instruction",
        );
    }

    #[test]
    fn claude_code_installs_under_dot_claude() {
        let path = skill_install_path(Path::new("/repo"), Agent::ClaudeCode);
        assert_eq!(
            path,
            PathBuf::from("/repo/.claude/skills/dbt-pr-review/SKILL.md"),
        );
    }

    #[test]
    fn cross_agents_install_under_dot_agents() {
        for agent in [Agent::Codex, Agent::Cursor, Agent::Copilot] {
            let path = skill_install_path(Path::new("/repo"), agent);
            assert_eq!(
                path,
                PathBuf::from("/repo/.agents/skills/dbt-pr-review/SKILL.md"),
                "{agent:?} installs under .agents/skills",
            );
        }
    }

    #[test]
    fn skills_root_distinguishes_claude_from_the_rest() {
        assert_eq!(Agent::ClaudeCode.skills_root(), ".claude/skills");
        assert_eq!(Agent::Codex.skills_root(), ".agents/skills");
        assert_eq!(Agent::Cursor.skills_root(), ".agents/skills");
        assert_eq!(Agent::Copilot.skills_root(), ".agents/skills");
    }

    #[test]
    fn not_in_git_repo_message_points_at_print() {
        let msg = SkillError::NotInGitRepo.message();
        assert!(msg.contains("git repository"), "{msg}");
        assert!(
            msg.contains("--print"),
            "the remediation names the no-write escape: {msg}",
        );
    }
}
