//! Parsed `dbt_project.yml` facts + the structural old/new diff
//! (cute-dbt#266, epic #262).
//!
//! [`ProjectDefinition`] is the owned POD the project-file adapter
//! (`adapters::project_def`) parses `dbt_project.yml` into — **standing
//! metadata**: the file is parsed whenever it is present, with or
//! without a diff, because vars / config trees / hooks / dispatch
//! defined there are part of every model's truth (the founder's v3
//! product frame on #262). The categorized project-change panel is one
//! consumer of the parsed model, not the trigger for parsing.
//!
//! Value vocabulary is `serde_json::Value` ONLY (the domain already
//! speaks `serde_json` — `cell_diff`, `unit_test_table`); the YAML parser's
//! types never appear here (the adapter converts, degrading
//! non-JSON-representable subtrees per-subtree). Source positions are
//! carried by the tiny [`Span`] POD — `{ line, column }`, nothing of the
//! parser's span machinery. The config trees mirror dbt-fusion's raw
//! project-config shape (`RawProjectConfig`, dbt-parser `utils.rs:29-105`
//! @ 9977b6cb): `+`-prefixed keys are config keys (prefix stripped),
//! non-`+` mapping keys are path/hierarchy children — `+` is the engine's
//! only config-vs-folder disambiguator.
//!
//! The structural diff ([`diff_project_definitions`]) compares two parsed
//! definitions field-by-field and emits categorized [`ProjectChange`]s —
//! Vars / `ConfigTree` / Dispatch / Hooks / Paths / Identity / Other — the
//! rows the report's "Project definition changed" panel renders. The
//! old side comes from [`crate::domain::pr_diff::reverse_apply`] over the
//! file's own hunks; when that (or either side's parse) degrades, the
//! panel falls back to the Shape-A raw-diff row ([`ProjectChangePanel::
//! Fallback`]) — fail-open display, never a failed report.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::pr_diff::DiffLine;

/// A source position in the authored `dbt_project.yml` — 1-based line and
/// column.
///
/// The domain twin of the YAML parser's marker type: only the two fields
/// the report could ever display ("defined at line N") cross the
/// boundary; the parser's byte offsets / filename machinery stay in the
/// adapter. Named `project_def::Span` (not re-exported at the domain
/// root) because [`crate::domain::cte::Span`] already owns the bare name
/// there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// 1-based source line.
    pub line: usize,
    /// 1-based source column.
    pub column: usize,
}

/// One node of a per-resource config tree (`models:` / `seeds:` / …),
/// mirroring fusion's raw project-config shape: `+key` entries are
/// config values (prefix stripped into [`configs`](Self::configs)),
/// non-`+` mapping keys are hierarchy children. A bare non-`+` key with
/// a **scalar** value — the legacy dbt-core dialect fusion strict-errors
/// on — is ingested tolerantly as a config key (ingest, never validate;
/// the engines' divergence is theirs to enforce).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigTree {
    /// Config keys at this level, `+` prefix stripped, in key order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub configs: BTreeMap<String, Value>,
    /// Path/hierarchy children (package or folder names), in key order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub children: BTreeMap<String, ConfigTree>,
}

