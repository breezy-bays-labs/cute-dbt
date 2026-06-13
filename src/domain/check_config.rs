//! Check selection + suppression — the `[checks]` config surface
//! (cute-dbt#171, epic cute-dbt#168).
//!
//! Three operator affordances, all strictly **display-layer**:
//!
//! - **Selection** — sqlfluff-style dual modes. `mode = "opt-out"`
//!   (default) displays every registered check minus `disable = [...]`;
//!   `mode = "opt-in"` displays only `enable = [...]` (so an opt-in
//!   user never has to author a huge opt-out list). Entries are exact
//!   check ids (`grain.unique-key-unbacked`) or group globs
//!   (`join.*`), resolved against the [`CheckId`] registry —
//!   **fail-closed**: an unknown id or group is a config error with
//!   remediation text, never a silent no-op.
//! - **`[[checks.suppress]]` entries** — targeted (check, model,
//!   reason) acknowledgements. `reason` is **required**: suppression
//!   is "we know and don't care," and a config entry lives far from
//!   the code it silences, so it must carry its own justification.
//!   Never for tool misreads — those are supersedes edges / detector
//!   bugs.
//! - **Inline pragma** — `-- cute-dbt: ignore(check-id, "reason")`
//!   scanned from the model's raw SQL ([`scan_pragmas`]). The pragma
//!   may appear on any line of the model file and applies
//!   **model-wide** (file-level granularity; construct-adjacent
//!   placement is deliberately out of scope). `reason` is optional
//!   here: the pragma sits beside the code it silences, so the
//!   surrounding source and review context are its justification
//!   surface.
//!
//! ## The display-layer invariant (cute-dbt#186, extended here)
//!
//! Selection and suppression run strictly **after**
//! [`resolve_supersedes`](crate::domain::checks::resolve_supersedes) —
//! [`apply_check_policy`] is the `filter_for_display` stage grown a
//! config surface. Disabled checks
//! still evaluate and still supersede; suppressing or disabling a
//! superseding check can never resurrect the finding it superseded.
//! Disabled checks' findings are *removed* from the payload; suppressed
//! findings are *kept and marked* ([`Finding::suppressed`]) so the
//! report can render the acknowledgement (and its reason) without any
//! browser-local state.
//!
//! Domain purity: `std` + `serde` only — no I/O, no parser deps. The
//! TOML deserialization happens in `adapters::config_reader`; the
//! validation/resolution here is pure and registry-generic
//! ([`CheckId`]) so it is testable against synthetic registries.

use std::fmt;

use serde::Deserialize;

use crate::domain::checks::{CheckId, Finding, Suppression, SuppressionSource, filter_for_display};

// ---------------------------------------------------------------------
// `[checks]` PODs (deserialized from the `--config` TOML).
// ---------------------------------------------------------------------

/// Selection mode for the `[checks]` section — the sqlfluff-style dual.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChecksMode {
    /// Every registered check is displayed unless listed in `disable`.
    /// The default — an absent `[checks]` section displays everything.
    #[default]
    OptOut,
    /// Only checks listed in `enable` are displayed.
    OptIn,
}

impl ChecksMode {
    /// The config string form (`opt-out` / `opt-in`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OptOut => "opt-out",
            Self::OptIn => "opt-in",
        }
    }
}

/// The `[checks]` section of the operator config
/// ([`crate::domain::AnalysisConfig`]).
///
/// `enable` / `disable` are `Option<Vec<_>>` so *presence* is
/// distinguishable from an empty list: `enable` is only legal (and is
/// required) in opt-in mode; `disable` is only legal in opt-out mode.
/// Cross-field legality and id/glob resolution are validated by
/// [`resolve_check_policy`] — serde alone cannot express them.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecksConfig {
    /// Selection mode; defaults to [`ChecksMode::OptOut`].
    #[serde(default)]
    pub mode: ChecksMode,
    /// Opt-in display list (exact ids or `group.*` globs). Legal — and
    /// required — only when `mode = "opt-in"`. May be empty (an
    /// explicit "display no checks").
    pub enable: Option<Vec<String>>,
    /// Opt-out hide list (exact ids or `group.*` globs). Legal only
    /// when `mode = "opt-out"`.
    pub disable: Option<Vec<String>>,
    /// Targeted `[[checks.suppress]]` acknowledgements.
    #[serde(default)]
    pub suppress: Vec<SuppressEntry>,
}

/// One `[[checks.suppress]]` entry: acknowledge a specific finding on a
/// specific model. All three fields are required (serde rejects a
/// missing field by name); an empty/whitespace `reason` is additionally
/// rejected by [`resolve_check_policy`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuppressEntry {
    /// Exact check id (`grain.unique-key-unbacked`). Group globs are
    /// deliberately rejected — suppression is a precise statement.
    pub check: String,
    /// The model the finding is on: the bare model name (`orders`) or
    /// the full node id (`model.shop.orders`).
    pub model: String,
    /// Why the team accepts this finding ("we know and don't care").
    /// Required and non-empty; carried into the report payload.
    pub reason: String,
}

// ---------------------------------------------------------------------
// Validation errors (fail-closed, remediation-bearing).
// ---------------------------------------------------------------------

