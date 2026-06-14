//! Experimental feature opt-in — the one named switch (cute-dbt#289,
//! epic #288).
//!
//! Two surfaces resolve to a single enabled set:
//!
//! - the `[experimental]` table of the `--config <PATH>` TOML
//!   ([`ExperimentalConfig`], an additive POD section on
//!   [`crate::domain::AnalysisConfig`] — the `[checks]` precedent), and
//! - the `CUTE_DBT_EXPERIMENTAL` environment variable
//!   ([`parse_experimental_env`]), read by the cli layer.
//!
//! **Union semantics**: enabled = TOML set ∪ env set
//! ([`EnabledExperiments::from_union`]). Either surface alone is
//! sufficient to enable an experiment; neither can disable what the
//! other enabled.
//!
//! The vocabulary is **closed and parse-time validated**
//! ([`Experiment::ALL`]): an unknown id on either surface is a clap
//! usage error (exit 2) with remediation text naming the known ids —
//! the `[checks]` fail-closed posture, never a
//! [`crate::domain::PreflightError`] variant.
//!
//! Mechanism only in cute-dbt#289: the resolved set threads through the
//! `report` run loop but nothing consumes it yet. The first consumer is
//! the project-state render gate (cute-dbt#291). `explore` takes no
//! gate at all (founder call 2026-06-12: runnable is fine, just not
//! headlined).

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;

/// One experiment id in the closed vocabulary.
///
/// v0.x vocabulary: exactly `project-state` (the cute-dbt#266–#269
/// project-definition surfaces plus the cute-dbt#267 scope widening).
/// New experiments are additive variants here plus an [`Experiment::ALL`]
/// entry — never a resolution rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Experiment {
    /// The project-state surfaces: the "Project definition changed"
    /// panel (categorized + fallback), per-model config attributions,
    /// vars/hooks/dispatch rows, and the cute-dbt#267 config-tree scope
    /// widening.
    ProjectState,
    /// The PR-review governance surfaces (cute-dbt#260, epic #260): the
    /// group/owner header chips, the reverse-reachability exposure /
    /// blast-radius panel, the classified contract-diff drawer, the
    /// enforcement-reality annotation, and the access/deprecation/version
    /// lifecycle chips. Every surface is render over already-parsed wire
    /// data, gated empty so the released golden never moves.
    Governance,
    /// The macro perspective lens (cute-dbt#265, epic #265): the
    /// "macro changed" section — the changed macro's body diff, the
    /// collapsible directory tree of the impacted root-project models (the
    /// reverse [`macro_blast_radius`](crate::domain::macro_blast_radius)),
    /// the impacted-model count, and a per-arm fidelity chip (baseline =
    /// exact macro-body comparison; pr-diff = path/name heuristic). Render
    /// over already-parsed wire data + the PR diff index; gated empty so
    /// the non-macro goldens stay byte-identical.
    MacroLens,
    /// The seed-data surfaces (cute-dbt#350, epic #350): the "Data tables"
    /// section's seed CONTENT — each in-scope seed's current data table
    /// (row-capped, with an honest "showing N of M rows" label) plus its
    /// old→new cell-diff on the pr-diff arm, rendered via the vendored
    /// `DataTables` + the #98/#127 NULL-aware cell-diff engine. The seed
    /// payload is gathered unconditionally at the data layer (#367/#370);
    /// this experiment gates whether it CROSSES to the render payload — the
    /// cli passes an empty `seed_cards` vec when off, so the section emits
    /// zero DOM (`DATA.seed_cards` absent) and every seed-free golden stays
    /// byte-identical. The "Data tables" section is seed-only + gated, so
    /// the default goldens never move (no pre-existing label is renamed).
    Seeds,
    /// The PR-scope lineage mini-DAG (cute-dbt#404, epic #352): the focused
    /// cross-model lineage subgraph at the top of the report — the models
    /// the PR modified (emphasized), the connectors between them (a quiet
    /// tier), and the deleted models (ghosts), each with its lines ± chip.
    /// Built from the already-computed scope sets via
    /// [`compute_pr_dag`](crate::domain::compute_pr_dag) (Slice A) + the
    /// per-node line counts (Slice B); gated empty so the cli passes `None`
    /// when off, the `{% match pr_dag %}` section emits zero bytes, and
    /// every default golden stays byte-identical (the `macro_lens` /
    /// governance / seeds precedent).
    PrScopeMiniDag,
}