/// The parsed `dbt_project.yml` — owned data, `std` + `serde` +
/// `serde_json` only (POD-only domain).
///
/// Field-to-category map for the diff: `name` / `version` /
/// `require_dbt_version` → Identity; `vars` → Vars; `config_trees` →
/// `ConfigTree`; `dispatch` → Dispatch; `on_run_start` / `on_run_end` →
/// Hooks; `paths` → Paths; `flags` and `other` → Other. The adapter
/// routes every top-level key into exactly one field, so nothing a
/// project file says is silently dropped — unrecognized keys land in
/// [`other`](Self::other) verbatim.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectDefinition {
    /// The project's `name:` (stringified when authored as a non-string
    /// scalar — tolerant ingestion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The project's `version:` — kept as authored (string or number;
    /// dbt accepts both).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Value>,
    /// `require-dbt-version:` — a string or a list of range strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_dbt_version: Option<Value>,
    /// The `vars:` block — var name → authored value (quoted Jinja
    /// scalars stay opaque strings; zero-compute never renders them).
    /// Package-scoped sub-maps stay nested mappings under the package
    /// name, exactly as authored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, Value>,
    /// Definition site of each top-level `vars:` entry — display
    /// provenance for later consumers ("defined at line N").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars_spans: BTreeMap<String, Span>,
    /// Per-resource config trees, keyed by the authored section name
    /// (`models`, `seeds`, `snapshots`, `tests` / `data_tests`,
    /// `unit_tests`, `sources`, `exposures`, `metrics`,
    /// `semantic-models`, `saved-queries`, `analyses`, `functions`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_trees: BTreeMap<String, ConfigTree>,
    /// The `dispatch:` block, verbatim (macro search-order config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<Value>,
    /// `on-run-start:` hook entries (a scalar authored form is wrapped
    /// into a one-element list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_run_start: Vec<Value>,
    /// `on-run-end:` hook entries (same normalization).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_run_end: Vec<Value>,
    /// The `flags:` block, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<Value>,
    /// Path configuration — every top-level key ending in `-paths` /
    /// `_paths` / `-path` / `_path` (deprecated `source-paths` /
    /// `data-paths` included — ingest, never validate) plus
    /// `clean-targets`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paths: BTreeMap<String, Value>,
    /// Everything else — `profile`, `config-version`, `query-comment`,
    /// `quoting`, the v1.10 `anchors:` reuse block, … — verbatim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub other: BTreeMap<String, Value>,
}

/// The category a project-definition change belongs to — the panel's
/// row grouping. Declaration order is display order (`Ord` derives from
/// it): vars first (the flagship blast-radius surface), then config
/// trees, then the rarer sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectChangeCategory {
    /// A `vars:` entry changed (blast radius not attributed in this
    /// slice — the panel row states so plainly).
    Vars,
    /// A config-tree path changed (`models:`/`seeds:`/… `+key` values).
    ConfigTree,
    /// The `dispatch:` macro search order changed — project-wide effect,
    /// not statically attributable.
    Dispatch,
    /// An `on-run-start:` / `on-run-end:` hook list changed.
    Hooks,
    /// A path-configuration key changed.
    Paths,
    /// `name:` / `version:` / `require-dbt-version:` changed.
    Identity,
    /// `flags:` or any unrecognized top-level key changed.
    Other,
}

/// One categorized project-definition change: the panel row's facts.
///
/// `old`/`new` are `None` for an added/removed key respectively; both
/// `Some` for a value change. Additive POD (ADR-5),
/// `Serialize`/`Deserialize` so the rows ride the inlined report payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectChange {
    /// Which panel grouping the change belongs to.
    pub category: ProjectChangeCategory,
    /// The changed key's display path — e.g. `default_state` (vars),
    /// `models.playground.marts: +materialized` (config tree),
    /// `on-run-start` (hooks), `name` (identity).
    pub label: String,
    /// The old value (`None` ⇒ the key was added).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Value>,
    /// The new value (`None` ⇒ the key was removed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Value>,
}

/// Why the panel degraded to the Shape-A raw-diff row instead of
/// categorized changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFallbackReason {
    /// The working-tree `dbt_project.yml` could not be parsed — nothing
    /// to categorize ("could not categorize").
    NewParseFailed,
    /// The reconstructed previous version could not be parsed
    /// ("could not categorize the previous version").
    OldParseFailed,
    /// [`crate::domain::pr_diff::reverse_apply`] refused (drift /
    /// malformed hunks) — "could not reconstruct the previous version".
    OldNotReconstructable,
    /// `dbt_project.yml` is in the diff but could not be read from the
    /// project root (absence note).
    FileUnreadable,
}