/// A `[checks]` validation/resolution failure. Surfaced as a **clap
/// usage error** (exit 2) by the `--config` value-parser — the
/// ARCHITECTURE.md §3 precedent: config errors are usage-time, never a
/// [`crate::domain::PreflightError`] variant.
///
/// Every variant's [`Display`](fmt::Display) carries remediation text;
/// unknown-id variants name the registry's known checks and groups so
/// the operator can fix the entry without leaving the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckConfigError {
    /// `enable` was supplied outside opt-in mode.
    EnableRequiresOptIn,
    /// `disable` was supplied outside opt-out mode.
    DisableRequiresOptOut,
    /// Opt-in mode without an `enable` list (which may be empty, but
    /// must be present — fail-closed beats a silently empty report).
    OptInRequiresEnable,
    /// A selection entry matched no registered check id and is not a
    /// `group.*` glob. Carries the offending entry plus the registry's
    /// known ids/groups for the remediation text.
    UnknownCheck {
        /// The list the entry came from (`enable` / `disable` /
        /// `suppress`).
        key: &'static str,
        /// The offending entry, verbatim.
        entry: String,
        /// Known check ids, in registry declaration order.
        known_checks: Vec<String>,
        /// Known groups, deduplicated, in registry declaration order.
        known_groups: Vec<String>,
    },
    /// A `group.*` glob whose group matches no registered check.
    UnknownGroup {
        /// The list the glob came from (`enable` / `disable`).
        key: &'static str,
        /// The offending glob, verbatim.
        entry: String,
        /// Known groups, deduplicated, in registry declaration order.
        known_groups: Vec<String>,
    },
    /// A `*` appeared anywhere other than the `group.*` form.
    UnsupportedGlob {
        /// The list the entry came from.
        key: &'static str,
        /// The offending entry, verbatim.
        entry: String,
    },
    /// A `[[checks.suppress]]` entry used a glob — suppression takes an
    /// exact check id.
    SuppressTakesExactId {
        /// The offending `check` value, verbatim.
        entry: String,
    },
    /// A `[[checks.suppress]]` entry with an empty/whitespace `reason`.
    EmptySuppressReason {
        /// The entry's `check` value.
        check: String,
        /// The entry's `model` value.
        model: String,
    },
}

impl fmt::Display for CheckConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnableRequiresOptIn => write!(
                f,
                "[checks] `enable` is only legal with mode = \"opt-in\"; \
                 either set mode = \"opt-in\" or use `disable` to hide \
                 checks in opt-out mode"
            ),
            Self::DisableRequiresOptOut => write!(
                f,
                "[checks] `disable` is only legal with mode = \"opt-out\" \
                 (the default); either drop `mode = \"opt-in\"` or list the \
                 checks you want via `enable`"
            ),
            Self::OptInRequiresEnable => write!(
                f,
                "[checks] mode = \"opt-in\" requires an `enable` list \
                 (it may be empty to display no checks)"
            ),
            Self::UnknownCheck {
                key,
                entry,
                known_checks,
                known_groups,
            } => write!(
                f,
                "[checks] {key} entry {entry:?} matches no registered \
                 check; known checks: {}; known groups: {} (see \
                 heuristics/registry.toml)",
                known_checks.join(", "),
                known_groups.join(", "),
            ),
            Self::UnknownGroup {
                key,
                entry,
                known_groups,
            } => write!(
                f,
                "[checks] {key} glob {entry:?} matches no registered \
                 check group; known groups: {} (see \
                 heuristics/registry.toml)",
                known_groups.join(", "),
            ),
            Self::UnsupportedGlob { key, entry } => write!(
                f,
                "[checks] {key} entry {entry:?} is not a supported \
                 pattern; use an exact check id or a group glob of the \
                 form \"<group>.*\""
            ),
            Self::SuppressTakesExactId { entry } => write!(
                f,
                "[[checks.suppress]] `check` takes an exact check id, \
                 got the pattern {entry:?}; suppression is a precise \
                 acknowledgement — to hide a whole group, use `disable`"
            ),
            Self::EmptySuppressReason { check, model } => write!(
                f,
                "[[checks.suppress]] entry for check {check:?} on model \
                 {model:?} has an empty `reason`; suppression is \"we \
                 know and don't care\" — say why"
            ),
        }
    }
}

impl std::error::Error for CheckConfigError {}

// ---------------------------------------------------------------------
// The resolved policy.
// ---------------------------------------------------------------------

/// One resolved suppression rule — a validated config entry or a
/// scanned pragma, normalized to the shape [`apply_check_policy`]
/// matches findings against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressRule<Id> {
    /// The check whose finding this rule acknowledges.
    pub check: Id,
    /// The model the rule targets: a full node id
    /// (`model.shop.orders`) or a bare model name (`orders`). Pragma
    /// rules always carry the full node id of the file they were
    /// scanned from.
    pub model: String,
    /// The acknowledgement reason. Always `Some` for config entries
    /// (validated non-empty); optional for pragmas.
    pub reason: Option<String>,
    /// Where the rule came from (config entry vs inline pragma).
    pub source: SuppressionSource,
}

/// The resolved display policy [`apply_check_policy`] enforces —
/// the output of [`resolve_check_policy`] plus any pragma rules the
/// cli layer scanned from raw model SQL.
///
/// `Default` is the absent-config policy: every registered check
/// displayed, nothing suppressed — byte-identical output to the
/// pre-#171 pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckPolicy<Id: CheckId> {
    /// Checks whose findings are displayed, in registry declaration
    /// order.
    pub displayed: Vec<Id>,
    /// Resolved suppression rules (config entries first, then pragmas
    /// in scan order).
    pub suppressions: Vec<SuppressRule<Id>>,
}

impl<Id: CheckId> Default for CheckPolicy<Id> {
    fn default() -> Self {
        // cute-dbt#260 Slice 3 — experiment-gated checks (the
        // `enforcement` group, gated behind `Experiment::Governance`) are
        // OFF in the default display set, so the gate-free `explore` page
        // and every non-governance render never surface them. The report
        // run loop's `build_check_policy` re-adds them when governance is
        // enabled. Gated checks still EVALUATE — this is a display filter,
        // not a registry exclusion (the suppression-hierarchy invariant).
        Self {
            displayed: Id::ALL
                .iter()
                .copied()
                .filter(|id| !id.is_experimental())
                .collect(),
            suppressions: Vec::new(),
        }
    }
}

impl<Id: CheckId> CheckPolicy<Id> {
    /// The complement of [`Self::displayed`] — the input
    /// [`filter_for_display`] consumes.
    fn disabled(&self) -> Vec<Id> {
        Id::ALL
            .iter()
            .copied()
            .filter(|id| !self.displayed.contains(id))
            .collect()
    }
}