impl Experiment {
    /// Every registered experiment, in declaration order — the closed
    /// vocabulary both opt-in surfaces validate against.
    pub const ALL: &'static [Experiment] = &[
        Experiment::ProjectState,
        Experiment::Governance,
        Experiment::MacroLens,
        Experiment::Seeds,
        Experiment::PrScopeMiniDag,
    ];

    /// The kebab-case wire id this experiment is named by in the
    /// `[experimental]` TOML list and the `CUTE_DBT_EXPERIMENTAL` env
    /// list.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::ProjectState => "project-state",
            Self::Governance => "governance",
            Self::MacroLens => "macro-lens",
            Self::Seeds => "seeds",
            Self::PrScopeMiniDag => "pr-scope-mini-dag",
        }
    }

    /// Resolve a wire id back to its experiment. `None` for anything
    /// outside the closed vocabulary — the caller raises the
    /// fail-closed [`ExperimentalError`].
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.id() == id)
    }
}

/// The default number of impacted-model bodies the macro lens inlines
/// before falling back to the lightweight tree-only affordance
/// (cute-dbt#265 Slice D, founder D5).
///
/// A widely-used macro can reach 50+ models; server-rendering every
/// impacted model's SQL panel would bloat the (frozen, single-file)
/// report. The cap bounds the heavy surface — the first
/// `DEFAULT_MACRO_BODY_CAP` impacted models (in id order) carry a
/// server-rendered inline SQL + call-site panel; the rest show a
/// "body not inlined — showing N of M" affordance (the model-selector
/// still lists ALL impacted models — that list is cheap). The cap is a
/// **gen-time knob**, not a post-gen HTML toggle: the report is static
/// once rendered, so the number of inlined bodies is fixed at
/// generation time via `[experimental] macro_body_cap` (or the
/// `--macro-body-cap` flag).
pub const DEFAULT_MACRO_BODY_CAP: usize = 10;

/// `[experimental]` table of the `--config` TOML — an additive POD
/// section on [`crate::domain::AnalysisConfig`].
///
/// Keys:
/// - `enable`, a list of exact experiment ids
///   (`enable = ["project-state"]`). No globs, no `"all"` — the TOML is
///   authored config, so it names experiments precisely; the `1`/`all`
///   shorthand is env-var-only ergonomics. An absent table (or an empty
///   list) enables nothing.
/// - `macro_body_cap`, an optional positive integer bounding how many
///   impacted-model bodies the macro lens inlines (cute-dbt#265 Slice D,
///   founder D5). Absent ⇒ [`DEFAULT_MACRO_BODY_CAP`] (resolved at the
///   cli I/O boundary). Only meaningful with the `macro-lens` experiment
///   on; inert otherwise.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExperimentalConfig {
    /// Exact experiment ids to enable. Unknown ids fail closed at
    /// `--config` parse time ([`resolve_experimental_config`]).
    #[serde(default)]
    pub enable: Vec<String>,
    /// The macro-lens inline-body cap (cute-dbt#265 Slice D, founder D5).
    /// `None` (the key omitted) ⇒ [`DEFAULT_MACRO_BODY_CAP`] at the cli
    /// boundary. A `usize` so a negative or non-integer value is a clap
    /// usage error at `--config` parse time (exit 2) — the
    /// fail-closed-config posture, never a [`crate::domain::PreflightError`].
    #[serde(default)]
    pub macro_body_cap: Option<usize>,
}

/// An `[experimental]` / `CUTE_DBT_EXPERIMENTAL` resolution failure.
/// Surfaced as a **clap usage error** (exit 2) by the cli layer — the
/// `[checks]` precedent: config errors are usage-time, never a
/// [`crate::domain::PreflightError`] variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExperimentalError {
    /// An entry matched no registered experiment id. Carries the
    /// offending entry plus the closed vocabulary for the remediation
    /// text.
    UnknownExperiment {
        /// The surface the entry came from (`[experimental] enable` /
        /// `CUTE_DBT_EXPERIMENTAL`).
        source: &'static str,
        /// The offending entry, verbatim.
        entry: String,
        /// The closed vocabulary, in declaration order.
        known: Vec<&'static str>,
    },
}

impl fmt::Display for ExperimentalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownExperiment {
                source,
                entry,
                known,
            } => write!(
                f,
                "{source} entry {entry:?} matches no registered \
                 experiment; known experiment ids: {}",
                known.join(", "),
            ),
        }
    }
}

impl std::error::Error for ExperimentalError {}