/// What the "Project definition changed" panel shows — rendered whenever
/// `dbt_project.yml` is in the PR diff (cute-dbt#266).
///
/// Fail-open by construction: every degrade arm is a [`Fallback`]
/// (raw diff + explicit copy), never a missing panel and never a failed
/// report.
///
/// [`Fallback`]: Self::Fallback
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectChangePanel {
    /// Both sides parsed; the structural diff produced categorized rows.
    /// An empty `changes` list is itself a truthful statement: the edit
    /// was formatting/comment-only (no semantic configuration change).
    Categorized {
        /// The categorized changes, sorted by (category, label).
        changes: Vec<ProjectChange>,
    },
    /// Semantic categorization degraded — show the raw diff lines with
    /// the reason's explicit copy (Shape A).
    Fallback {
        /// Which degrade arm fired.
        reason: ProjectFallbackReason,
        /// The file's own hunks rendered as raw `-`/`+` lines
        /// ([`crate::domain::pr_diff::raw_hunk_lines`]).
        raw: Vec<DiffLine>,
    },
}

/// The project-definition gather outcome the run loop threads into the
/// renderer (cute-dbt#266) — one value, two consumers.
///
/// `definition` is the **standing metadata**: the parsed working-tree
/// `dbt_project.yml` whenever it is present and parses, on BOTH scope
/// arms (the founder's parse-always posture; future consumers — explorer
/// panes, provenance chips — read it from the payload). `panel` is the
/// **diff-gated** consumer: `Some` exactly when `dbt_project.yml` is in
/// the PR diff. Both `None` (the default) leaves the payload
/// byte-identical to the pre-#266 shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectFacts {
    /// The parsed working-tree `dbt_project.yml` (standing metadata);
    /// `None` when unreadable, unparseable, or no project root resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<ProjectDefinition>,
    /// The "Project definition changed" panel content; `Some` exactly
    /// when `dbt_project.yml` is in the PR diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<ProjectChangePanel>,
}

/// Structurally diff two parsed project definitions into categorized
/// changes, sorted by (category, label) for deterministic panel rows.
///
/// Union semantics per keyed collection: a key present on either side is
/// compared; equal values emit nothing, differing values emit one
/// [`ProjectChange`] with the side-specific `old`/`new` (`None` marks
/// the absent side). Config trees are walked recursively — one change
/// per differing `+key` leaf, labelled with its dotted tree path — so a
/// folder-level edit reports exactly the keys that changed, not the
/// whole subtree. A file created in the PR diffs against
/// [`ProjectDefinition::default()`] (every entry reports as added).
///
/// Spans ([`ProjectDefinition::vars_spans`]) are display provenance, not
/// config facts — they never participate in change detection.
#[must_use]
pub fn diff_project_definitions(
    old: &ProjectDefinition,
    new: &ProjectDefinition,
) -> Vec<ProjectChange> {
    let mut out = Vec::new();

    // Identity — name / version / require-dbt-version.
    let identity = [
        (
            "name",
            old.name.clone().map(Value::String),
            new.name.clone().map(Value::String),
        ),
        ("version", old.version.clone(), new.version.clone()),
        (
            "require-dbt-version",
            old.require_dbt_version.clone(),
            new.require_dbt_version.clone(),
        ),
    ];
    for (label, o, n) in identity {
        push_if_changed(&mut out, ProjectChangeCategory::Identity, label, o, n);
    }

    diff_value_maps(&mut out, ProjectChangeCategory::Vars, &old.vars, &new.vars);

    // Config trees — union of section names, each walked recursively.
    for section in union_keys(&old.config_trees, &new.config_trees) {
        diff_config_tree(
            &mut out,
            &section,
            old.config_trees.get(&section),
            new.config_trees.get(&section),
        );
    }

    push_if_changed(
        &mut out,
        ProjectChangeCategory::Dispatch,
        "dispatch",
        old.dispatch.clone(),
        new.dispatch.clone(),
    );

    for (label, o, n) in [
        ("on-run-start", &old.on_run_start, &new.on_run_start),
        ("on-run-end", &old.on_run_end, &new.on_run_end),
    ] {
        if o != n {
            out.push(ProjectChange {
                category: ProjectChangeCategory::Hooks,
                label: label.to_owned(),
                old: (!o.is_empty()).then(|| Value::Array(o.clone())),
                new: (!n.is_empty()).then(|| Value::Array(n.clone())),
            });
        }
    }

    diff_value_maps(
        &mut out,
        ProjectChangeCategory::Paths,
        &old.paths,
        &new.paths,
    );

    push_if_changed(
        &mut out,
        ProjectChangeCategory::Other,
        "flags",
        old.flags.clone(),
        new.flags.clone(),
    );
    diff_value_maps(
        &mut out,
        ProjectChangeCategory::Other,
        &old.other,
        &new.other,
    );

    out.sort_by(|a, b| (a.category, &a.label).cmp(&(b.category, &b.label)));
    out
}