/// Look up a registered check by its exact dotted id string.
#[must_use]
pub fn check_by_id<Id: CheckId>(id_str: &str) -> Option<Id> {
    Id::ALL
        .iter()
        .copied()
        .find(|id| id.spec().id_str == id_str)
}

/// Known check ids, in registry declaration order (remediation text).
fn known_checks<Id: CheckId>() -> Vec<String> {
    Id::SPECS.iter().map(|s| s.id_str.to_owned()).collect()
}

/// Known groups, deduplicated, in declaration order (remediation text).
fn known_groups<Id: CheckId>() -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    for spec in Id::SPECS {
        if !groups.iter().any(|g| g == spec.group) {
            groups.push(spec.group.to_owned());
        }
    }
    groups
}

/// Resolve one `enable`/`disable` entry to the registered checks it
/// names: an exact id resolves to that check; a `group.*` glob resolves
/// to every check in the group. Fail-closed on anything else.
fn resolve_entry<Id: CheckId>(key: &'static str, entry: &str) -> Result<Vec<Id>, CheckConfigError> {
    if let Some(group) = entry.strip_suffix(".*") {
        if group.contains('*') {
            return Err(CheckConfigError::UnsupportedGlob {
                key,
                entry: entry.to_owned(),
            });
        }
        let matched: Vec<Id> = Id::ALL
            .iter()
            .copied()
            .filter(|id| id.spec().group == group)
            .collect();
        if matched.is_empty() {
            return Err(CheckConfigError::UnknownGroup {
                key,
                entry: entry.to_owned(),
                known_groups: known_groups::<Id>(),
            });
        }
        return Ok(matched);
    }
    if entry.contains('*') {
        return Err(CheckConfigError::UnsupportedGlob {
            key,
            entry: entry.to_owned(),
        });
    }
    match check_by_id::<Id>(entry) {
        Some(id) => Ok(vec![id]),
        None => Err(CheckConfigError::UnknownCheck {
            key,
            entry: entry.to_owned(),
            known_checks: known_checks::<Id>(),
            known_groups: known_groups::<Id>(),
        }),
    }
}

/// Resolve a whole selection list to the set of checks it names,
/// preserving registry declaration order in the result.
fn resolve_list<Id: CheckId>(
    key: &'static str,
    entries: &[String],
) -> Result<Vec<Id>, CheckConfigError> {
    let mut named: Vec<Id> = Vec::new();
    for entry in entries {
        for id in resolve_entry::<Id>(key, entry)? {
            if !named.contains(&id) {
                named.push(id);
            }
        }
    }
    // Registry declaration order, independent of entry order.
    Ok(Id::ALL
        .iter()
        .copied()
        .filter(|id| named.contains(id))
        .collect())
}

/// Validate the `[checks]` section against the `Id` registry and
/// resolve it into the display policy.
///
/// This is the **fail-closed gate**: cross-field mode legality
/// (`enable` ⇔ opt-in, `disable` ⇔ opt-out), id/glob resolution against
/// the registry, exact-id-only + non-empty-reason suppression entries.
/// Run at `--config` parse time by the cli value-parser so every
/// failure is a clap usage error (exit 2) with remediation text.
///
/// # Errors
///
/// Any [`CheckConfigError`] variant — see each variant's docs.
pub fn resolve_check_policy<Id: CheckId>(
    config: &ChecksConfig,
) -> Result<CheckPolicy<Id>, CheckConfigError> {
    Ok(CheckPolicy {
        displayed: resolve_displayed::<Id>(config)?,
        suppressions: resolve_suppressions::<Id>(config)?,
    })
}

/// Resolve the `[checks]` `mode` + `enable`/`disable` lists into the set of
/// displayed check ids. Enforces the cross-field mode legality gate:
/// `enable` requires opt-in, `disable` requires opt-out, opt-in requires a
/// non-empty `enable` list.
fn resolve_displayed<Id: CheckId>(config: &ChecksConfig) -> Result<Vec<Id>, CheckConfigError> {
    match config.mode {
        ChecksMode::OptOut => {
            if config.enable.is_some() {
                return Err(CheckConfigError::EnableRequiresOptIn);
            }
            let disabled = match &config.disable {
                Some(entries) => resolve_list::<Id>("disable", entries)?,
                None => Vec::new(),
            };
            Ok(Id::ALL
                .iter()
                .copied()
                .filter(|id| !disabled.contains(id))
                .collect())
        }
        ChecksMode::OptIn => {
            if config.disable.is_some() {
                return Err(CheckConfigError::DisableRequiresOptOut);
            }
            let Some(entries) = &config.enable else {
                return Err(CheckConfigError::OptInRequiresEnable);
            };
            resolve_list::<Id>("enable", entries)
        }
    }
}

/// Resolve the `[[suppress]]` entries into [`SuppressRule`]s. Each entry is
/// exact-id-only (no glob), resolves against the registry, and requires a
/// non-empty reason — any violation is a fail-closed config error.
fn resolve_suppressions<Id: CheckId>(
    config: &ChecksConfig,
) -> Result<Vec<SuppressRule<Id>>, CheckConfigError> {
    let mut suppressions = Vec::new();
    for entry in &config.suppress {
        if entry.check.contains('*') {
            return Err(CheckConfigError::SuppressTakesExactId {
                entry: entry.check.clone(),
            });
        }
        let check =
            check_by_id::<Id>(&entry.check).ok_or_else(|| CheckConfigError::UnknownCheck {
                key: "suppress",
                entry: entry.check.clone(),
                known_checks: known_checks::<Id>(),
                known_groups: known_groups::<Id>(),
            })?;
        if entry.reason.trim().is_empty() {
            return Err(CheckConfigError::EmptySuppressReason {
                check: entry.check.clone(),
                model: entry.model.clone(),
            });
        }
        suppressions.push(SuppressRule {
            check,
            model: entry.model.clone(),
            reason: Some(entry.reason.clone()),
            source: SuppressionSource::Config,
        });
    }
    Ok(suppressions)
}

// ---------------------------------------------------------------------
// Inline pragma scanning.
// ---------------------------------------------------------------------

