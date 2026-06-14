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

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::manifest::Manifest;
use crate::domain::path::normalize_path;
use crate::domain::pr_diff::{BlockDiff, DiffLine, diff_lines};

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

/// The `config_trees` section whose resolution targets **model** nodes —
/// the one section cute-dbt#267's scope widening attributes (seeds /
/// snapshots / tests trees resolve against non-model resource types,
/// which host no unit tests in v0.1).
const MODELS_SECTION: &str = "models";

/// The dotted display path of a config-tree node: the section name alone
/// at the root (`models`), else `section.seg1.seg2…`
/// (`models.healthcare_analytics.marts`). One format authority for the
/// panel labels and the cute-dbt#267 attribution chips — the two
/// surfaces cannot drift.
fn dotted_tree_path(section: &str, segments: &[String]) -> String {
    if segments.is_empty() {
        section.to_owned()
    } else {
        format!("{section}.{}", segments.join("."))
    }
}

/// Structured identity of one changed config-tree leaf (cute-dbt#267):
/// the section (`models` / `seeds` / …), the hierarchy segments under
/// it, and the `+`-stripped config key. Carried on
/// [`ProjectChange::tree`] for `ConfigTree`-category changes so the
/// scope-widening attribution consumes the SAME change set the panel
/// renders (never a re-derived parallel diff).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigLeafPath {
    /// The authored section name (`models`, `seeds`, …).
    pub section: String,
    /// Hierarchy segments under the section root (package / folder /
    /// node names) — empty for a section-root config key.
    pub segments: Vec<String>,
    /// The config key, `+` prefix stripped (`materialized`, `tags`, …).
    pub key: String,
}

impl ConfigLeafPath {
    /// The dotted tree path without the key — `models` /
    /// `models.healthcare_analytics.marts`.
    #[must_use]
    pub fn dotted(&self) -> String {
        dotted_tree_path(&self.section, &self.segments)
    }

    /// The panel row label — `models.healthcare_analytics.marts:
    /// +materialized`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}: +{}", self.dotted(), self.key)
    }
}

/// The category a project-definition change belongs to — the panel's
/// row grouping. Declaration order is display order (`Ord` derives from
/// it): vars first (the flagship blast-radius surface), then config
/// trees, then the rarer sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectChangeCategory {
    /// A `vars:` entry changed. Since cute-dbt#268 the row carries
    /// precedence-resolved per-var attribution
    /// ([`ProjectChange::vars`]); blast radius is stated at honest
    /// tiers, never widened into scope.
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
    /// Hook-row enrichment (cute-dbt#269) — `Some` exactly on `Hooks`
    /// rows after [`attach_hook_facts`]: the inline SQL diff of the hook
    /// bodies plus the manifest-side `operation.*` reality. `None` on
    /// every other category (and on pre-#269 payloads — additive,
    /// ADR-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<HookChangeFacts>,
    /// Structured leaf identity for [`ProjectChangeCategory::ConfigTree`]
    /// changes (cute-dbt#267) — `None` for every other category. The
    /// attribution matcher and the panel's affected-models listing both
    /// key on this, so they consume the exact change set the row
    /// displays. Omitted from JSON when absent (pre-#267 payloads stay
    /// byte-stable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<ConfigLeafPath>,
    /// Vars-row enrichment (cute-dbt#268) — `Some` exactly on `Vars`
    /// rows after [`crate::domain::vars::attach_var_facts`]: the
    /// precedence-resolved per-var entries with tiered affected-model
    /// lists, package masking, and the override-pin subtraction. `None`
    /// on every other category (and on pre-#268 payloads — additive,
    /// ADR-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<crate::domain::vars::VarChangeFacts>,
}

/// One manifest `operation.*` node backing a project hook entry
/// (cute-dbt#269).
///
/// dbt materializes each `on-run-start:` / `on-run-end:` entry as a node
/// `operation.{project}.{project}-on-run-{start|end}-{i}` whose
/// `raw_code` is the hook SQL verbatim and whose `original_file_path`
/// is `./dbt_project.yml` (dbt-fusion `resolve_operations.rs:106-145` @
/// `9977b6cb…`; dbt-core emits the same shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOperation {
    /// The full node id (`operation.{project}.{project}-on-run-start-0`).
    pub id: String,
    /// The hook SQL (`raw_code`), empty when the node carries none.
    pub sql: String,
}

/// The manifest-side reality of the project's run hooks (cute-dbt#269):
/// the `operation.*` nodes for each hook kind, in hook-index order.
/// Transient (extracted per run, consumed by [`attach_hook_facts`]) —
/// the payload-riding facts live in [`HookChangeFacts`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookOperations {
    /// `on-run-start` operations, ordered by their `-{i}` suffix.
    pub on_run_start: Vec<HookOperation>,
    /// `on-run-end` operations, same ordering.
    pub on_run_end: Vec<HookOperation>,
}

/// How the manifest's `operation.*` nodes relate to the working-tree
/// hook entries (cute-dbt#269) — the hook row's honesty verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookManifestPresence {
    /// The operation bodies byte-match the parsed working-tree entries
    /// (single-trailing-newline frame tolerated) — the diff's new side
    /// IS the manifest's operation nodes. Also the verdict when both
    /// sides are empty (hooks removed, manifest agrees).
    Matched,
    /// The manifest carries no `operation.*` nodes for this hook while
    /// the working tree declares entries — the manifest may predate the
    /// edit; the diff falls back to the parsed file.
    Absent,
    /// Operation nodes exist but do not match the working-tree entries
    /// — manifest and working tree are out of sync; the diff falls back
    /// to the parsed file.
    Diverged,
}

/// Hook-row facts attached to a `Hooks` [`ProjectChange`]
/// (cute-dbt#269). Additive POD (ADR-5) — rides the inlined payload so
/// the report JS can render the SQL diff with the #111 renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookChangeFacts {
    /// Inline line diff of the hook bodies, old → new (the #111
    /// [`BlockDiff`] vocabulary). `None` when the change carries no
    /// substantive line difference (the row falls back to its plain
    /// detail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql_diff: Option<BlockDiff>,
    /// The manifest's `operation.*` node ids for this hook kind, in
    /// hook-index order (empty when the manifest carries none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_ids: Vec<String>,
    /// The manifest-side honesty verdict.
    pub manifest: HookManifestPresence,
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
    /// Per-model config-tree attributions (cute-dbt#267) — node-id →
    /// the subtree edits whose fqn-resolved value changed for that model
    /// ([`attribute_config_tree_changes`]). Non-empty only when the
    /// panel is [`ProjectChangePanel::Categorized`] on the pr-diff arm
    /// (every degrade arm attributes nothing — never a guessed
    /// widening). Drives the scope widening, the model-row provenance
    /// chips, and the panel's affected-models listings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_attributions: BTreeMap<String, Vec<ConfigAttribution>>,
    /// Per-model var-reference chips (cute-dbt#268) — node-id → the
    /// edited vars the model references, tiered
    /// ([`crate::domain::vars::attribute_var_changes`]). Non-empty only
    /// when the panel is [`ProjectChangePanel::Categorized`] on the
    /// pr-diff arm AND the diff edits `vars:`. **Never** a scope input
    /// (contextualize-don't-widen — the founder's v3 frame): the render
    /// layer decorates in-scope models with chips; the in-scope set is
    /// untouched.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub var_references: BTreeMap<String, Vec<crate::domain::vars::VarReference>>,
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
            &mut Vec::new(),
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
                hook: None,
                tree: None,
                vars: None,
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
            hook: None,
            tree: None,
            vars: None,
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