/// The resolved experimental opt-in set the `report` run loop threads
/// to its consumers (the cute-dbt#291 project-state gate is the first).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnabledExperiments {
    /// Every enabled experiment, deduplicated, in [`Ord`] order.
    pub enabled: BTreeSet<Experiment>,
}

impl EnabledExperiments {
    /// The union of the two opt-in surfaces: enabled = TOML set ∪ env
    /// set (dbt's OR posture — either surface alone suffices; neither
    /// can disable what the other enabled).
    #[must_use]
    pub fn from_union(toml: &BTreeSet<Experiment>, env: &BTreeSet<Experiment>) -> Self {
        Self {
            enabled: toml.union(env).copied().collect(),
        }
    }

    /// Whether `experiment` was opted into on either surface.
    #[must_use]
    pub fn is_enabled(&self, experiment: Experiment) -> bool {
        self.enabled.contains(&experiment)
    }
}

/// Resolve the `[experimental]` TOML section to the set it names.
///
/// Fail-closed at `--config` parse time (run by the cli value-parser,
/// the [`crate::domain::check_config::resolve_check_policy`] posture):
/// every entry must be an exact registered id.
///
/// # Errors
///
/// [`ExperimentalError::UnknownExperiment`] naming the offending entry
/// and the closed vocabulary.
pub fn resolve_experimental_config(
    config: &ExperimentalConfig,
) -> Result<BTreeSet<Experiment>, ExperimentalError> {
    resolve_ids(
        "[experimental] enable",
        config.enable.iter().map(String::as_str),
    )
}

/// Parse a `CUTE_DBT_EXPERIMENTAL` environment-variable value.
///
/// Accepted shapes:
///
/// - `1` or `all` — enable every registered experiment,
/// - a comma-separated list of exact experiment ids
///   (`project-state`); entries are trimmed and empty entries (a
///   trailing comma) are tolerated,
/// - an empty / whitespace-only value — enables nothing (so a CI
///   `CUTE_DBT_EXPERIMENTAL: ""` is a no-op, not an error).
///
/// Anything else fails closed — the TOML-arm posture (a usage error at
/// the cli layer, exit 2).
///
/// # Errors
///
/// [`ExperimentalError::UnknownExperiment`] naming the offending entry
/// and the closed vocabulary.
pub fn parse_experimental_env(value: &str) -> Result<BTreeSet<Experiment>, ExperimentalError> {
    let trimmed = value.trim();
    if trimmed == "1" || trimmed == "all" {
        return Ok(Experiment::ALL.iter().copied().collect());
    }
    resolve_ids(
        "CUTE_DBT_EXPERIMENTAL",
        trimmed.split(',').map(str::trim).filter(|e| !e.is_empty()),
    )
}