/// Push one change when the two optional values differ.
fn push_if_changed(
    out: &mut Vec<ProjectChange>,
    category: ProjectChangeCategory,
    label: &str,
    old: Option<Value>,
    new: Option<Value>,
) {
    if old != new {
        out.push(ProjectChange {
            category,
            label: label.to_owned(),
            old,
            new,
        });
    }
}

/// Diff two flat key→value maps under one category (vars / paths /
/// other) — union of keys, one change per differing key.
fn diff_value_maps(
    out: &mut Vec<ProjectChange>,
    category: ProjectChangeCategory,
    old: &BTreeMap<String, Value>,
    new: &BTreeMap<String, Value>,
) {
    for key in union_keys(old, new) {
        push_if_changed(
            out,
            category,
            &key,
            old.get(&key).cloned(),
            new.get(&key).cloned(),
        );
    }
}

/// Recursively diff one config-tree node — `path` is the dotted tree
/// path so far (starting at the section name). Emits one `ConfigTree`
/// change per differing `+key` leaf, labelled `"{path}: +{key}"`.
fn diff_config_tree(
    out: &mut Vec<ProjectChange>,
    path: &str,
    old: Option<&ConfigTree>,
    new: Option<&ConfigTree>,
) {
    let empty = ConfigTree::default();
    let old = old.unwrap_or(&empty);
    let new = new.unwrap_or(&empty);
    for key in union_keys(&old.configs, &new.configs) {
        push_if_changed(
            out,
            ProjectChangeCategory::ConfigTree,
            &format!("{path}: +{key}"),
            old.configs.get(&key).cloned(),
            new.configs.get(&key).cloned(),
        );
    }
    for child in union_keys(&old.children, &new.children) {
        diff_config_tree(
            out,
            &format!("{path}.{child}"),
            old.children.get(&child),
            new.children.get(&child),
        );
    }
}