/// Recursively diff one config-tree node — `segments` is the hierarchy
/// path under `section` so far (empty at the section root). Emits one
/// `ConfigTree` change per differing `+key` leaf, labelled via
/// [`ConfigLeafPath::label`] and carrying the structured leaf on
/// [`ProjectChange::tree`] (the cute-dbt#267 attribution input).
fn diff_config_tree(
    out: &mut Vec<ProjectChange>,
    section: &str,
    segments: &mut Vec<String>,
    old: Option<&ConfigTree>,
    new: Option<&ConfigTree>,
) {
    let empty = ConfigTree::default();
    let old = old.unwrap_or(&empty);
    let new = new.unwrap_or(&empty);
    for key in union_keys(&old.configs, &new.configs) {
        let (old_v, new_v) = (
            old.configs.get(&key).cloned(),
            new.configs.get(&key).cloned(),
        );
        if old_v != new_v {
            let tree = ConfigLeafPath {
                section: section.to_owned(),
                segments: segments.clone(),
                key,
            };
            out.push(ProjectChange {
                category: ProjectChangeCategory::ConfigTree,
                label: tree.label(),
                old: old_v,
                new: new_v,
                hook: None,
                tree: Some(tree),
                vars: None,
            });
        }
    }
    for child in union_keys(&old.children, &new.children) {
        segments.push(child.clone());
        diff_config_tree(
            out,
            section,
            segments,
            old.children.get(&child),
            new.children.get(&child),
        );
        segments.pop();
    }
}

/// The sorted union of two `BTreeMap`s' keys.
fn union_keys<V>(a: &BTreeMap<String, V>, b: &BTreeMap<String, V>) -> Vec<String> {
    let mut keys: Vec<String> = a.keys().chain(b.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    keys
}

// ---------------------------------------------------------------------
// Hook operation nodes + hook-row enrichment (cute-dbt#269)
// ---------------------------------------------------------------------

/// Collect the manifest's hook `operation.*` nodes for `project_name`
/// (cute-dbt#269).
///
/// Selection mirrors dbt's own naming contract: a node with
/// `resource_type == "operation"` whose bare name is
/// `{project_name}-on-run-{start|end}-{i}` — the name prefix partitions
/// the ROOT project's hooks from any installed package's (both anchor at
/// the package-internal `dbt_project.yml`, so the path alone cannot).
/// **Both engine spellings of the kind segment are accepted** (verified
/// against real manifests 2026-06-12): dbt-core and fusion-at-source
/// (`resolve_operations.rs:120-122` @ `9977b6cb…`) hyphenate
/// (`-on-run-start-`), while the released dbt-fusion 2.0.0-preview.177
/// emits underscores (`-on_run_start-`) and drops the `./` path prefix
/// — ingest both, never validate. The `original_file_path` is checked
/// **through the normalization authority's leaf** ([`normalize_path`],
/// which owns the `./` strip) — never a call-site strip — and
/// tolerantly: an absent path is accepted. Results sort by the `-{i}`
/// suffix, dbt's hook order. An empty `project_name` selects nothing
/// (no project to partition by).
#[must_use]
pub fn hook_operations(manifest: &Manifest, project_name: &str) -> HookOperations {
    let mut out = HookOperations::default();
    if project_name.is_empty() {
        return out;
    }
    let mut start: Vec<(usize, HookOperation)> = Vec::new();
    let mut end: Vec<(usize, HookOperation)> = Vec::new();
    for (id, node) in manifest.nodes() {
        if node.resource_type() != "operation" || !hook_path_is_project_file(node) {
            continue;
        }
        let name = node.bare_name();
        for (kind, bucket) in [("on-run-start", &mut start), ("on-run-end", &mut end)] {
            if let Some(index) = hook_name_index(name, project_name, kind) {
                bucket.push((
                    index,
                    HookOperation {
                        id: id.as_str().to_owned(),
                        sql: node.raw_code().unwrap_or_default().to_owned(),
                    },
                ));
            }
        }
    }
    start.sort_by_key(|(index, _)| *index);
    end.sort_by_key(|(index, _)| *index);
    out.on_run_start = start.into_iter().map(|(_, op)| op).collect();
    out.on_run_end = end.into_iter().map(|(_, op)| op).collect();
    out
}

/// Parse the `-{i}` suffix of `{project}-{kind}-{i}`, accepting both
/// engine spellings of the kind segment (`on-run-start` hyphenated —
/// dbt-core + fusion @ `9977b6cb…` — and `on_run_start` underscored —
/// released fusion 2.0.0-preview.177, real-manifest-verified).
fn hook_name_index(name: &str, project_name: &str, kind: &str) -> Option<usize> {
    let underscored = kind.replace('-', "_");
    name.strip_prefix(&format!("{project_name}-{kind}-"))
        .or_else(|| name.strip_prefix(&format!("{project_name}-{underscored}-")))
        .and_then(|suffix| suffix.parse::<usize>().ok())
}

/// Whether an operation node's declaring path is the project file —
/// `./dbt_project.yml` resolves through [`normalize_path`]'s `./` strip
/// (the single normalization authority's leaf); an absent path is
/// tolerated (ingest, never validate).
fn hook_path_is_project_file(node: &crate::domain::manifest::Node) -> bool {
    node.original_file_path()
        .is_none_or(|p| normalize_path(p, None) == "dbt_project.yml")
}

/// Attach [`HookChangeFacts`] to every `Hooks` row of a categorized
/// change list (cute-dbt#269).
///
/// Mirrors the #111 model-SQL-diff architecture: the diff's **new side
/// comes from the manifest's operation nodes** when they byte-match the
/// parsed working-tree entries (the same-revision contract, with the
/// single-trailing-newline frame tolerated — the dbt-core/fusion
/// divergence precedent), guarded exactly like `block_aligns_with_hunks`
/// guards the model diff: on any mismatch ([`HookManifestPresence::Absent`]
/// / [`Diverged`](HookManifestPresence::Diverged)) the new side falls
/// back to the parsed file — never a silently wrong diff. The old side
/// is always the reverse-applied file's parsed entries (there is no old
/// manifest on the PR-diff arm).
pub fn attach_hook_facts(changes: &mut [ProjectChange], ops: &HookOperations) {
    for change in changes
        .iter_mut()
        .filter(|c| c.category == ProjectChangeCategory::Hooks)
    {
        let kind_ops = if change.label == "on-run-start" {
            &ops.on_run_start
        } else {
            &ops.on_run_end
        };
        let old_entries = hook_entry_texts(change.old.as_ref());
        let new_entries = hook_entry_texts(change.new.as_ref());
        let op_bodies: Vec<String> = kind_ops
            .iter()
            .map(|op| trim_one_newline(&op.sql).to_owned())
            .collect();
        let presence = if op_bodies == new_entries {
            HookManifestPresence::Matched
        } else if op_bodies.is_empty() {
            HookManifestPresence::Absent
        } else {
            HookManifestPresence::Diverged
        };
        let new_side = if presence == HookManifestPresence::Matched {
            &op_bodies
        } else {
            &new_entries
        };
        let diff = diff_lines(&entry_lines(&old_entries), &entry_lines(new_side));
        change.hook = Some(HookChangeFacts {
            sql_diff: diff.has_real_change().then_some(diff),
            operation_ids: kind_ops.iter().map(|op| op.id.clone()).collect(),
            manifest: presence,
        });
    }
}

/// A hooks change's entries as display texts: a string entry verbatim
/// (the dbt shape), anything else as compact JSON (tolerant ingestion
/// keeps exotic YAML); each frame-normalized by [`trim_one_newline`].
fn hook_entry_texts(side: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(entries)) = side else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|v| match v {
            Value::String(s) => trim_one_newline(s).to_owned(),
            other => serde_json::to_string(other).unwrap_or_default(),
        })
        .collect()
}