/// Resolve a list of wire ids against the closed vocabulary —
/// fail-closed on the first unknown entry.
fn resolve_ids<'a>(
    source: &'static str,
    entries: impl Iterator<Item = &'a str>,
) -> Result<BTreeSet<Experiment>, ExperimentalError> {
    let mut set = BTreeSet::new();
    for entry in entries {
        match Experiment::from_id(entry) {
            Some(experiment) => {
                set.insert(experiment);
            }
            None => {
                return Err(ExperimentalError::UnknownExperiment {
                    source,
                    entry: entry.to_owned(),
                    known: Experiment::ALL.iter().map(|e| e.id()).collect(),
                });
            }
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- vocabulary -----

    #[test]
    fn every_registered_experiment_round_trips_through_its_id() {
        for experiment in Experiment::ALL.iter().copied() {
            assert_eq!(
                Experiment::from_id(experiment.id()),
                Some(experiment),
                "id round-trip for {experiment:?}",
            );
        }
    }

    #[test]
    fn ids_are_kebab_case_and_unique() {
        let mut seen = BTreeSet::new();
        for experiment in Experiment::ALL.iter().copied() {
            let id = experiment.id();
            assert!(
                id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "kebab-case id: {id:?}",
            );
            assert!(seen.insert(id), "duplicate id: {id:?}");
        }
    }

    #[test]
    fn from_id_rejects_anything_outside_the_vocabulary() {
        assert_eq!(Experiment::from_id("project_state"), None);
        assert_eq!(Experiment::from_id("ProjectState"), None);
        assert_eq!(Experiment::from_id(""), None);
        assert_eq!(Experiment::from_id("all"), None);
    }

    #[test]
    fn governance_is_a_registered_experiment_with_its_wire_id() {
        // cute-dbt#260 Slice 0: the gating seam for the governance
        // surfaces. The id round-trips and the variant is in the closed
        // vocabulary (so `1`/`all` enables it on the diff-showcase row).
        assert_eq!(Experiment::Governance.id(), "governance");
        assert_eq!(
            Experiment::from_id("governance"),
            Some(Experiment::Governance)
        );
        assert!(Experiment::ALL.contains(&Experiment::Governance));
    }

    #[test]
    fn macro_lens_is_a_registered_experiment_with_its_wire_id() {
        // cute-dbt#265 Slice B: the gating seam for the macro perspective
        // section. The id round-trips and the variant is in the closed
        // vocabulary (so `1`/`all` enables it on the diff-showcase row).
        assert_eq!(Experiment::MacroLens.id(), "macro-lens");
        assert_eq!(
            Experiment::from_id("macro-lens"),
            Some(Experiment::MacroLens)
        );
        assert!(Experiment::ALL.contains(&Experiment::MacroLens));
    }

    #[test]
    fn seeds_is_a_registered_experiment_with_its_wire_id() {
        // cute-dbt#350 — the gating seam for the "Data tables" seed
        // content. The id round-trips and the variant is in the closed
        // vocabulary (so `1`/`all` enables it on the seed-showcase row).
        assert_eq!(Experiment::Seeds.id(), "seeds");
        assert_eq!(Experiment::from_id("seeds"), Some(Experiment::Seeds));
        assert!(Experiment::ALL.contains(&Experiment::Seeds));
    }

    #[test]
    fn pr_scope_mini_dag_is_a_registered_experiment_with_its_wire_id() {
        // cute-dbt#404 (epic #352) — the gating seam for the PR-scope
        // lineage mini-DAG at the report top. The id round-trips and the
        // variant is in the closed vocabulary (so `1`/`all` enables it on
        // the prdiff-minidag golden row).
        assert_eq!(Experiment::PrScopeMiniDag.id(), "pr-scope-mini-dag");
        assert_eq!(
            Experiment::from_id("pr-scope-mini-dag"),
            Some(Experiment::PrScopeMiniDag)
        );
        assert!(Experiment::ALL.contains(&Experiment::PrScopeMiniDag));
    }

    // ----- [experimental] TOML resolution -----

    #[test]
    fn default_config_resolves_to_the_empty_set() {
        let set =
            resolve_experimental_config(&ExperimentalConfig::default()).expect("default resolves");
        assert!(set.is_empty());
    }

    #[test]
    fn enable_project_state_resolves_to_the_singleton_set() {
        let config = ExperimentalConfig {
            enable: vec!["project-state".to_owned()],
            macro_body_cap: None,
        };
        let set = resolve_experimental_config(&config).expect("known id resolves");
        assert_eq!(set, BTreeSet::from([Experiment::ProjectState]));
    }

    #[test]
    fn duplicate_enable_entries_dedup() {
        let config = ExperimentalConfig {
            enable: vec!["project-state".to_owned(), "project-state".to_owned()],
            macro_body_cap: None,
        };
        let set = resolve_experimental_config(&config).expect("duplicates resolve");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn unknown_enable_entry_fails_closed_with_remediation() {
        let config = ExperimentalConfig {
            enable: vec!["projcet-state".to_owned()],
            macro_body_cap: None,
        };
        let err = resolve_experimental_config(&config).expect_err("unknown id fails");
        let msg = err.to_string();
        assert!(msg.contains("projcet-state"), "names the entry: {msg}");
        assert!(msg.contains("project-state"), "names known ids: {msg}");
        assert!(msg.contains("[experimental]"), "names the surface: {msg}");
    }

    #[test]
    fn default_config_has_no_macro_body_cap_override() {
        // cute-dbt#265 Slice D: an absent `macro_body_cap` key means the
        // cli boundary applies DEFAULT_MACRO_BODY_CAP — the POD carries
        // None, not the literal default (the default lives at the I/O
        // boundary, the same posture as the report title fallback).
        assert!(ExperimentalConfig::default().macro_body_cap.is_none());
    }

    #[test]
    fn macro_body_cap_zero_is_a_valid_override() {
        // 0 is a legal cap (inline nothing — tree-only). A `usize` accepts
        // it; the render side treats 0 as "no inline bodies".
        let config = ExperimentalConfig {
            enable: vec![],
            macro_body_cap: Some(0),
        };
        assert_eq!(config.macro_body_cap, Some(0));
    }

    #[test]
    fn toml_does_not_accept_the_env_only_all_shorthand() {
        // "all"/"1" are env-var ergonomics; authored TOML names
        // experiments precisely.
        for shorthand in ["all", "1"] {
            let config = ExperimentalConfig {
                enable: vec![shorthand.to_owned()],
                macro_body_cap: None,
            };
            let err =
                resolve_experimental_config(&config).expect_err("TOML rejects the env shorthand");
            assert!(matches!(err, ExperimentalError::UnknownExperiment { .. }));
        }
    }

    // ----- CUTE_DBT_EXPERIMENTAL env parsing -----

    #[test]
    fn env_value_1_enables_every_experiment() {
        let set = parse_experimental_env("1").expect("\"1\" parses");
        assert_eq!(set, Experiment::ALL.iter().copied().collect());
    }

    #[test]
    fn env_value_all_enables_every_experiment() {
        let set = parse_experimental_env("all").expect("\"all\" parses");
        assert_eq!(set, Experiment::ALL.iter().copied().collect());
    }

    #[test]
    fn env_shorthand_tolerates_surrounding_whitespace() {
        let set = parse_experimental_env(" 1 ").expect("\" 1 \" parses");
        assert_eq!(set, Experiment::ALL.iter().copied().collect());
    }

    #[test]
    fn env_comma_list_resolves_ids() {
        let set = parse_experimental_env("project-state").expect("id list parses");
        assert_eq!(set, BTreeSet::from([Experiment::ProjectState]));
    }

    #[test]
    fn env_entries_are_trimmed_and_a_trailing_comma_is_tolerated() {
        let set = parse_experimental_env(" project-state , ").expect("padded list parses");
        assert_eq!(set, BTreeSet::from([Experiment::ProjectState]));
    }

    #[test]
    fn empty_env_value_enables_nothing() {
        // A CI step exporting CUTE_DBT_EXPERIMENTAL: "" must be a
        // no-op, not a usage error.
        assert!(parse_experimental_env("").expect("empty parses").is_empty());
        assert!(
            parse_experimental_env("  ")
                .expect("blank parses")
                .is_empty()
        );
    }

    #[test]
    fn unknown_env_entry_fails_closed_with_remediation() {
        let err = parse_experimental_env("project-state,bogus").expect_err("bogus fails");
        let msg = err.to_string();
        assert!(msg.contains("bogus"), "names the entry: {msg}");
        assert!(msg.contains("project-state"), "names known ids: {msg}");
        assert!(
            msg.contains("CUTE_DBT_EXPERIMENTAL"),
            "names the surface: {msg}",
        );
    }

    #[test]
    fn env_uppercase_all_fails_closed() {
        // Strict vocabulary: "ALL"/"0"/"true" are not accepted forms —
        // fail closed rather than guess.
        for not_a_form in ["ALL", "0", "true"] {
            assert!(
                parse_experimental_env(not_a_form).is_err(),
                "{not_a_form:?} is not an accepted form",
            );
        }
    }

    // ----- union semantics (exhaustive — repo convention: exhaustive
    // coverage over sampling, no proptest dep) -----

    /// Decode a bitmask over [`Experiment::ALL`] into the subset it
    /// selects.
    fn subset(mask: usize) -> BTreeSet<Experiment> {
        Experiment::ALL
            .iter()
            .copied()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, e)| e)
            .collect()
    }

    #[test]
    fn union_semantics_hold_exhaustively() {
        // For EVERY pair of subsets (toml, env) of the vocabulary:
        // is_enabled(e) iff toml ∋ e OR env ∋ e — and the union is
        // commutative.
        let n = Experiment::ALL.len();
        for toml_mask in 0..(1 << n) {
            for env_mask in 0..(1 << n) {
                let toml = subset(toml_mask);
                let env = subset(env_mask);
                let union = EnabledExperiments::from_union(&toml, &env);
                for experiment in Experiment::ALL.iter().copied() {
                    assert_eq!(
                        union.is_enabled(experiment),
                        toml.contains(&experiment) || env.contains(&experiment),
                        "union semantics: toml={toml:?} env={env:?} e={experiment:?}",
                    );
                }
                assert_eq!(
                    union,
                    EnabledExperiments::from_union(&env, &toml),
                    "union is commutative: toml={toml:?} env={env:?}",
                );
            }
        }
    }

    #[test]
    fn empty_union_enables_nothing() {
        let union = EnabledExperiments::from_union(&BTreeSet::new(), &BTreeSet::new());
        assert_eq!(union, EnabledExperiments::default());
        for experiment in Experiment::ALL.iter().copied() {
            assert!(!union.is_enabled(experiment));
        }
    }
}