/// The sorted union of two `BTreeMap`s' keys.
fn union_keys<V>(a: &BTreeMap<String, V>, b: &BTreeMap<String, V>) -> Vec<String> {
    let mut keys: Vec<String> = a.keys().chain(b.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defn() -> ProjectDefinition {
        ProjectDefinition {
            name: Some("playground".to_owned()),
            version: Some(json!("1.0")),
            require_dbt_version: None,
            vars: BTreeMap::from([
                ("default_state".to_owned(), json!("CT")),
                ("grid_density".to_owned(), json!(7)),
            ]),
            vars_spans: BTreeMap::from([("default_state".to_owned(), Span { line: 8, column: 3 })]),
            config_trees: BTreeMap::from([(
                "models".to_owned(),
                ConfigTree {
                    configs: BTreeMap::new(),
                    children: BTreeMap::from([(
                        "playground".to_owned(),
                        ConfigTree {
                            configs: BTreeMap::from([("materialized".to_owned(), json!("view"))]),
                            children: BTreeMap::from([(
                                "marts".to_owned(),
                                ConfigTree {
                                    configs: BTreeMap::from([(
                                        "materialized".to_owned(),
                                        json!("table"),
                                    )]),
                                    children: BTreeMap::new(),
                                },
                            )]),
                        },
                    )]),
                },
            )]),
            dispatch: None,
            on_run_start: Vec::new(),
            on_run_end: Vec::new(),
            flags: None,
            paths: BTreeMap::from([("model-paths".to_owned(), json!(["models"]))]),
            other: BTreeMap::from([("profile".to_owned(), json!("playground"))]),
        }
    }

    // ----- POD serde round-trips (ADR-5) -----

    #[test]
    fn project_definition_round_trips_through_json() {
        let def = defn();
        let json = serde_json::to_string(&def).expect("serialize");
        let back: ProjectDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(def, back);
    }

    #[test]
    fn empty_project_definition_serializes_to_an_empty_object() {
        // Every field is default-skipped, so an absent dbt_project.yml
        // contributes zero payload noise.
        assert_eq!(
            serde_json::to_string(&ProjectDefinition::default()).expect("serialize"),
            "{}",
        );
    }

    #[test]
    fn project_change_panel_round_trips_both_variants() {
        let categorized = ProjectChangePanel::Categorized {
            changes: vec![ProjectChange {
                category: ProjectChangeCategory::Vars,
                label: "default_state".to_owned(),
                old: Some(json!("CT")),
                new: Some(json!("VT")),
            }],
        };
        let fallback = ProjectChangePanel::Fallback {
            reason: ProjectFallbackReason::OldNotReconstructable,
            raw: vec![DiffLine {
                kind: crate::domain::pr_diff::DiffLineKind::Added,
                text: "vars:".to_owned(),
                emphasis: None,
            }],
        };
        for panel in [categorized, fallback] {
            let json = serde_json::to_string(&panel).expect("serialize");
            let back: ProjectChangePanel = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(panel, back);
        }
    }

    #[test]
    fn category_serializes_snake_case_and_orders_by_declaration() {
        assert_eq!(
            serde_json::to_string(&ProjectChangeCategory::ConfigTree).unwrap(),
            r#""config_tree""#,
        );
        let mut cats = vec![
            ProjectChangeCategory::Other,
            ProjectChangeCategory::Identity,
            ProjectChangeCategory::Vars,
            ProjectChangeCategory::Hooks,
        ];
        cats.sort();
        assert_eq!(
            cats,
            vec![
                ProjectChangeCategory::Vars,
                ProjectChangeCategory::Hooks,
                ProjectChangeCategory::Identity,
                ProjectChangeCategory::Other,
            ],
        );
    }

    // ----- diff: identity / no-op -----

    #[test]
    fn identical_definitions_diff_to_no_changes() {
        assert!(diff_project_definitions(&defn(), &defn()).is_empty());
    }

    #[test]
    fn span_only_divergence_is_not_a_change() {
        // A comment/formatting edit moves definition sites without
        // changing facts — spans are provenance, never change signal.
        let old = defn();
        let mut new = defn();
        new.vars_spans.insert(
            "default_state".to_owned(),
            Span {
                line: 99,
                column: 1,
            },
        );
        assert!(diff_project_definitions(&old, &new).is_empty());
    }

    // ----- diff: vars -----

    #[test]
    fn a_changed_var_reports_old_and_new_value() {
        let old = defn();
        let mut new = defn();
        new.vars.insert("default_state".to_owned(), json!("VT"));
        let changes = diff_project_definitions(&old, &new);
        assert_eq!(
            changes,
            vec![ProjectChange {
                category: ProjectChangeCategory::Vars,
                label: "default_state".to_owned(),
                old: Some(json!("CT")),
                new: Some(json!("VT")),
            }],
        );
    }

    #[test]
    fn an_added_var_reports_none_old_and_a_removed_var_none_new() {
        let old = defn();
        let mut new = defn();
        new.vars.remove("grid_density");
        new.vars.insert("brand_new".to_owned(), json!(true));
        let changes = diff_project_definitions(&old, &new);
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[0],
            ProjectChange {
                category: ProjectChangeCategory::Vars,
                label: "brand_new".to_owned(),
                old: None,
                new: Some(json!(true)),
            },
        );
        assert_eq!(
            changes[1],
            ProjectChange {
                category: ProjectChangeCategory::Vars,
                label: "grid_density".to_owned(),
                old: Some(json!(7)),
                new: None,
            },
        );
    }

    // ----- diff: config trees -----

    #[test]
    fn a_changed_config_leaf_is_labelled_with_its_dotted_tree_path() {
        let old = defn();
        let mut new = defn();
        new.config_trees
            .get_mut("models")
            .unwrap()
            .children
            .get_mut("playground")
            .unwrap()
            .children
            .get_mut("marts")
            .unwrap()
            .configs
            .insert("materialized".to_owned(), json!("incremental"));
        let changes = diff_project_definitions(&old, &new);
        assert_eq!(
            changes,
            vec![ProjectChange {
                category: ProjectChangeCategory::ConfigTree,
                label: "models.playground.marts: +materialized".to_owned(),
                old: Some(json!("table")),
                new: Some(json!("incremental")),
            }],
        );
    }

    #[test]
    fn an_untouched_sibling_subtree_reports_nothing() {
        // Only the differing leaf reports — never the whole subtree.
        let old = defn();
        let mut new = defn();
        new.config_trees
            .get_mut("models")
            .unwrap()
            .children
            .get_mut("playground")
            .unwrap()
            .configs
            .insert("tags".to_owned(), json!(["nightly"]));
        let changes = diff_project_definitions(&old, &new);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].label, "models.playground: +tags");
        assert_eq!(changes[0].old, None);
    }

    #[test]
    fn a_new_config_section_reports_each_leaf_as_added() {
        let old = defn();
        let mut new = defn();
        new.config_trees.insert(
            "seeds".to_owned(),
            ConfigTree {
                configs: BTreeMap::from([("quote_columns".to_owned(), json!(false))]),
                children: BTreeMap::new(),
            },
        );
        let changes = diff_project_definitions(&old, &new);
        assert_eq!(
            changes,
            vec![ProjectChange {
                category: ProjectChangeCategory::ConfigTree,
                label: "seeds: +quote_columns".to_owned(),
                old: None,
                new: Some(json!(false)),
            }],
        );
    }

    // ----- diff: dispatch / hooks / paths / identity / other -----

    #[test]
    fn dispatch_hooks_paths_identity_and_other_each_report_their_category() {
        let old = defn();
        let mut new = defn();
        new.name = Some("renamed".to_owned());
        new.dispatch = Some(json!([{ "macro_namespace": "dbt_utils",
                                      "search_order": ["playground", "dbt_utils"] }]));
        new.on_run_start = vec![json!("grant usage on database x to role y")];
        new.paths
            .insert("model-paths".to_owned(), json!(["models", "marts"]));
        new.other.insert("profile".to_owned(), json!("prod"));
        new.flags = Some(json!({ "send_anonymous_usage_stats": false }));

        let changes = diff_project_definitions(&old, &new);
        let cats: Vec<(ProjectChangeCategory, &str)> = changes
            .iter()
            .map(|c| (c.category, c.label.as_str()))
            .collect();
        assert_eq!(
            cats,
            vec![
                (ProjectChangeCategory::Dispatch, "dispatch"),
                (ProjectChangeCategory::Hooks, "on-run-start"),
                (ProjectChangeCategory::Paths, "model-paths"),
                (ProjectChangeCategory::Identity, "name"),
                (ProjectChangeCategory::Other, "flags"),
                (ProjectChangeCategory::Other, "profile"),
            ],
            "one change per surface, sorted by (category, label)",
        );
        // Hooks change carries both sides as arrays (old side empty ⇒ None).
        let hooks = &changes[1];
        assert_eq!(hooks.old, None);
        assert_eq!(
            hooks.new,
            Some(json!(["grant usage on database x to role y"])),
        );
    }

    #[test]
    fn a_file_created_in_the_pr_diffs_against_the_default_as_all_added() {
        let changes = diff_project_definitions(&ProjectDefinition::default(), &defn());
        assert!(!changes.is_empty());
        assert!(
            changes.iter().all(|c| c.old.is_none()),
            "every change of a created file is an addition",
        );
        // Identity arrives too.
        assert!(
            changes
                .iter()
                .any(|c| c.category == ProjectChangeCategory::Identity && c.label == "name"),
        );
    }

    #[test]
    fn changes_sort_by_category_then_label() {
        let old = ProjectDefinition::default();
        let new = defn();
        let changes = diff_project_definitions(&old, &new);
        let mut sorted = changes.clone();
        sorted.sort_by(|a, b| (a.category, &a.label).cmp(&(b.category, &b.label)));
        assert_eq!(changes, sorted, "diff output arrives pre-sorted");
    }
}