/// Strip exactly one trailing line terminator — `\r\n` or `\n` (the
/// engine frame divergence precedent from the #111 model SQL diff,
/// extended to the CRLF frame; never `trim_end_matches`).
///
/// The parsed-file sides are LF-clean by construction (dbt-yaml
/// normalizes every CRLF scalar style — pinned by the adapter's
/// `crlf_authored_files_parse_to_lf_clean_hook_entries`), but the
/// manifest `raw_code` side is engine-serialized JSON with no such
/// guarantee from this pipeline — fusion already ships MODEL `raw_code`
/// file-verbatim including the file's line endings (the #111 finding),
/// so a CRLF frame here must not silently degrade the byte-match
/// verdict to the file fallback (gemini review on PR #285).
fn trim_one_newline(s: &str) -> &str {
    s.strip_suffix("\r\n")
        .or_else(|| s.strip_suffix('\n'))
        .unwrap_or(s)
}

/// Flatten hook entries into diffable lines (an entry may be a
/// multi-line block scalar), `\r`-trimmed per line — the
/// [`DiffLine::text`] contract (gemini review on PR #285).
///
/// Deliberately `split('\n')` + per-line `\r` strip, NOT [`str::lines`]:
/// `lines()` also swallows a trailing empty segment, so a genuine blank
/// last line (`"a\n\n"` after the one-terminator frame trim → `"a\n"`)
/// would vanish from the diff — the #111 "real blank line at EOF
/// survives" precedent.
fn entry_lines(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|e| {
            e.split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        })
        .collect()
}

// ---------------------------------------------------------------------
// Config-tree change attribution (cute-dbt#267)
// ---------------------------------------------------------------------

/// One provenance fact for an affected model (cute-dbt#267): which
/// `dbt_project.yml` subtree contributed a changed config key to it.
/// Rendered as the model-row chip
/// (`+materialized via dbt_project.yml · models.healthcare_analytics.marts`)
/// and inverted into the panel's per-row affected-models listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigAttribution {
    /// The `+`-stripped config key whose resolved value changed for this
    /// model.
    pub key: String,
    /// Dotted path of the **contributing** subtree — the deepest edited
    /// node that wins this key's resolution for the model's fqn (fusion's
    /// deepest-match-wins), e.g. `models.healthcare_analytics.marts`;
    /// bare `models` for a section-root key.
    pub path: String,
}

/// fusion's `get_config_for_fqn` descent (dbt-parser
/// `dbt_project_config.rs:109-122` @ 9977b6cbb1b761065536300037560d8e3c037011):
/// walk the fqn's segments child-by-child from the section root, stopping
/// at the first segment with no matching child, and resolve `key` to the
/// **deepest** visited node that sets it. fusion's
/// `recur_build_dbt_project_config` (`:297-336`, `default_to`) makes
/// children inherit unset fields from their parent, so per key the
/// deepest setter wins and shallower settings apply only where no deeper
/// node sets the key — exactly what tracking the last hit reproduces.
///
/// Returns the winning `(depth, value)`: `depth` is the number of fqn
/// segments consumed at the winning node (`0` = the section root).
fn resolve_key_for_fqn<'t>(
    tree: &'t ConfigTree,
    fqn: &[String],
    key: &str,
) -> Option<(usize, &'t Value)> {
    let mut node = tree;
    let mut winner = node.configs.get(key).map(|v| (0, v));
    for (i, segment) in fqn.iter().enumerate() {
        let Some(child) = node.children.get(segment) else {
            break;
        };
        node = child;
        if let Some(v) = node.configs.get(key) {
            winner = Some((i + 1, v));
        }
    }
    winner
}

/// The config keys the structural diff changed under the `models:`
/// section — the attribution's key universe. Reading them off the
/// [`ProjectChange::tree`] entries guarantees the attribution and the
/// panel categorize the SAME edit.
fn changed_models_tree_keys(changes: &[ProjectChange]) -> BTreeSet<&String> {
    changes
        .iter()
        .filter_map(|c| c.tree.as_ref())
        .filter(|t| t.section == MODELS_SECTION)
        .map(|t| &t.key)
        .collect()
}

/// The attributions for ONE model's fqn across the changed keys: a key
/// contributes when its fqn-resolved value differs between the old and
/// new `models:` trees (fusion's deepest-match-wins, so an edit shadowed
/// by a deeper unchanged setting contributes nothing). The chip path is
/// the deeper of the two winning nodes — the edited leaf that caused the
/// difference (value change / addition / removal alike).
fn attribute_one_fqn(
    old_tree: &ConfigTree,
    new_tree: &ConfigTree,
    fqn: &[String],
    changed_keys: &BTreeSet<&String>,
) -> Vec<ConfigAttribution> {
    let mut out = Vec::new();
    for key in changed_keys {
        let old_win = resolve_key_for_fqn(old_tree, fqn, key);
        let new_win = resolve_key_for_fqn(new_tree, fqn, key);
        if old_win.map(|(_, v)| v) == new_win.map(|(_, v)| v) {
            continue;
        }
        let depth = match (old_win, new_win) {
            (Some((od, _)), Some((nd, _))) => od.max(nd),
            (Some((od, _)), None) => od,
            (None, Some((nd, _))) => nd,
            // Both None compares equal above — unreachable here.
            (None, None) => continue,
        };
        out.push(ConfigAttribution {
            key: (*key).clone(),
            path: dotted_tree_path(MODELS_SECTION, &fqn[..depth]),
        });
    }
    out
}

/// Map a categorized `dbt_project.yml` diff onto the models it affects
/// (cute-dbt#267 — the C2 slice of epic #262).
///
/// For every `model` node in `current` carrying a non-empty `fqn`, and
/// for every config key the diff changed under the `models:` section,
/// the key's value is resolved against the model's fqn in BOTH trees via
/// fusion's own algorithm (the private `resolve_key_for_fqn` descent
/// documented above). The model is
/// affected exactly when the resolved values differ — **TOTAL tier**:
/// the selection is fusion's resolution semantics, not a heuristic, so
/// an edit shadowed by a deeper unchanged setting selects nothing and a
/// package subtree (`models.dbt_utils.…`) never selects an own-project
/// model (the fqn's first segment is its package name).
///
/// Returns node-id → attributions (sorted by key within each model;
/// `BTreeMap` keys give deterministic model order). Empty when the diff
/// touched no `models:` config key. A model with an empty `fqn`
/// (pre-cute-dbt#278 manifests) is never selected — the matcher refuses
/// to guess.
#[must_use]
pub fn attribute_config_tree_changes(
    current: &Manifest,
    old: &ProjectDefinition,
    new: &ProjectDefinition,
    changes: &[ProjectChange],
) -> BTreeMap<String, Vec<ConfigAttribution>> {
    let changed_keys = changed_models_tree_keys(changes);
    if changed_keys.is_empty() {
        return BTreeMap::new();
    }
    let empty = ConfigTree::default();
    let old_tree = old.config_trees.get(MODELS_SECTION).unwrap_or(&empty);
    let new_tree = new.config_trees.get(MODELS_SECTION).unwrap_or(&empty);
    let mut out = BTreeMap::new();
    for (id, node) in current.nodes() {
        if node.resource_type() != "model" || node.fqn().is_empty() {
            continue;
        }
        let attributions = attribute_one_fqn(old_tree, new_tree, node.fqn(), &changed_keys);
        if !attributions.is_empty() {
            out.insert(id.as_str().to_owned(), attributions);
        }
    }
    out
}