/// One parsed `-- cute-dbt: ignore(check-id, "reason")` pragma.
///
/// `check` is the raw id string — resolution against the registry
/// happens at the call site ([`check_by_id`]) so an unknown id can be
/// surfaced as a stderr warning by the cli layer (a pragma is source
/// text, not config: it warns instead of failing the run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckPragma {
    /// The check id named by the pragma, verbatim.
    pub check: String,
    /// The optional quoted reason.
    pub reason: Option<String>,
}

/// Scan raw model SQL for `-- cute-dbt: ignore(check-id, "reason")`
/// pragmas, in line order.
///
/// Grammar (whitespace-tolerant):
///
/// ```text
/// pragma  := "--" ws? "cute-dbt:" ws? "ignore" ws? "(" ws? id
///            ( ws? "," ws? '"' reason '"' )? ws? ")"
/// id      := [^,)"]+ (trimmed; the dotted check id)
/// reason  := [^"]*  (may contain ')' — the quotes delimit it)
/// ```
///
/// The pragma is recognized after any `--` on a line — full-line or
/// trailing (the sqlfluff `-- noqa` placement) — and applies
/// **model-wide** regardless of position (file-level granularity).
/// Anything after the closing `)` on the line is ignored. Lines whose
/// comment is not a cute-dbt pragma are skipped silently.
#[must_use]
pub fn scan_pragmas(sql: &str) -> Vec<CheckPragma> {
    sql.lines()
        .filter_map(|line| {
            line.match_indices("--")
                .find_map(|(at, _)| parse_pragma(&line[at + 2..]))
        })
        .collect()
}

/// Parse the comment body after `--` as a pragma, or `None`.
///
/// Parsed structurally rather than by `find(')')` over the whole call:
/// the quoted reason is consumed as a unit first, so a `)` *inside* the
/// quotes is part of the reason, never the closing parenthesis.
fn parse_pragma(comment: &str) -> Option<CheckPragma> {
    let rest = comment.trim_start().strip_prefix("cute-dbt:")?;
    let rest = rest.trim_start().strip_prefix("ignore")?;
    let rest = rest.trim_start().strip_prefix('(')?;
    // The id runs to the first ',' (a reason follows) or ')' (no reason).
    let id_end = rest.find([',', ')'])?;
    let check = rest[..id_end].trim();
    if check.is_empty() || check.contains('"') {
        return None;
    }
    let reason = if rest[id_end..].starts_with(',') {
        let after_comma = rest[id_end + 1..].trim_start();
        let inner = after_comma.strip_prefix('"')?;
        let quote_end = inner.find('"')?;
        // Only the closing ')' may follow the quoted reason.
        if !inner[quote_end + 1..].trim_start().starts_with(')') {
            return None;
        }
        Some(inner[..quote_end].to_owned())
    } else {
        None
    };
    Some(CheckPragma {
        check: check.to_owned(),
        reason,
    })
}

// ---------------------------------------------------------------------
// Policy application — the grown filter_for_display stage.
// ---------------------------------------------------------------------

/// `true` when a suppression rule's `model` names the finding's model:
/// the full node id verbatim, the bare (leaf) model name, or — for a
/// VERSIONED node id, whose leaf is the `.vN` version suffix
/// (`model.<pkg>.<name>.v<N>`, the wire grammar both engines emit;
/// cute-dbt#256) — the authored model name before the suffix, so one
/// rule reaches every version. Purely additive over the pre-#256
/// matches; a model genuinely named `v2` (a 3-segment id) still matches
/// only by its own leaf.
fn model_matches(rule_model: &str, model_id: &str) -> bool {
    if model_id == rule_model {
        return true;
    }
    let mut segments = model_id.rsplit('.');
    let leaf = segments.next().unwrap_or(model_id);
    if leaf == rule_model {
        return true;
    }
    // The versioned-id arm applies only to ≥4-segment ids
    // (resource.package.name.vN): `segments` must still hold the
    // package + resource segments after taking the candidate name.
    is_version_suffix(leaf)
        && segments.clone().count() >= 3
        && segments.next().is_some_and(|name| name == rule_model)
}