// ---------------------------------------------------------------------
// Standing config provenance (cute-dbt#270, epic #262)
// ---------------------------------------------------------------------

/// One standing config-provenance fact for a model (cute-dbt#270): a
/// `models:` config key whose value resolves to this model, the resolved
/// value, and the dotted path of the **winning** (deepest) subtree that
/// sets it — e.g. `materialized = view via models.healthcare_analytics`.
/// Rendered on the explore model-detail pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigProvenance {
    /// The `+`-stripped config key (`materialized`, `tags`, …).
    pub key: String,
    /// The resolved value, verbatim.
    pub value: Value,
    /// Dotted path of the contributing subtree — `models` for a
    /// section-root key, else `models.seg1.seg2…`.
    pub path: String,
}

/// Every config key the `models:` tree sets along `fqn`, paired with its
/// resolved value (fusion's deepest-match-wins) — the key universe is the
/// union of `+key`s on every node the fqn descent visits.
fn keys_along_fqn<'t>(tree: &'t ConfigTree, fqn: &[String]) -> BTreeSet<&'t String> {
    let mut keys: BTreeSet<&String> = tree.configs.keys().collect();
    let mut node = tree;
    for segment in fqn {
        let Some(child) = node.children.get(segment) else {
            break;
        };
        node = child;
        keys.extend(node.configs.keys());
    }
    keys
}

/// Resolve every `models:` config key that applies to ONE model's fqn,
/// fusion's deepest-match-wins (the same private `resolve_key_for_fqn`
/// descent the change-attribution uses). Sorted by key for deterministic
/// rendering.
fn provenance_for_fqn(tree: &ConfigTree, fqn: &[String]) -> Vec<ConfigProvenance> {
    keys_along_fqn(tree, fqn)
        .into_iter()
        .filter_map(|key| {
            resolve_key_for_fqn(tree, fqn, key).map(|(depth, value)| ConfigProvenance {
                key: key.clone(),
                value: value.clone(),
                path: dotted_tree_path(MODELS_SECTION, &fqn[..depth]),
            })
        })
        .collect()
}

/// Resolve the standing `models:` config provenance for every model node
/// (cute-dbt#270 — the explore-pane decoration of epic #262).
///
/// Pure computation over the parsed project definition + the manifest
/// (zero-compute), reusing the SAME fusion `get_config_for_fqn` descent
/// (`resolve_key_for_fqn`) the diff-gated config attribution uses, so the
/// report's "affected models" listing and the explore detail pane resolve
/// configs identically. Returns node-id → its resolved configs (sorted by
/// key). A model with an empty `fqn` resolves only section-root keys (the
/// descent breaks immediately — never a guess). Empty when the project
/// sets no `models:` configs.
#[must_use]
pub fn resolve_model_configs(
    current: &Manifest,
    def: &ProjectDefinition,
) -> BTreeMap<String, Vec<ConfigProvenance>> {
    let empty = ConfigTree::default();
    let tree = def.config_trees.get(MODELS_SECTION).unwrap_or(&empty);
    if tree.configs.is_empty() && tree.children.is_empty() {
        return BTreeMap::new();
    }
    let mut out = BTreeMap::new();
    for (id, node) in current.nodes() {
        if node.resource_type() != "model" {
            continue;
        }
        let provenance = provenance_for_fqn(tree, node.fqn());
        if !provenance.is_empty() {
            out.insert(id.as_str().to_owned(), provenance);
        }
    }
    out
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
                hook: None,
                tree: None,
                vars: None,
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
                hook: None,
                tree: None,
                vars: None,
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
                hook: None,
                tree: None,
                vars: None,
            },
        );
        assert_eq!(
            changes[1],
            ProjectChange {
                category: ProjectChangeCategory::Vars,
                label: "grid_density".to_owned(),
                old: Some(json!(7)),
                new: None,
                hook: None,
                tree: None,
                vars: None,
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
                hook: None,
                tree: Some(ConfigLeafPath {
                    section: "models".to_owned(),
                    segments: vec!["playground".to_owned(), "marts".to_owned()],
                    key: "materialized".to_owned(),
                }),
                vars: None,
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
                hook: None,
                tree: Some(ConfigLeafPath {
                    section: "seeds".to_owned(),
                    segments: Vec::new(),
                    key: "quote_columns".to_owned(),
                }),
                vars: None,
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

    // ----- hook operation nodes (cute-dbt#269) -----

    use std::collections::HashMap;

    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, Node, NodeConfig, NodeId,
    };

    /// A synthetic `operation.*` node in the dbt shape (fusion
    /// `resolve_operations.rs:106-145` @ `9977b6cb…`): id
    /// `operation.{project}.{project}-on-run-{kind}-{i}`, the name as
    /// its last segment, `raw_code` = the hook SQL, declaring path
    /// `./dbt_project.yml` VERBATIM.
    fn operation_node(project: &str, kind: &str, index: usize, sql: &str) -> (NodeId, Node) {
        let name = format!("{project}-on-run-{kind}-{index}");
        let id = NodeId::new(format!("operation.{project}.{name}"));
        let node = Node::new(
            id.clone(),
            "operation",
            Checksum::new("sha256", "feed"),
            None,
            Some(sql.to_owned()),
            DependsOn::default(),
            Some("./dbt_project.yml".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(Some(name), Some(project.to_owned()));
        (id, node)
    }

    fn manifest_with(nodes: Vec<(NodeId, Node)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json"),
            nodes.into_iter().collect::<HashMap<_, _>>(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[test]
    fn hook_operations_collects_per_kind_in_index_order() {
        let m = manifest_with(vec![
            operation_node("playground", "start", 1, "vacuum t"),
            operation_node("playground", "start", 0, "grant usage"),
            operation_node("playground", "end", 0, "analyze t"),
        ]);
        let ops = hook_operations(&m, "playground");
        let start: Vec<(&str, &str)> = ops
            .on_run_start
            .iter()
            .map(|o| (o.id.as_str(), o.sql.as_str()))
            .collect();
        assert_eq!(
            start,
            vec![
                (
                    "operation.playground.playground-on-run-start-0",
                    "grant usage",
                ),
                ("operation.playground.playground-on-run-start-1", "vacuum t"),
            ],
            "start hooks sort by the -i suffix",
        );
        assert_eq!(ops.on_run_end.len(), 1);
        assert_eq!(ops.on_run_end[0].sql, "analyze t");
    }

    #[test]
    fn hook_operations_partitions_by_project_name() {
        // An installed package's hooks also anchor at ./dbt_project.yml —
        // only the ROOT project's name prefix selects.
        let m = manifest_with(vec![
            operation_node("playground", "start", 0, "grant usage"),
            operation_node("dbt_utils", "start", 0, "package hook"),
        ]);
        let ops = hook_operations(&m, "playground");
        assert_eq!(ops.on_run_start.len(), 1);
        assert_eq!(ops.on_run_start[0].sql, "grant usage");
        assert!(
            hook_operations(&m, "").on_run_start.is_empty(),
            "an empty project name selects nothing",
        );
    }

    #[test]
    fn hook_operations_ignores_non_operation_nodes_and_foreign_paths() {
        // A model node whose name happens to match, and an operation
        // anchored at some other file, are both rejected.
        let (mid, model) = operation_node("playground", "start", 0, "select 1");
        let model = Node::new(
            mid.clone(),
            "model",
            Checksum::new("sha256", "feed"),
            None,
            model.raw_code().map(str::to_owned),
            DependsOn::default(),
            Some("./dbt_project.yml".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(
            Some("playground-on-run-start-0".to_owned()),
            Some("playground".to_owned()),
        );
        let (oid, op) = operation_node("playground", "start", 1, "elsewhere");
        let op_foreign = Node::new(
            oid.clone(),
            "operation",
            Checksum::new("sha256", "feed"),
            None,
            Some("elsewhere".to_owned()),
            DependsOn::default(),
            Some("macros/helpers.sql".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(op.name().map(str::to_owned), Some("playground".to_owned()));
        let m = manifest_with(vec![(mid, model), (oid, op_foreign)]);
        assert!(hook_operations(&m, "playground").on_run_start.is_empty());
    }

    #[test]
    fn hook_operations_accepts_the_fusion_underscore_dialect() {
        // Real-manifest-verified (2026-06-12, dbt-fusion
        // 2.0.0-preview.177 compile of the dogfood jaffle_shop):
        // name `jaffle_shop-on_run_start-0` (UNDERSCORED kind segment)
        // and original_file_path `dbt_project.yml` (no `./` prefix) —
        // both diverge from the pinned fusion source @ `9977b6cb…` and
        // from dbt-core. Ingest both dialects, never validate.
        let id = NodeId::new("operation.jaffle_shop.jaffle_shop-on_run_start-0");
        let node = Node::new(
            id.clone(),
            "operation",
            Checksum::new("sha256", "feed"),
            None,
            Some("create schema if not exists analytics_audit".to_owned()),
            DependsOn::default(),
            Some("dbt_project.yml".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(
            Some("jaffle_shop-on_run_start-0".to_owned()),
            Some("jaffle_shop".to_owned()),
        );
        let m = manifest_with(vec![(id, node)]);
        let ops = hook_operations(&m, "jaffle_shop");
        assert_eq!(ops.on_run_start.len(), 1);
        assert_eq!(
            ops.on_run_start[0].sql,
            "create schema if not exists analytics_audit",
        );
    }

    #[test]
    fn hook_operations_tolerates_a_missing_declaring_path() {
        // Ingest, never validate: an operation node without
        // original_file_path still selects by name pattern.
        let (id, node) = operation_node("playground", "end", 0, "analyze t");
        let node = Node::new(
            id.clone(),
            "operation",
            Checksum::new("sha256", "feed"),
            None,
            Some("analyze t".to_owned()),
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(node.name().map(str::to_owned), None);
        let m = manifest_with(vec![(id, node)]);
        assert_eq!(hook_operations(&m, "playground").on_run_end.len(), 1);
    }

    // ----- attach_hook_facts (cute-dbt#269) -----

    fn hooks_change(label: &str, old: Option<Value>, new: Option<Value>) -> ProjectChange {
        ProjectChange {
            category: ProjectChangeCategory::Hooks,
            label: label.to_owned(),
            old,
            new,
            hook: None,
            tree: None,
            vars: None,
        }
    }

    #[test]
    fn attach_marks_matched_when_operations_byte_match_the_new_entries() {
        let m = manifest_with(vec![operation_node(
            "playground",
            "start",
            0,
            "grant select on schema x",
        )]);
        let ops = hook_operations(&m, "playground");
        let mut changes = vec![hooks_change(
            "on-run-start",
            Some(json!(["grant usage on schema x"])),
            Some(json!(["grant select on schema x"])),
        )];
        attach_hook_facts(&mut changes, &ops);
        let facts = changes[0].hook.as_ref().expect("hook facts attached");
        assert_eq!(facts.manifest, HookManifestPresence::Matched);
        assert_eq!(
            facts.operation_ids,
            vec!["operation.playground.playground-on-run-start-0"],
        );
        let diff = facts.sql_diff.as_ref().expect("a real change diffs");
        let kinds: Vec<crate::domain::pr_diff::DiffLineKind> =
            diff.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                crate::domain::pr_diff::DiffLineKind::Removed,
                crate::domain::pr_diff::DiffLineKind::Added,
            ],
        );
        assert_eq!(diff.lines[1].text, "grant select on schema x");
    }

    #[test]
    fn attach_matched_tolerates_the_single_trailing_newline_frame() {
        // The engine-frame divergence precedent (#111): an operation
        // body retaining the trailing newline still matches the parsed
        // entry.
        let m = manifest_with(vec![operation_node(
            "playground",
            "start",
            0,
            "grant select on schema x\n",
        )]);
        let ops = hook_operations(&m, "playground");
        let mut changes = vec![hooks_change(
            "on-run-start",
            None,
            Some(json!(["grant select on schema x"])),
        )];
        attach_hook_facts(&mut changes, &ops);
        assert_eq!(
            changes[0].hook.as_ref().unwrap().manifest,
            HookManifestPresence::Matched,
        );
    }

    #[test]
    fn attach_matched_tolerates_a_crlf_trailing_frame() {
        // Gemini review on PR #285: a CRLF terminator on the manifest's
        // operation body (an engine serializing raw_code file-verbatim
        // from a Windows-authored project — the #111 model-raw_code
        // semantic) must not degrade the Matched verdict to the file
        // fallback. The parsed-file side is LF-clean by construction
        // (the adapter CRLF pin), so the frame trim is what bridges.
        let m = manifest_with(vec![operation_node(
            "playground",
            "start",
            0,
            "grant select on schema x\r\n",
        )]);
        let ops = hook_operations(&m, "playground");
        let mut changes = vec![hooks_change(
            "on-run-start",
            None,
            Some(json!(["grant select on schema x"])),
        )];
        attach_hook_facts(&mut changes, &ops);
        assert_eq!(
            changes[0].hook.as_ref().unwrap().manifest,
            HookManifestPresence::Matched,
        );
    }

    #[test]
    fn attach_diff_lines_are_cr_trimmed_for_crlf_multiline_bodies() {
        // Gemini review on PR #285: interior CRLF breaks in a multiline
        // hook body must split into `\r`-free diff lines (the
        // DiffLine::text contract) — a stray `\r` would skew the
        // intra-line emphasis offsets and leak into the rendered diff.
        let m = manifest_with(vec![operation_node(
            "playground",
            "end",
            0,
            "analyze t1\r\nanalyze t3",
        )]);
        let ops = hook_operations(&m, "playground");
        let mut changes = vec![hooks_change(
            "on-run-end",
            Some(json!(["analyze t1\nanalyze t2"])),
            Some(json!(["analyze t1\nanalyze t3"])),
        )];
        attach_hook_facts(&mut changes, &ops);
        let facts = changes[0].hook.as_ref().expect("hook facts attached");
        // The byte-match verdict compares whole entries pre-split, so an
        // interior-CRLF op body reads as Diverged (honest: the bytes
        // differ from the parser-clean file side) and the diff comes
        // from the file — the Diverged fallback is itself the guard
        // that keeps interior `\r` out of the rendered diff.
        assert_eq!(facts.manifest, HookManifestPresence::Diverged);
        let diff = facts.sql_diff.as_ref().expect("diff present");
        assert!(
            diff.lines.iter().all(|l| !l.text.contains('\r')),
            "every diff line is \\r-trimmed: {:?}",
            diff.lines,
        );
        let kinds: Vec<(crate::domain::pr_diff::DiffLineKind, &str)> = diff
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (crate::domain::pr_diff::DiffLineKind::Context, "analyze t1"),
                (crate::domain::pr_diff::DiffLineKind::Removed, "analyze t2"),
                (crate::domain::pr_diff::DiffLineKind::Added, "analyze t3"),
            ],
        );
    }

    #[test]
    fn entry_lines_keeps_a_genuine_trailing_blank_line() {
        // The reason entry_lines is split('\n')+strip and NOT
        // str::lines(): a real blank last line must survive into the
        // diff (the #111 precedent); lines() would swallow the trailing
        // empty segment along with the `\r`s.
        assert_eq!(
            entry_lines(&["a\r\n\r".to_owned()]),
            vec!["a".to_owned(), String::new()],
        );
    }

    #[test]
    fn attach_marks_absent_and_falls_back_to_file_entries() {
        let ops = HookOperations::default();
        let mut changes = vec![hooks_change(
            "on-run-end",
            Some(json!(["analyze table x"])),
            Some(json!(["analyze table y"])),
        )];
        attach_hook_facts(&mut changes, &ops);
        let facts = changes[0].hook.as_ref().expect("hook facts attached");
        assert_eq!(facts.manifest, HookManifestPresence::Absent);
        assert!(facts.operation_ids.is_empty());
        let diff = facts.sql_diff.as_ref().expect("file-side diff");
        assert!(diff.lines.iter().any(|l| l.text == "analyze table y"));
    }

    #[test]
    fn attach_marks_diverged_and_never_renders_a_silently_wrong_diff() {
        // The manifest's operation body disagrees with the working tree
        // (stale manifest): the diff must come from the FILE entries —
        // using the stale op body would show "no change".
        let m = manifest_with(vec![operation_node(
            "playground",
            "start",
            0,
            "grant usage on schema x",
        )]);
        let ops = hook_operations(&m, "playground");
        let mut changes = vec![hooks_change(
            "on-run-start",
            Some(json!(["grant usage on schema x"])),
            Some(json!(["grant select on schema x"])),
        )];
        attach_hook_facts(&mut changes, &ops);
        let facts = changes[0].hook.as_ref().expect("hook facts attached");
        assert_eq!(facts.manifest, HookManifestPresence::Diverged);
        assert_eq!(
            facts.operation_ids,
            vec!["operation.playground.playground-on-run-start-0"],
            "the diverged ops are still named, so the row copy can audit them",
        );
        let diff = facts.sql_diff.as_ref().expect("file-side diff");
        assert!(
            diff.lines
                .iter()
                .any(|l| l.kind == crate::domain::pr_diff::DiffLineKind::Added
                    && l.text == "grant select on schema x"),
            "the new side comes from the file, never the stale manifest",
        );
    }

    #[test]
    fn attach_hook_removal_with_a_fresh_manifest_is_matched() {
        // Hooks removed in the PR + recompiled manifest carries no ops:
        // both sides empty — the manifest AGREES with the working tree.
        let ops = HookOperations::default();
        let mut changes = vec![hooks_change(
            "on-run-start",
            Some(json!(["grant usage on schema x"])),
            None,
        )];
        attach_hook_facts(&mut changes, &ops);
        let facts = changes[0].hook.as_ref().expect("hook facts attached");
        assert_eq!(facts.manifest, HookManifestPresence::Matched);
        assert!(facts.operation_ids.is_empty());
        let diff = facts.sql_diff.as_ref().expect("removal diff");
        assert!(
            diff.lines
                .iter()
                .all(|l| l.kind == crate::domain::pr_diff::DiffLineKind::Removed),
        );
    }

    #[test]
    fn attach_leaves_non_hook_rows_untouched() {
        let m = manifest_with(vec![operation_node("playground", "start", 0, "grant x")]);
        let ops = hook_operations(&m, "playground");
        let mut changes = vec![ProjectChange {
            category: ProjectChangeCategory::Vars,
            label: "dq_threshold".to_owned(),
            old: Some(json!(10)),
            new: Some(json!(5)),
            hook: None,
            tree: None,
            vars: None,
        }];
        attach_hook_facts(&mut changes, &ops);
        assert_eq!(changes[0].hook, None);
    }

    #[test]
    fn hook_change_facts_round_trip_through_json() {
        // ADR-5: the payload-riding POD must survive the wire.
        let facts = HookChangeFacts {
            sql_diff: Some(BlockDiff {
                lines: vec![DiffLine {
                    kind: crate::domain::pr_diff::DiffLineKind::Added,
                    text: "grant select".to_owned(),
                    emphasis: Some((6, 12)),
                }],
            }),
            operation_ids: vec!["operation.p.p-on-run-start-0".to_owned()],
            manifest: HookManifestPresence::Matched,
        };
        let json = serde_json::to_string(&facts).expect("serialize");
        let back: HookChangeFacts = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(facts, back);
        // The presence enum serializes snake_case for the JS consumer.
        assert!(json.contains(r#""manifest":"matched""#), "{json}");
    }

    // ----- cute-dbt#267: config-tree change attribution -----
    // (manifest/Node/HashMap imports shared with the #269 hook tests above.)

    #[test]
    fn config_leaf_path_composes_dotted_path_and_label_from_one_authority() {
        let root = ConfigLeafPath {
            section: "models".to_owned(),
            segments: Vec::new(),
            key: "materialized".to_owned(),
        };
        assert_eq!(root.dotted(), "models");
        assert_eq!(root.label(), "models: +materialized");
        let nested = ConfigLeafPath {
            section: "models".to_owned(),
            segments: vec!["shop".to_owned(), "marts".to_owned()],
            key: "tags".to_owned(),
        };
        assert_eq!(nested.dotted(), "models.shop.marts");
        assert_eq!(nested.label(), "models.shop.marts: +tags");
    }

    #[test]
    fn config_attribution_round_trips_through_json() {
        let attribution = ConfigAttribution {
            key: "materialized".to_owned(),
            path: "models.shop.marts".to_owned(),
        };
        let json = serde_json::to_string(&attribution).expect("serialize");
        let back: ConfigAttribution = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(attribution, back);
    }

    #[test]
    fn project_facts_with_attributions_round_trip_and_empty_map_is_omitted() {
        // ADR-5 pin: the attribution map rides ProjectFacts additively —
        // absent it serializes to nothing (pre-#267 byte stability).
        assert_eq!(
            serde_json::to_string(&ProjectFacts::default()).expect("serialize"),
            "{}",
        );
        let facts = ProjectFacts {
            definition: None,
            panel: None,
            config_attributions: BTreeMap::from([(
                "model.shop.dim_a".to_owned(),
                vec![ConfigAttribution {
                    key: "materialized".to_owned(),
                    path: "models.shop".to_owned(),
                }],
            )]),
            var_references: BTreeMap::new(),
        };
        let json = serde_json::to_string(&facts).expect("serialize");
        let back: ProjectFacts = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(facts, back);
    }

    /// A model node with the given full id and fqn.
    fn fqn_model(id: &str, fqn: &[&str]) -> (NodeId, Node) {
        let node_id = NodeId::new(id);
        let node = Node::new(
            node_id.clone(),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_fqn(fqn.iter().map(|s| (*s).to_owned()).collect());
        (node_id, node)
    }

    /// A manifest holding exactly the given nodes.
    fn manifest_of(nodes: Vec<(NodeId, Node)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().collect::<HashMap<_, _>>(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    /// A `models:` config tree from `(path-segments, key, value)` leaves.
    fn models_tree(leaves: &[(&[&str], &str, Value)]) -> BTreeMap<String, ConfigTree> {
        let mut root = ConfigTree::default();
        for (segments, key, value) in leaves {
            let mut node = &mut root;
            for segment in *segments {
                node = node.children.entry((*segment).to_owned()).or_default();
            }
            node.configs.insert((*key).to_owned(), value.clone());
        }
        BTreeMap::from([("models".to_owned(), root)])
    }

    /// A `ProjectDefinition` carrying only the given `models:` tree.
    fn def_with_models_tree(leaves: &[(&[&str], &str, Value)]) -> ProjectDefinition {
        ProjectDefinition {
            config_trees: models_tree(leaves),
            ..ProjectDefinition::default()
        }
    }

    /// Run the full pipeline under test: structural diff → attribution.
    fn attribute(
        current: &Manifest,
        old: &ProjectDefinition,
        new: &ProjectDefinition,
    ) -> BTreeMap<String, Vec<ConfigAttribution>> {
        let changes = diff_project_definitions(old, new);
        attribute_config_tree_changes(current, old, new, &changes)
    }

    #[test]
    fn attribution_selects_models_under_the_edited_subtree_by_fqn_prefix() {
        // The flagship case: a marts-folder edit selects exactly the
        // models whose fqn descends through ["shop", "marts"].
        let current = manifest_of(vec![
            fqn_model("model.shop.fct_orders", &["shop", "marts", "fct_orders"]),
            fqn_model("model.shop.stg_raw", &["shop", "staging", "stg_raw"]),
        ]);
        let old = def_with_models_tree(&[(&["shop", "marts"], "materialized", json!("view"))]);
        let new = def_with_models_tree(&[(&["shop", "marts"], "materialized", json!("table"))]);
        let map = attribute(&current, &old, &new);
        assert_eq!(
            map.keys().collect::<Vec<_>>(),
            vec!["model.shop.fct_orders"],
            "only the marts model is selected",
        );
        assert_eq!(
            map["model.shop.fct_orders"],
            vec![ConfigAttribution {
                key: "materialized".to_owned(),
                path: "models.shop.marts".to_owned(),
            }],
        );
    }

    #[test]
    fn attribution_root_tree_edit_selects_all_own_project_models() {
        // A section-root key applies to every model whose resolution is
        // not shadowed deeper — with no deeper setters, that is ALL
        // fqn-bearing models, own-project and package alike.
        let current = manifest_of(vec![
            fqn_model("model.shop.fct_orders", &["shop", "marts", "fct_orders"]),
            fqn_model("model.shop.stg_raw", &["shop", "staging", "stg_raw"]),
            fqn_model("model.dbt_utils.helper", &["dbt_utils", "helper"]),
        ]);
        let old = def_with_models_tree(&[]);
        let new = def_with_models_tree(&[(&[], "persist_docs", json!({"relation": true}))]);
        let map = attribute(&current, &old, &new);
        assert_eq!(map.len(), 3, "the root tree reaches every model");
        for attributions in map.values() {
            assert_eq!(attributions[0].path, "models", "the chip names the root");
        }
    }

    #[test]
    fn attribution_package_subtree_never_selects_own_project_models() {
        // models.dbt_utils.… is a package subtree: the fqn's first
        // segment is the package name, so an own-project fqn can never
        // descend into it.
        let current = manifest_of(vec![
            fqn_model("model.shop.fct_orders", &["shop", "marts", "fct_orders"]),
            fqn_model("model.dbt_utils.helper", &["dbt_utils", "helper"]),
        ]);
        let old = def_with_models_tree(&[(&["dbt_utils"], "enabled", json!(true))]);
        let new = def_with_models_tree(&[(&["dbt_utils"], "enabled", json!(false))]);
        let map = attribute(&current, &old, &new);
        assert_eq!(
            map.keys().collect::<Vec<_>>(),
            vec!["model.dbt_utils.helper"],
            "only the package's own models are under its subtree",
        );
        assert_eq!(map["model.dbt_utils.helper"][0].path, "models.dbt_utils");
    }

    #[test]
    fn attribution_deepest_match_wins_a_shadowed_edit_selects_nothing_under_the_shadow() {
        // fusion's deepest-match-wins: editing the project-level value
        // changes nothing for models under a subtree that (unchanged)
        // sets the same key deeper — those models' resolved config did
        // not change, so they are NOT selected (sound, not heuristic).
        let current = manifest_of(vec![
            fqn_model("model.shop.fct_orders", &["shop", "marts", "fct_orders"]),
            fqn_model("model.shop.stg_raw", &["shop", "staging", "stg_raw"]),
        ]);
        let old = def_with_models_tree(&[
            (&["shop"], "materialized", json!("view")),
            (&["shop", "marts"], "materialized", json!("table")),
        ]);
        let new = def_with_models_tree(&[
            (&["shop"], "materialized", json!("ephemeral")),
            (&["shop", "marts"], "materialized", json!("table")),
        ]);
        let map = attribute(&current, &old, &new);
        assert_eq!(
            map.keys().collect::<Vec<_>>(),
            vec!["model.shop.stg_raw"],
            "the marts model is shadowed by its unchanged deeper setting",
        );
        assert_eq!(
            map["model.shop.stg_raw"][0].path, "models.shop",
            "the chip names the edited (winning) node",
        );
    }

    #[test]
    fn attribution_removal_attributes_to_the_removed_leaf() {
        // Removing the deeper setting re-exposes the shallower one: the
        // resolved value changes and the chip names the REMOVAL site
        // (the deeper of the two winners).
        let current = manifest_of(vec![fqn_model(
            "model.shop.fct_orders",
            &["shop", "marts", "fct_orders"],
        )]);
        let old = def_with_models_tree(&[
            (&["shop"], "materialized", json!("view")),
            (&["shop", "marts"], "materialized", json!("table")),
        ]);
        let new = def_with_models_tree(&[(&["shop"], "materialized", json!("view"))]);
        let map = attribute(&current, &old, &new);
        assert_eq!(
            map["model.shop.fct_orders"],
            vec![ConfigAttribution {
                key: "materialized".to_owned(),
                path: "models.shop.marts".to_owned(),
            }],
        );
    }

    #[test]
    fn attribution_addition_attributes_to_the_added_leaf() {
        // Adding a deeper setting shadows the shallower one: the chip
        // names the ADDITION site (the new, deeper winner).
        let current = manifest_of(vec![fqn_model(
            "model.shop.fct_orders",
            &["shop", "marts", "fct_orders"],
        )]);
        let old = def_with_models_tree(&[(&["shop"], "tags", json!(["all"]))]);
        let new = def_with_models_tree(&[
            (&["shop"], "tags", json!(["all"])),
            (&["shop", "marts"], "tags", json!(["nightly"])),
        ]);
        let map = attribute(&current, &old, &new);
        assert_eq!(
            map["model.shop.fct_orders"],
            vec![ConfigAttribution {
                key: "tags".to_owned(),
                path: "models.shop.marts".to_owned(),
            }],
        );
    }

    #[test]
    fn attribution_of_a_non_config_tree_change_is_empty() {
        // A vars-only diff changes no models: tree (reflexivity over the
        // models tree: identical trees ⇒ identical resolution).
        let current = manifest_of(vec![fqn_model(
            "model.shop.fct_orders",
            &["shop", "marts", "fct_orders"],
        )]);
        let mut old = def_with_models_tree(&[(&["shop"], "materialized", json!("view"))]);
        let mut new = old.clone();
        old.vars.insert("flag".to_owned(), json!(1));
        new.vars.insert("flag".to_owned(), json!(2));
        assert!(attribute(&current, &old, &new).is_empty());
    }

    #[test]
    fn attribution_never_selects_an_empty_fqn_or_non_model_node() {
        // A model with no fqn (pre-#278 manifest) cannot be matched —
        // the matcher refuses to guess. A non-model node never matches.
        let (seed_id, seed) = {
            let node_id = NodeId::new("seed.shop.raw_payers");
            let node = Node::new(
                node_id.clone(),
                "seed",
                Checksum::new("sha256", "ck"),
                None,
                None,
                DependsOn::default(),
                None,
                NodeConfig::default(),
                None,
                BTreeMap::new(),
            )
            .with_fqn(vec!["shop".to_owned(), "raw_payers".to_owned()]);
            (node_id, node)
        };
        let current = manifest_of(vec![fqn_model("model.shop.legacy", &[]), (seed_id, seed)]);
        let old = def_with_models_tree(&[(&[], "materialized", json!("view"))]);
        let new = def_with_models_tree(&[(&[], "materialized", json!("table"))]);
        assert!(attribute(&current, &old, &new).is_empty());
    }

    #[test]
    fn attribution_ignores_non_models_sections() {
        // A seeds: tree edit resolves against seed nodes, not models —
        // this slice attributes (and widens) the models: section only.
        let current = manifest_of(vec![fqn_model(
            "model.shop.fct_orders",
            &["shop", "marts", "fct_orders"],
        )]);
        let tree = |value: Value| {
            let mut trees = BTreeMap::new();
            trees.insert(
                "seeds".to_owned(),
                ConfigTree {
                    configs: BTreeMap::from([("quote_columns".to_owned(), value)]),
                    children: BTreeMap::new(),
                },
            );
            ProjectDefinition {
                config_trees: trees,
                ..ProjectDefinition::default()
            }
        };
        assert!(attribute(&current, &tree(json!(false)), &tree(json!(true))).is_empty());
    }

    #[test]
    fn attribution_membership_is_exactly_contiguous_fqn_prefix_descent() {
        // House property style (exhaustive structured space, no proptest):
        // with a single edited key and no other setters, a model is
        // selected IFF the edited path is a contiguous prefix of its fqn
        // — fusion's get_config_for_fqn walk breaks on the first missing
        // child, so a deeper segment match without its ancestors never
        // fires.
        let edit_paths: &[&[&str]] = &[
            &[],
            &["shop"],
            &["shop", "marts"],
            &["shop", "marts", "fct_orders"],
            &["dbt_utils"],
            &["marts"], // a bare folder name: never an fqn FIRST segment below
        ];
        let fqns: &[&[&str]] = &[
            &["shop", "marts", "fct_orders"],
            &["shop", "marts", "dim_x"],
            &["shop", "staging", "stg_raw"],
            &["shop", "one_file_model"],
            &["dbt_utils", "helper"],
        ];
        for edit_path in edit_paths {
            let old = def_with_models_tree(&[(edit_path, "materialized", json!("view"))]);
            let new = def_with_models_tree(&[(edit_path, "materialized", json!("table"))]);
            for (i, fqn) in fqns.iter().enumerate() {
                let id = format!("model.p.m{i}");
                let current = manifest_of(vec![fqn_model(&id, fqn)]);
                let map = attribute(&current, &old, &new);
                let expected = fqn.len() >= edit_path.len()
                    && edit_path.iter().zip(fqn.iter()).all(|(a, b)| a == b);
                assert_eq!(
                    map.contains_key(&id),
                    expected,
                    "edit at {edit_path:?} vs fqn {fqn:?}: selection must equal \
                     contiguous-prefix membership",
                );
            }
        }
    }

    // ----- standing config provenance (cute-dbt#270) -----

    #[test]
    fn provenance_resolves_deepest_match_wins_per_key() {
        // The section root sets materialized=view; marts overrides to
        // table. A marts model resolves table via models.shop.marts; a
        // staging model resolves view via the section root.
        let current = manifest_of(vec![
            fqn_model("model.shop.fct_orders", &["shop", "marts", "fct_orders"]),
            fqn_model("model.shop.stg_raw", &["shop", "staging", "stg_raw"]),
        ]);
        let def = def_with_models_tree(&[
            (&[], "materialized", json!("view")),
            (&["shop", "marts"], "materialized", json!("table")),
        ]);
        let map = resolve_model_configs(&current, &def);
        assert_eq!(
            map["model.shop.fct_orders"],
            vec![ConfigProvenance {
                key: "materialized".to_owned(),
                value: json!("table"),
                path: "models.shop.marts".to_owned(),
            }],
            "the marts model wins the deepest setter",
        );
        assert_eq!(
            map["model.shop.stg_raw"],
            vec![ConfigProvenance {
                key: "materialized".to_owned(),
                value: json!("view"),
                path: "models".to_owned(),
            }],
            "the staging model resolves the section-root value",
        );
    }

    #[test]
    fn provenance_collects_every_applicable_key_sorted() {
        let current = manifest_of(vec![fqn_model(
            "model.shop.fct_orders",
            &["shop", "marts", "fct_orders"],
        )]);
        let def = def_with_models_tree(&[
            (&["shop"], "tags", json!(["core"])),
            (&["shop", "marts"], "materialized", json!("table")),
        ]);
        let map = resolve_model_configs(&current, &def);
        assert_eq!(
            map["model.shop.fct_orders"],
            vec![
                ConfigProvenance {
                    key: "materialized".to_owned(),
                    value: json!("table"),
                    path: "models.shop.marts".to_owned(),
                },
                ConfigProvenance {
                    key: "tags".to_owned(),
                    value: json!(["core"]),
                    path: "models.shop".to_owned(),
                },
            ],
            "both keys resolve, sorted by key",
        );
    }

    #[test]
    fn provenance_is_empty_for_a_config_free_project() {
        let current = manifest_of(vec![fqn_model("model.shop.m", &["shop", "m"])]);
        let map = resolve_model_configs(&current, &ProjectDefinition::default());
        assert!(map.is_empty());
    }

    #[test]
    fn provenance_round_trips_through_json() {
        let prov = ConfigProvenance {
            key: "materialized".to_owned(),
            value: json!("incremental"),
            path: "models.shop.marts".to_owned(),
        };
        let json = serde_json::to_string(&prov).expect("serialize");
        let back: ConfigProvenance = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(prov, back);
    }
}