/// `true` for a `v<digits>` id segment — dbt's version-suffix shape.
fn is_version_suffix(segment: &str) -> bool {
    segment
        .strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Stage 3 with a config surface (cute-dbt#171) — apply the resolved
/// display policy to already-resolved findings:
///
/// 1. **Selection** — remove findings of non-displayed checks
///    (delegates to [`filter_for_display`]);
/// 2. **Suppression** — mark (never remove) findings matched by a
///    [`SuppressRule`] with [`Finding::suppressed`], carrying the
///    rule's reason + source into the payload.
///
/// Runs strictly **after** [`resolve_supersedes`] — the cute-dbt#186
/// invariant extends here: disabling or suppressing a superseding
/// check never resurrects the finding it superseded, because the
/// superseded finding was already dropped during resolution and this
/// stage only ever removes or marks.
///
/// [`resolve_supersedes`]: crate::domain::checks::resolve_supersedes
#[must_use]
pub fn apply_check_policy<Id: CheckId>(
    findings: Vec<Finding<Id>>,
    policy: &CheckPolicy<Id>,
) -> Vec<Finding<Id>> {
    let mut displayed = filter_for_display(findings, &policy.disabled());
    for finding in &mut displayed {
        let rule = policy.suppressions.iter().find(|rule| {
            rule.check == finding.check && model_matches(&rule.model, finding.model_id.as_str())
        });
        if let Some(rule) = rule {
            finding.suppressed = Some(Suppression {
                source: rule.source,
                reason: rule.reason.clone(),
            });
        }
    }
    displayed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::checks::{
        CheckContext, HeuristicId, HeuristicSpec, Instrument, Tier, Verdict, resolve_supersedes,
    };
    use crate::domain::manifest::NodeId;

    // ===== synthetic registry (hand-impl'd CheckId) ===================
    //
    // The production registry holds one check, so glob expansion +
    // multi-check selection need a synthetic registry. The trait is
    // implemented by hand here (the `heuristics!` macro is private to
    // checks.rs); detectors are irrelevant to policy tests — findings
    // are built directly via `Finding::new`.

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum PolicyTestId {
        JoinGeneral,
        JoinSpecific,
        CaseUnrelated,
    }

    const fn spec_of(
        id: PolicyTestId,
        id_str: &'static str,
        group: &'static str,
        supersedes: &'static [PolicyTestId],
    ) -> HeuristicSpec<PolicyTestId> {
        HeuristicSpec {
            id,
            id_str,
            name: "synthetic",
            group,
            tier: Tier::Advisory,
            instrument: Instrument::Both,
            supersedes,
            evidence: &["none"],
            conditions: &["synthetic"],
            exclusions: &[],
            recommendation: "synthetic fix",
            rationale: "synthetic rationale",
        }
    }

    impl CheckId for PolicyTestId {
        const ALL: &'static [Self] = &[Self::JoinGeneral, Self::JoinSpecific, Self::CaseUnrelated];
        const SPECS: &'static [HeuristicSpec<Self>] = &[
            spec_of(Self::JoinGeneral, "join.general", "join", &[]),
            spec_of(
                Self::JoinSpecific,
                "join.specific",
                "join",
                &[Self::JoinGeneral],
            ),
            spec_of(Self::CaseUnrelated, "case.unrelated", "case", &[]),
        ];

        fn spec(self) -> &'static HeuristicSpec<Self> {
            &Self::SPECS[self as usize]
        }

        fn detect(self, _ctx: &CheckContext<'_>) -> Vec<Finding<Self>> {
            Vec::new()
        }
    }

    fn finding(check: PolicyTestId, model: &str, construct: &str) -> Finding<PolicyTestId> {
        Finding::new(
            check,
            NodeId::new(model),
            construct,
            Verdict::Uncovered,
            Vec::new(),
        )
    }

    fn checks_of(findings: &[Finding<PolicyTestId>]) -> Vec<PolicyTestId> {
        findings.iter().map(|f| f.check).collect()
    }

    fn config(toml: &str) -> ChecksConfig {
        toml::from_str(toml).expect("test TOML parses")
    }

    fn resolve(toml: &str) -> Result<CheckPolicy<PolicyTestId>, CheckConfigError> {
        resolve_check_policy::<PolicyTestId>(&config(toml))
    }

    // ===== ChecksConfig serde =========================================

    #[test]
    fn empty_config_defaults_to_opt_out_with_nothing_listed() {
        let cfg = config("");
        assert_eq!(cfg.mode, ChecksMode::OptOut);
        assert!(cfg.enable.is_none());
        assert!(cfg.disable.is_none());
        assert!(cfg.suppress.is_empty());
    }

    #[test]
    fn modes_deserialize_from_kebab_case() {
        assert_eq!(config("mode = \"opt-out\"").mode, ChecksMode::OptOut);
        assert_eq!(config("mode = \"opt-in\"").mode, ChecksMode::OptIn);
    }

    #[test]
    fn unknown_mode_is_a_serde_error() {
        let err = toml::from_str::<ChecksConfig>("mode = \"allow\"").expect_err("unknown mode");
        assert!(err.to_string().contains("opt-out") || err.to_string().contains("variant"));
    }

    #[test]
    fn suppress_entries_deserialize_with_all_three_fields() {
        let cfg = config(
            r#"
[[suppress]]
check = "join.general"
model = "orders"
reason = "we know and don't care"
"#,
        );
        assert_eq!(
            cfg.suppress,
            vec![SuppressEntry {
                check: "join.general".to_owned(),
                model: "orders".to_owned(),
                reason: "we know and don't care".to_owned(),
            }],
        );
    }

    #[test]
    fn suppress_entry_missing_reason_is_a_serde_error() {
        let err = toml::from_str::<ChecksConfig>(
            r#"
[[suppress]]
check = "join.general"
model = "orders"
"#,
        )
        .expect_err("missing reason");
        assert!(err.to_string().contains("reason"), "{err}");
    }

    #[test]
    fn unknown_checks_field_is_a_serde_error() {
        let err = toml::from_str::<ChecksConfig>("enabel = [\"join.*\"]").expect_err("typo");
        let msg = err.to_string();
        assert!(
            msg.contains("enabel") || msg.contains("unknown field"),
            "{msg}"
        );
    }

    // ===== mode / cross-field validation ==============================

    #[test]
    fn default_config_displays_every_registered_check() {
        let policy = resolve("").expect("default resolves");
        assert_eq!(policy.displayed, PolicyTestId::ALL.to_vec());
        assert!(policy.suppressions.is_empty());
        assert_eq!(policy, CheckPolicy::default());
    }

    #[test]
    fn enable_in_opt_out_mode_is_an_error() {
        let err = resolve("enable = [\"join.general\"]").expect_err("enable needs opt-in");
        assert_eq!(err, CheckConfigError::EnableRequiresOptIn);
        assert!(err.to_string().contains("opt-in"), "{err}");
    }

    #[test]
    fn disable_in_opt_in_mode_is_an_error() {
        let err = resolve("mode = \"opt-in\"\ndisable = [\"join.general\"]")
            .expect_err("disable needs opt-out");
        assert_eq!(err, CheckConfigError::DisableRequiresOptOut);
        assert!(err.to_string().contains("opt-out"), "{err}");
    }

    #[test]
    fn opt_in_without_enable_is_an_error() {
        let err = resolve("mode = \"opt-in\"").expect_err("opt-in needs enable");
        assert_eq!(err, CheckConfigError::OptInRequiresEnable);
        assert!(err.to_string().contains("enable"), "{err}");
    }

    #[test]
    fn opt_in_with_empty_enable_displays_nothing() {
        let policy = resolve("mode = \"opt-in\"\nenable = []").expect("explicit empty");
        assert!(policy.displayed.is_empty());
    }

    #[test]
    fn opt_out_disable_removes_exactly_the_listed_checks() {
        let policy = resolve("disable = [\"join.specific\"]").expect("resolves");
        assert_eq!(
            policy.displayed,
            vec![PolicyTestId::JoinGeneral, PolicyTestId::CaseUnrelated],
        );
    }

    #[test]
    fn opt_in_enable_displays_exactly_the_listed_checks() {
        let policy = resolve("mode = \"opt-in\"\nenable = [\"case.unrelated\"]").expect("resolves");
        assert_eq!(policy.displayed, vec![PolicyTestId::CaseUnrelated]);
    }

    // ===== glob resolution (fail-closed) ==============================

    #[test]
    fn group_glob_expands_to_every_check_in_the_group() {
        let policy = resolve("mode = \"opt-in\"\nenable = [\"join.*\"]").expect("resolves");
        assert_eq!(
            policy.displayed,
            vec![PolicyTestId::JoinGeneral, PolicyTestId::JoinSpecific],
        );
    }

    #[test]
    fn glob_and_exact_entries_union_in_declaration_order() {
        let policy = resolve("mode = \"opt-in\"\nenable = [\"case.unrelated\", \"join.*\"]")
            .expect("resolves");
        // Declaration order, not entry order.
        assert_eq!(policy.displayed, PolicyTestId::ALL.to_vec());
    }

    #[test]
    fn unknown_check_id_is_an_error_with_remediation() {
        let err = resolve("disable = [\"join.nonexistent\"]").expect_err("unknown id");
        let msg = err.to_string();
        assert!(msg.contains("join.nonexistent"), "{msg}");
        assert!(msg.contains("join.general"), "names known checks: {msg}");
        assert!(msg.contains("case"), "names known groups: {msg}");
    }

    #[test]
    fn unknown_group_glob_is_an_error_with_remediation() {
        let err = resolve("disable = [\"grain.*\"]").expect_err("unknown group");
        let msg = err.to_string();
        assert!(msg.contains("grain.*"), "{msg}");
        assert!(msg.contains("join"), "names known groups: {msg}");
    }

    #[test]
    fn star_entries_outside_group_glob_form_error() {
        for entry in ["*", "join.*.deep", "jo*n.general", "*.general"] {
            let err =
                resolve(&format!("disable = [\"{entry}\"]")).expect_err("unsupported pattern");
            match err {
                CheckConfigError::UnsupportedGlob { .. }
                | CheckConfigError::UnknownGroup { .. } => {}
                other => panic!("{entry:?}: expected glob rejection, got {other:?}"),
            }
        }
    }

    #[test]
    fn production_registry_resolves_every_id_and_group() {
        // Sanity over the REAL registry, shape-robust: EVERY registered
        // check's exact id and group glob resolve, and disabling one
        // removes exactly that id / that group — nothing else. Extends
        // automatically as checks join the registry (union.arm-coverage
        // arrived with cute-dbt#172/#191).
        for spec in HeuristicId::SPECS {
            let cfg = config(&format!("disable = [\"{}\"]", spec.id_str));
            let policy = resolve_check_policy::<HeuristicId>(&cfg).expect("exact id resolves");
            let expected: Vec<HeuristicId> = HeuristicId::ALL
                .iter()
                .copied()
                .filter(|id| *id != spec.id)
                .collect();
            assert_eq!(
                policy.displayed, expected,
                "{} removes itself only",
                spec.id_str
            );

            let cfg = config(&format!("disable = [\"{}.*\"]", spec.group));
            let policy = resolve_check_policy::<HeuristicId>(&cfg).expect("group glob resolves");
            let expected: Vec<HeuristicId> = HeuristicId::ALL
                .iter()
                .copied()
                .filter(|id| id.spec().group != spec.group)
                .collect();
            assert_eq!(
                policy.displayed, expected,
                "{}.* removes exactly its group",
                spec.group
            );
        }
    }

    // ===== suppress entry validation ==================================

    #[test]
    fn suppress_entry_resolves_to_a_config_sourced_rule() {
        let policy = resolve(
            r#"
[[suppress]]
check = "join.general"
model = "orders"
reason = "known and accepted"
"#,
        )
        .expect("resolves");
        assert_eq!(
            policy.suppressions,
            vec![SuppressRule {
                check: PolicyTestId::JoinGeneral,
                model: "orders".to_owned(),
                reason: Some("known and accepted".to_owned()),
                source: SuppressionSource::Config,
            }],
        );
        // Suppression never narrows the displayed set.
        assert_eq!(policy.displayed, PolicyTestId::ALL.to_vec());
    }

    #[test]
    fn suppress_with_unknown_check_is_an_error() {
        let err = resolve(
            r#"
[[suppress]]
check = "join.nope"
model = "orders"
reason = "r"
"#,
        )
        .expect_err("unknown suppress check");
        assert!(matches!(
            err,
            CheckConfigError::UnknownCheck {
                key: "suppress",
                ..
            }
        ));
    }

    #[test]
    fn suppress_with_a_glob_is_an_error() {
        let err = resolve(
            r#"
[[suppress]]
check = "join.*"
model = "orders"
reason = "r"
"#,
        )
        .expect_err("glob in suppress");
        assert_eq!(
            err,
            CheckConfigError::SuppressTakesExactId {
                entry: "join.*".to_owned(),
            },
        );
    }

    #[test]
    fn suppress_with_blank_reason_is_an_error() {
        let err = resolve(
            r#"
[[suppress]]
check = "join.general"
model = "orders"
reason = "   "
"#,
        )
        .expect_err("blank reason");
        assert!(matches!(err, CheckConfigError::EmptySuppressReason { .. }));
        assert!(err.to_string().contains("say why"), "{err}");
    }

    // ===== pragma scanning ============================================

    #[test]
    fn pragma_with_reason_parses() {
        let pragmas = scan_pragmas(
            "-- cute-dbt: ignore(grain.unique-key-unbacked, \"backfill dupes\")\nselect 1",
        );
        assert_eq!(
            pragmas,
            vec![CheckPragma {
                check: "grain.unique-key-unbacked".to_owned(),
                reason: Some("backfill dupes".to_owned()),
            }],
        );
    }

    #[test]
    fn pragma_without_reason_parses_with_none() {
        let pragmas = scan_pragmas("--cute-dbt: ignore(join.general)");
        assert_eq!(
            pragmas,
            vec![CheckPragma {
                check: "join.general".to_owned(),
                reason: None,
            }],
        );
    }

    #[test]
    fn pragma_is_whitespace_tolerant() {
        let pragmas = scan_pragmas("--   cute-dbt:   ignore (  join.general  ,  \"r\"  )  ");
        assert_eq!(
            pragmas,
            vec![CheckPragma {
                check: "join.general".to_owned(),
                reason: Some("r".to_owned()),
            }],
        );
    }

    #[test]
    fn trailing_pragma_after_code_parses() {
        // The sqlfluff `-- noqa` placement: trailing on a code line.
        let pragmas = scan_pragmas("select 1 as id -- cute-dbt: ignore(join.general, \"known\")");
        assert_eq!(pragmas.len(), 1);
        assert_eq!(pragmas[0].check, "join.general");
    }

    #[test]
    fn multiple_pragmas_scan_in_line_order() {
        let sql = "-- cute-dbt: ignore(join.general)\n\
                   select 1\n\
                   -- cute-dbt: ignore(case.unrelated, \"second\")";
        let pragmas = scan_pragmas(sql);
        assert_eq!(pragmas.len(), 2);
        assert_eq!(pragmas[0].check, "join.general");
        assert_eq!(pragmas[1].check, "case.unrelated");
    }

    #[test]
    fn non_pragma_comments_and_lookalikes_are_skipped() {
        for sql in [
            "-- a plain comment",
            "-- cute-dbt: something-else(join.general)",
            "-- cute-dbt: ignore join.general",     // no parens
            "-- cute-dbt: ignore(join.general",     // unclosed
            "-- cute-dbt: ignore()",                // empty id
            "-- cute-dbt: ignore(join.general, r)", // unquoted reason
            "select '--cute-dbt' from t",           // no ignore() after the dashes
        ] {
            assert!(
                scan_pragmas(sql).is_empty(),
                "{sql:?} must not parse as a pragma: {:?}",
                scan_pragmas(sql),
            );
        }
    }

    #[test]
    fn malformed_reason_quoting_is_not_a_pragma() {
        for sql in [
            "-- cute-dbt: ignore(join.general, \"half)",
            // Junk between the quoted reason and the closing paren.
            "-- cute-dbt: ignore(join.general, \"r\" x)",
        ] {
            assert!(scan_pragmas(sql).is_empty(), "{sql:?}");
        }
    }

    #[test]
    fn reason_containing_a_closing_paren_parses_whole() {
        // The quotes delimit the reason — a ')' inside them is reason
        // text, not the call's closing parenthesis (PR #193 review).
        let pragmas =
            scan_pragmas("-- cute-dbt: ignore(join.general, \"see RFC-12 (appendix B)\")");
        assert_eq!(
            pragmas,
            vec![CheckPragma {
                check: "join.general".to_owned(),
                reason: Some("see RFC-12 (appendix B)".to_owned()),
            }],
        );
    }

    // ===== policy application =========================================

    fn resolved_pair() -> Vec<Finding<PolicyTestId>> {
        // General + Specific fired on the same construct; resolution
        // drops General (Specific supersedes it).
        let evaluated = vec![
            finding(PolicyTestId::JoinGeneral, "model.shop.orders", "join#1"),
            finding(PolicyTestId::JoinSpecific, "model.shop.orders", "join#1"),
        ];
        let resolved = resolve_supersedes(evaluated);
        assert_eq!(checks_of(&resolved), vec![PolicyTestId::JoinSpecific]);
        resolved
    }

    #[test]
    fn default_policy_is_a_no_op() {
        let resolved = resolved_pair();
        let applied = apply_check_policy(resolved.clone(), &CheckPolicy::default());
        assert_eq!(applied, resolved);
    }

    /// THE cute-dbt#171 invariant extension (of cute-dbt#186's
    /// disable test): SUPPRESSING the superseding check must not
    /// resurrect the superseded finding — suppression only marks, and
    /// it runs after resolution.
    #[test]
    fn suppressing_the_superseding_check_does_not_resurrect_the_superseded_finding() {
        let policy = CheckPolicy {
            displayed: PolicyTestId::ALL.to_vec(),
            suppressions: vec![SuppressRule {
                check: PolicyTestId::JoinSpecific,
                model: "orders".to_owned(),
                reason: Some("accepted".to_owned()),
                source: SuppressionSource::Config,
            }],
        };
        let applied = apply_check_policy(resolved_pair(), &policy);
        assert_eq!(
            checks_of(&applied),
            vec![PolicyTestId::JoinSpecific],
            "General must stay dropped; Specific stays present (marked)"
        );
        assert_eq!(
            applied[0].suppressed,
            Some(Suppression {
                source: SuppressionSource::Config,
                reason: Some("accepted".to_owned()),
            }),
        );
    }

    /// The disable arm of the same invariant, through the CONFIG path:
    /// disabling the superseding check removes its finding without
    /// resurrecting the superseded one.
    #[test]
    fn disabling_the_superseding_check_via_config_does_not_resurrect() {
        let policy = resolve_check_policy::<PolicyTestId>(&config("disable = [\"join.specific\"]"))
            .expect("resolves");
        let applied = apply_check_policy(resolved_pair(), &policy);
        assert!(
            applied.is_empty(),
            "disabling Specific removes it WITHOUT resurrecting General; got {applied:?}"
        );
    }

    /// Selection/suppression never reach evaluation or resolution: the
    /// pipeline stages upstream of `apply_check_policy` are
    /// policy-blind by construction (their signatures take no policy),
    /// and applying any policy never grows the finding set.
    #[test]
    fn policy_application_only_removes_or_marks_never_adds() {
        let resolved = resolved_pair();
        for policy in [
            CheckPolicy::default(),
            CheckPolicy {
                displayed: vec![PolicyTestId::JoinGeneral],
                suppressions: Vec::new(),
            },
            CheckPolicy {
                displayed: PolicyTestId::ALL.to_vec(),
                suppressions: vec![SuppressRule {
                    check: PolicyTestId::JoinSpecific,
                    model: "model.shop.orders".to_owned(),
                    reason: None,
                    source: SuppressionSource::Pragma,
                }],
            },
        ] {
            let applied = apply_check_policy(resolved.clone(), &policy);
            assert!(applied.len() <= resolved.len());
            for finding in &applied {
                let mut unmarked = finding.clone();
                unmarked.suppressed = None;
                assert!(
                    resolved.contains(&unmarked),
                    "applied finding must be an (optionally marked) resolved finding"
                );
            }
        }
    }

    #[test]
    fn suppression_matches_the_full_node_id() {
        let policy = CheckPolicy {
            displayed: PolicyTestId::ALL.to_vec(),
            suppressions: vec![SuppressRule {
                check: PolicyTestId::CaseUnrelated,
                model: "model.shop.orders".to_owned(),
                reason: Some("r".to_owned()),
                source: SuppressionSource::Config,
            }],
        };
        let applied = apply_check_policy(
            vec![finding(
                PolicyTestId::CaseUnrelated,
                "model.shop.orders",
                "c#1",
            )],
            &policy,
        );
        assert!(applied[0].suppressed.is_some());
    }

    #[test]
    fn suppression_does_not_match_other_models_or_other_checks() {
        let policy = CheckPolicy {
            displayed: PolicyTestId::ALL.to_vec(),
            suppressions: vec![SuppressRule {
                check: PolicyTestId::CaseUnrelated,
                model: "orders".to_owned(),
                reason: Some("r".to_owned()),
                source: SuppressionSource::Config,
            }],
        };
        let applied = apply_check_policy(
            vec![
                finding(PolicyTestId::CaseUnrelated, "model.shop.customers", "c#1"),
                finding(PolicyTestId::JoinGeneral, "model.shop.orders", "j#1"),
            ],
            &policy,
        );
        assert!(
            applied.iter().all(|f| f.suppressed.is_none()),
            "neither a different model nor a different check matches: {applied:?}"
        );
    }

    #[test]
    fn bare_model_name_matches_a_versioned_node_id() {
        // cute-dbt#256 (the #254 handoff): a versioned model's id leaf
        // is the `.vN` version suffix (`model.<pkg>.<name>.v<N>` — the
        // wire grammar both engines emit; live-verified
        // `model.jaffle_shop.versioned_demo.v2`). A suppression naming
        // the authored model must reach every version of it.
        assert!(model_matches(
            "dim_customers",
            "model.shop.dim_customers.v2"
        ));
        // The pre-#256 behaviors stay additive: the literal leaf and the
        // full id still match.
        assert!(model_matches("v2", "model.shop.dim_customers.v2"));
        assert!(model_matches(
            "model.shop.dim_customers.v2",
            "model.shop.dim_customers.v2"
        ));
        // A version-suffix-LOOKING leaf on a 3-segment id is a model
        // genuinely named v2 — its package segment must not match.
        assert!(model_matches("v2", "model.shop.v2"));
        assert!(!model_matches("shop", "model.shop.v2"));
        // Non-version leaves never expose the prior segment.
        assert!(!model_matches("dim_customers", "model.shop.other"));
        assert!(!model_matches(
            "dim_customers",
            "model.shop.dim_customers.final"
        ));
    }

    #[test]
    fn bare_model_name_does_not_match_a_mid_segment() {
        // `shop` is the package segment, not the leaf — must not match.
        assert!(!model_matches("shop", "model.shop.orders"));
        assert!(model_matches("orders", "model.shop.orders"));
        assert!(model_matches("model.shop.orders", "model.shop.orders"));
    }

    #[test]
    fn suppressed_finding_serializes_reason_and_source() {
        let policy = CheckPolicy {
            displayed: PolicyTestId::ALL.to_vec(),
            suppressions: vec![SuppressRule {
                check: PolicyTestId::JoinGeneral,
                model: "orders".to_owned(),
                reason: Some("known dupes".to_owned()),
                source: SuppressionSource::Config,
            }],
        };
        let applied = apply_check_policy(
            vec![finding(
                PolicyTestId::JoinGeneral,
                "model.shop.orders",
                "j#1",
            )],
            &policy,
        );
        let json = serde_json::to_value(&applied[0]).expect("serializes");
        assert_eq!(json["suppressed"]["source"], "config");
        assert_eq!(json["suppressed"]["reason"], "known dupes");
    }

    #[test]
    fn pragma_sourced_suppression_serializes_without_reason_key() {
        let policy = CheckPolicy {
            displayed: PolicyTestId::ALL.to_vec(),
            suppressions: vec![SuppressRule {
                check: PolicyTestId::JoinGeneral,
                model: "model.shop.orders".to_owned(),
                reason: None,
                source: SuppressionSource::Pragma,
            }],
        };
        let applied = apply_check_policy(
            vec![finding(
                PolicyTestId::JoinGeneral,
                "model.shop.orders",
                "j#1",
            )],
            &policy,
        );
        let json = serde_json::to_value(&applied[0]).expect("serializes");
        assert_eq!(json["suppressed"]["source"], "pragma");
        assert!(
            json["suppressed"].get("reason").is_none(),
            "absent pragma reason must be serde-skipped: {json}"
        );
    }

    #[test]
    fn unsuppressed_finding_omits_the_suppressed_key() {
        let json = serde_json::to_value(finding(
            PolicyTestId::JoinGeneral,
            "model.shop.orders",
            "j#1",
        ))
        .expect("serializes");
        assert!(
            json.get("suppressed").is_none(),
            "None suppression must be serde-skipped: {json}"
        );
    }

    #[test]
    fn check_by_id_resolves_exact_ids_only() {
        assert_eq!(
            check_by_id::<PolicyTestId>("join.general"),
            Some(PolicyTestId::JoinGeneral)
        );
        assert_eq!(check_by_id::<PolicyTestId>("join.*"), None);
        assert_eq!(check_by_id::<PolicyTestId>("nope"), None);
    }
}
