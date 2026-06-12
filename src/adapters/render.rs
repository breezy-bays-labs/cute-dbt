//! askama 0.16 renderer that produces the v0.1 `report.html` from the
//! cute-dbt domain.
//!
//! Composition layer between [`Manifest`] / [`InScopeSet`] /
//! [`ModelInScopeSet`] / [`CteGraph`] and the askama template at
//! `templates/report.html`. The template owns DOM + class + JS structure;
//! this module owns:
//!
//! - **Per-model payload assembly.** Walks `models_in_scope`, resolves
//!   each model node, builds a [`ModelPayload`] carrying its CTE graph,
//!   per-CTE compiled SQL, and the list of in-scope unit tests targeting
//!   it. Models with zero in-scope unit tests render the "0 unit tests
//!   wired" empty state.
//! - **CTE graph parsing.** Invokes [`parse_cte_graph`] once per in-scope
//!   model. The Stage-2 preflight already proves `compiled_code` is
//!   `Some` for every in-scope model; a parse failure here is treated as
//!   an empty graph (the renderer surfaces "no DAG available" — the
//!   report stays valid, the model card is just sparse).
//! - **Node-role classification.** Walks the [`CteGraph`]; the terminal
//!   node (named [`TERMINAL_NODE_NAME`]) is `final`; a CTE whose body is
//!   a plain `SELECT … FROM <single relation>` with zero incoming edges
//!   is `import`; everything else is `transform`.
//! - **Import-CTE binding.** Parses `ref('NAME')` out of each unit
//!   test's `given[].input`, then locates the matching leaf-CTE node
//!   in two passes (case-insensitive). Pass-1: name match — an
//!   import-CTE whose own name equals `NAME` (the unwrapper
//!   convention; strict role gate so a transform CTE cannot
//!   spuriously bind). Pass-2: body match — any non-terminal leaf
//!   CTE (zero incoming edges) whose engine-extracted
//!   `body_leaf_table_refs` contain `NAME`. Pass-2 catches both the
//!   dbt-idiomatic `with source as (select * from
//!   "db"."schema"."MODEL")` shape and the messy multi-ref case
//!   (cute-dbt#34) where one CTE body references multiple `ref()`
//!   targets via `UNION ALL`, `JOIN`, or derived subqueries —
//!   classified `Transform` in the DAG but still a valid binding
//!   surface for every leaf ref. The template stacks every matching
//!   given vertically; unmatched givens against an import-CTE
//!   surface "no fixture provided — dbt treats unspecified inputs
//!   as empty". `source()` references are NOT yet bound — tracked
//!   as cute-dbt#57 for v0.2.
//!
//! ## Security
//!
//! The JSON payload is emitted into a `<script type="application/json">`
//! block via askama's `| json | safe` pipeline. askama's `json` filter
//! escapes `<`, `>`, `&`, and `'` (per the askama 0.16 book), so a
//! manifest-derived `</script>` substring cannot break out of the JSON
//! block. The Mermaid runtime renders DAGs from this same payload under
//! `securityLevel: 'strict'`; click-to-expand is wired by external jQuery
//! handlers binding to the rendered SVG `<g>` elements — never Mermaid's
//! `click` directive (which `'strict'` disables). See
//! [`ARCHITECTURE.md` §5](../../../ARCHITECTURE.md) for the zero-egress
//! gate this preserves.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use askama::Template;
use serde::Serialize;
use serde_json::Value;

use crate::adapters::asset_embed::{
    APPEARANCE_JS, CYTO_DAG_JS, CYTOSCAPE_JS, DATATABLES_CSS, DATATABLES_JS, FAVICON_DATA_URI,
    INTERACTION_JS, JQUERY_JS, MERMAID_JS, REPORT_CSS, SAKURA_CSS, THEME_JS,
};
use crate::adapters::cte_engine::{TERMINAL_NODE_NAME, parse_cte_graph};
use crate::domain::{
    BANNER_EMPTY_SCOPE, BlockDiff, CheckId, CheckPolicy, CteGraph, DiffLine, DiffLineKind,
    EdgeType, Finding, FixtureTable, HeuristicId, InScopeSet, Instrument, Manifest,
    ModelInScopeSet, ModelYamlOutcome, Node, NodeId, ProjectChange, ProjectChangeCategory,
    ProjectChangePanel, ProjectDefinition, ProjectFacts, ProjectFallbackReason, SourceNode,
    TestMetadata, Tier, UnitTest, UnitTestDataDiff, UnitTestGiven, UnitTestOverrides,
    UnitTestYamlBlock, apply_check_policy, model_findings, resolve_target_model,
    resolve_tested_model, table_from_manifest_rows,
};

/// Snake-case wire key for an [`EdgeType`] — the exact JSON-serde string
/// the JS `JOIN_COLORS` map keys are matched against.
///
/// Exhaustive match — adding a new [`EdgeType`] variant fails to compile
/// here, which keeps the render-side palette in lockstep with the
/// classifier vocabulary. The `edge-vocab-completeness` CI guard greps
/// this match (and the JS palette in `templates/report.html`) so the
/// invariant is structurally enforced both at compile time and at CI
/// time.
#[must_use]
pub fn edge_type_wire_key(edge_type: EdgeType) -> &'static str {
    match edge_type {
        EdgeType::From => "from",
        EdgeType::Inner => "inner",
        EdgeType::Left => "left",
        EdgeType::Right => "right",
        EdgeType::Full => "full",
        EdgeType::Cross => "cross",
        EdgeType::UnionAll => "union_all",
        EdgeType::UnionDistinct => "union_distinct",
    }
}

/// Role of a node in the rendered CTE DAG.
///
/// Classification happens at the render layer (not in `domain`) because
/// it depends on graph topology (incoming edges) and on the terminal
/// node name, both of which are properties of the parsed graph rather
/// than the dbt manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// The terminal `SELECT` — keyed by [`TERMINAL_NODE_NAME`] in the
    /// graph, rendered as the model's bare name in the payload.
    Final,
    /// A leaf CTE whose body is a plain `SELECT … FROM <single relation>`
    /// with no incoming edges — the moral equivalent of a dbt source.
    Import,
    /// Anything else — joins, transformations, intermediate CTEs.
    Transform,
}

/// Parse the bare name out of a unit-test `given[].input` string of the
/// form `ref('NAME')` / `ref("NAME")` (case-insensitive `ref`; either
/// quote style — dbt accepts both in authored YAML and both engines
/// ship the authored string **verbatim** on the manifest wire,
/// cute-dbt#245).
///
/// The keyword check is case-insensitive across any byte casing
/// (`ref` / `REF` / `Ref` / `rEf` / …) and tolerates whitespace between
/// the keyword and the opening parenthesis (`ref ('x')`, `REF\t('y')`,
/// etc. — Jinja's `{{ ref(...) }}` macro accepts this).
///
/// Returns `None` when the input does not match the `ref('…')` shape,
/// when the inner name is empty, or when the parentheses / quotes are
/// unbalanced or mismatched (open/close must be the same character —
/// see `strip_matching_quotes`). The caller (`bind_import_to_given`)
/// treats `None` as "no import-CTE match" and surfaces the design's
/// empty-state copy.
#[must_use]
pub fn parse_ref_name(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let prefix = trimmed.get(..3)?;
    if !prefix.eq_ignore_ascii_case("ref") {
        return None;
    }
    let after_ref = trimmed[3..].trim_start();
    let inside = after_ref.strip_prefix('(')?.strip_suffix(')')?;
    let name = strip_matching_quotes(inside.trim())?;
    if name.is_empty() { None } else { Some(name) }
}

/// Strip one pair of **matching** string-literal quotes (`'…'` or
/// `"…"`) from the ends of `s`.
///
/// dbt given inputs are authored as Python/Jinja string literals, which
/// accept either quote character but require the open and close to be
/// the same one. A mixed pair (`"x'`), an unbalanced quote (`'x`), or a
/// bare token returns `None` — the fail-open posture both callers
/// ([`parse_ref_name`] / [`parse_source_ref`]) rely on (cute-dbt#245).
fn strip_matching_quotes(s: &str) -> Option<&str> {
    s.strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .or_else(|| s.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')))
}

/// Parse the `(source_name, table_name)` pair out of a unit-test
/// `given[].input` string of the form `source('a', 'b')` (cute-dbt#57)
/// — the exact sibling of [`parse_ref_name`].
///
/// Both engines serialize the given input **verbatim** from the authored
/// YAML (fusion clones the string into the manifest node; core
/// round-trips it byte-for-byte), and dbt's `source()` Jinja function
/// takes exactly two arguments, so the manifest string is the literal
/// authored call. The same tolerances as [`parse_ref_name`] apply:
/// case-insensitive keyword (`source` / `SOURCE` / …), whitespace
/// between the keyword and the parenthesis and around the arguments,
/// and either quote style with the matching-quote rule applied **per
/// argument** (each arg is its own string literal, so
/// `source("a", 'b')` is engine-valid; cute-dbt#245 — the
/// `name=`/`table_name=` kwargs variants stay engine-valid-but-rare and
/// deliberately deferred).
///
/// Returns `None` when the input does not match the `source('…','…')`
/// shape — including a call with more than two top-level arguments —
/// or when either name is empty. The caller treats `None` as "no
/// import-CTE match" and surfaces the design's empty-state copy
/// (fail-open, same as an unresolvable `ref`).
#[must_use]
pub fn parse_source_ref(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim();
    let prefix = trimmed.get(..6)?;
    if !prefix.eq_ignore_ascii_case("source") {
        return None;
    }
    let after_keyword = trimmed[6..].trim_start();
    let inside = after_keyword.strip_prefix('(')?.strip_suffix(')')?;
    // Top-level comma split into AT MOST two parts: dbt's source()
    // takes exactly two arguments, so a third comma-separated part —
    // a malformed 3-arg call, or a comma inside a quoted name pushing
    // the split past two fragments — rejects the call outright rather
    // than stripping the tail to a garbage pair (CodeRabbit PR #248).
    // A comma inside a quoted name that still yields exactly two parts
    // leaves a fragment with an unbalanced or mismatched quote pair,
    // which the matching-quote strip below rejects. Both paths fall
    // through to `None` — fail-open by construction.
    let mut parts = inside.splitn(3, ',');
    let first = parts.next()?;
    let second = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let source_name = strip_matching_quotes(first.trim())?;
    let table_name = strip_matching_quotes(second.trim())?;
    if source_name.is_empty() || table_name.is_empty() {
        None
    } else {
        Some((source_name, table_name))
    }
}

/// Bind a unit test's `given[]` entries to a named import-CTE.
///
/// Returns the first `given` whose `input` parses to `ref('NAME')` with
/// `NAME` matching `cte_name` (case-insensitive). dbt's manifest uses
/// case-insensitive identifiers for `ref(...)`; the case-sensitive
/// `CteNode::name()` value is matched against the case-folded ref name.
#[must_use]
pub fn bind_import_to_given<'t>(
    unit_test: &'t UnitTest,
    cte_name: &str,
) -> Option<&'t UnitTestGiven> {
    let target = cte_name.to_ascii_lowercase();
    unit_test.given().iter().find(|given| {
        parse_ref_name(given.input()).is_some_and(|n| n.eq_ignore_ascii_case(&target))
    })
}

/// Classify a graph node into a [`NodeRole`].
///
/// The terminal node (whose name equals [`TERMINAL_NODE_NAME`]) is
/// `Final`. A CTE with zero incoming edges whose body the engine
/// classified as `is_simple_from_shape` is `Import`. Everything else
/// is `Transform`. The renderer reads the engine-computed POD fact
/// directly; it never re-parses the CTE body slice (cute-dbt#40).
#[must_use]
pub fn classify_node_role(graph: &CteGraph, node_index: usize) -> NodeRole {
    let Some(node) = graph.nodes().get(node_index) else {
        return NodeRole::Transform;
    };
    if node.name() == TERMINAL_NODE_NAME {
        return NodeRole::Final;
    }
    let has_incoming = graph.edges().iter().any(|edge| edge.to() == node_index);
    if has_incoming {
        return NodeRole::Transform;
    }
    if node.is_simple_from_shape() {
        NodeRole::Import
    } else {
        NodeRole::Transform
    }
}

/// Per-model entry in the JSON payload — mirrors the design's
/// `window.CUTE_DBT_SAMPLE.models[i]` shape so the inlined interaction
/// script consumes it without remapping.
#[derive(Debug, Clone, Serialize)]
pub struct ModelPayload {
    /// Bare model name (e.g. `customer_rollup`) — the model selector
    /// label and the terminal-node id in the DAG.
    pub name: String,
    /// Full project-relative path of the model's source file
    /// (`Node::original_file_path`, e.g. `models/staging/stg_orders.sql`)
    /// — the Model-SQL code-card file-path header (cute-dbt#179; founder
    /// call: the full `models/…/x.sql`, not the bare filename). `None`
    /// (key omitted, older fixtures stay byte-stable) when the manifest
    /// carries no `original_file_path`; the JS then falls back to the
    /// synthesized `<name>.sql` (the cute-dbt#155 terminal label).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Authored model description (cute-dbt#200) — the model node's
    /// top-level `description`, surfaced as the in-card model context
    /// (handoff README §2.5). `None` (key omitted — pre-#200 payloads
    /// and undescribed models stay byte-stable) when the manifest
    /// carries no prose (the adapter drops dbt-core's empty-string
    /// unset shape).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// DAG nodes + edges, keyed for the design's JS.
    pub dag: DagPayload,
    /// Per-node compiled SQL, keyed by node id (CTE name or model name
    /// for the terminal). Empty when the CTE engine could not parse
    /// (the model card still renders the metadata + tests + an empty DAG).
    pub compiled_sql: BTreeMap<String, String>,
    /// Raw Jinja source of the model file (`models/**/*.sql`).
    /// Surfaced verbatim in the per-model "Model SQL" expandable
    /// section (cute-dbt#47). `None` only when the manifest lacks
    /// `raw_code` (defensive — dbt 1.8+ populates this on every node).
    /// Skipped from the JSON payload when `None` so older fixtures
    /// stay stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_sql: Option<String>,
    /// Inline diff of this model's RAW SQL (`raw_code`) when the PR diff
    /// changed the model's `.sql` (cute-dbt#111) — present only in PR-diff
    /// mode, block aligned + touched + a substantive (non-whitespace)
    /// change. `None` (key omitted) for baseline mode, models in scope
    /// only via a changed test, stale diffs, and whitespace-only edits, so
    /// the template's Model SQL section falls back to the plain raw view.
    /// Mirrors [`TestPayload::yaml_diff`]'s threading exactly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_diff: Option<BlockDiff>,
    /// The model's authored schema-file `models:` entry (cute-dbt#247) —
    /// the Model-YAML section, peer of Model SQL. Built from the cli's
    /// `gather_model_yaml` outcome (`Node.patch_path` read through the
    /// `ProjectFileReader`): either the sliced block (`raw`, plus a
    /// `diff` in PR-diff mode when the diff edited it) or a truthful
    /// `missing` placeholder naming what is absent. `None` (key omitted
    /// — pre-#247 payloads and render paths that skip the gather stay
    /// byte-stable) only when no gather outcome exists for the model;
    /// the template then hides the section entirely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_yaml: Option<ModelYamlPayload>,
    /// Unit tests targeting this model that are in scope. Empty
    /// `[]` triggers the "0 unit tests wired" empty state.
    pub tests: Vec<TestPayload>,
    /// `true` when the model's compiled SQL was a `WITH RECURSIVE`
    /// query — the template surfaces a banner; the recursive arm has
    /// already been omitted from the graph by the CTE engine.
    pub is_recursive: bool,
    /// `true` when `config.materialized == "incremental"` (cute-dbt#145).
    /// Drives the model-header incremental badge and gates the per-test
    /// mode badge / expect-semantics tooltip (the template reads this
    /// enclosing-model flag — the `is_recursive` precedent — rather than
    /// denormalizing it onto each test). Serialized only on incremental
    /// models (`skip_serializing_if = std::ops::Not::not`) so the example
    /// diff stays localized to incremental models; the template's
    /// `!(m && m.is_incremental)` read is undefined-safe when omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_incremental: bool,
    /// Coverage-intelligence findings for this model (cute-dbt#169) —
    /// the per-(construct, check) verdicts the check engine computed
    /// during payload assembly ([`model_findings`]: evaluate ALL
    /// registered checks → resolve supersedes; display filtering is a
    /// separate downstream concern). Since cute-dbt#170 each entry is a
    /// [`FindingPayload`] — the domain [`Finding`] flattened verbatim
    /// plus the render-resolved `pin_node` / `sketches` fields the
    /// findings surface consumes. Omitted from JSON when empty so every
    /// pre-#169 payload (and the committed goldens whose models trip no
    /// check) stays byte-stable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<FindingPayload>,
}

/// The Model-YAML section's render shape (cute-dbt#247): the model's
/// authored schema-file `models:` entry, or a truthful degrade.
///
/// Exactly one of [`raw`](Self::raw) / [`missing`](Self::missing) is
/// set. `Rust computes, JS only renders`: the degrade copy is composed
/// here (the private `model_yaml_payload` mapping) from the domain
/// [`ModelYamlOutcome`], so the wording lives in one testable place and
/// the template never invents text.
#[derive(Debug, Clone, Serialize)]
pub struct ModelYamlPayload {
    /// Project-relative schema file path (the scheme-stripped manifest
    /// `patch_path`) — the code-card header label. `None` (key omitted)
    /// only on the no-`patch_path` degrade (there is no file to name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The sliced authored `models:` entry, verbatim (the
    /// [`crate::domain::extract_model_block`] slice — leading/trailing
    /// comments included). `None` on every degrade arm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Inline diff of the block (PR-diff mode, block aligned + touched +
    /// substantive — `attach_model_yaml_diffs`). `None` in baseline mode
    /// and whenever the diff did not edit this block, so the section
    /// shows the plain File view. Same Diff/File semantics as
    /// [`ModelPayload::sql_diff`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<BlockDiff>,
    /// Truthful placeholder copy when the authored block could not be
    /// surfaced — names exactly what is missing (no `patch_path`, no
    /// `--project-root`, file missing/unreadable, entry not found). The
    /// template renders this text verbatim; it never shows an empty or
    /// misleading section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
}

/// Map a model's gather outcome onto the Model-YAML render shape
/// (cute-dbt#247). `None` in → `None` out (no gather ran for this model
/// — direct render paths and the explore arm — so the template hides
/// the section). `model_name` is the bare model name, used by the
/// entry-not-found copy.
fn model_yaml_payload(
    outcome: Option<&ModelYamlOutcome>,
    model_name: &str,
) -> Option<ModelYamlPayload> {
    let payload = match outcome? {
        ModelYamlOutcome::Found { path, block, diff } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: Some(block.raw.clone()),
            diff: diff.clone(),
            missing: None,
        },
        ModelYamlOutcome::NoPatchPath => ModelYamlPayload {
            path: None,
            raw: None,
            diff: None,
            missing: Some(
                "No schema file declares this model in the manifest — there is no authored \
                 models: entry to show."
                    .to_owned(),
            ),
        },
        ModelYamlOutcome::NoProjectRoot { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!(
                "Schema file {path} was not read — re-run with --project-root to surface the \
                 authored model YAML."
            )),
        },
        ModelYamlOutcome::FileMissing { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!(
                "Schema file {path} was not found under the project root."
            )),
        },
        ModelYamlOutcome::Unreadable { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!("Schema file {path} could not be read.")),
        },
        ModelYamlOutcome::EntryNotFound { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!(
                "No models: entry named \"{model_name}\" was found in {path}."
            )),
        },
    };
    Some(payload)
}

/// One coverage finding in render shape (cute-dbt#170): the domain
/// [`Finding`] — serialized FLAT, so every cute-dbt#169 wire key
/// (`check` / `tier` / `instrument` / `model_id` / `construct` /
/// `verdict` / `evidence` / `recommendation` / `suppressed`) is
/// unchanged — plus the two render-resolved fields the findings panel
/// consumes. Rust computes, JS only renders.
#[derive(Debug, Clone, Serialize)]
pub struct FindingPayload {
    /// The policy-applied domain finding, flattened onto this object.
    #[serde(flatten)]
    pub finding: Finding<HeuristicId>,
    /// The DAG node id this finding's evidence pins to: a
    /// `group[<node>]` construct (e.g. `union[combined_metrics]`) pins
    /// the named CTE node; a model-level construct (e.g.
    /// `config.unique_key`) pins the terminal node. `None` (key
    /// omitted) when the model's graph is empty or the named node is
    /// not in the graph — the template then renders no pin affordance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_node: Option<String>,
    /// Copy-pasteable fixture sketches LIFTED out of the evidence list
    /// (the `SUGGESTED_GIVEN_LABEL` entries the union check emits,
    /// cute-dbt#172) — the template renders each as a copyable code
    /// block instead of a plain evidence row. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sketches: Vec<String>,
}

/// The evidence label the union check stamps on its copy-pasteable
/// given-row sketches (`suggested_given_sketch` in
/// `src/domain/checks.rs`). [`finding_payload`] lifts these entries into
/// [`FindingPayload::sketches`]; everything else stays plain evidence.
const SUGGESTED_GIVEN_LABEL: &str = "suggested given";

/// Resolve the DAG node a finding pins to (see
/// [`FindingPayload::pin_node`]). Bracketed constructs name an engine
/// node verbatim (`union[<consumer>]`) or qualify it with a sub-construct
/// segment (`left_join[<consumer>:<right>]`, cute-dbt#173) — the consumer
/// node is the pin either way; the match is case-insensitive because SQL
/// identifiers fold.
fn resolve_pin_node(graph: &CteGraph, construct: &str) -> Option<String> {
    let node_named = |name: &str| {
        graph
            .nodes()
            .iter()
            .find(|node| node.name().eq_ignore_ascii_case(name))
            .map(|node| node.name().to_owned())
    };
    let named = construct
        .find('[')
        .and_then(|open| construct[open + 1..].strip_suffix(']'))
        .and_then(|name| {
            node_named(name).or_else(|| {
                name.split(':')
                    .next()
                    .filter(|c| !c.is_empty())
                    .and_then(node_named)
            })
        });
    named.or_else(|| node_named(TERMINAL_NODE_NAME))
}

/// Wrap one policy-applied domain [`Finding`] into its render shape:
/// resolve the pin target and lift the `suggested given` evidence
/// entries into [`FindingPayload::sketches`].
fn finding_payload(graph: &CteGraph, mut finding: Finding<HeuristicId>) -> FindingPayload {
    let pin_node = resolve_pin_node(graph, &finding.construct);
    let (sketches, evidence): (Vec<_>, Vec<_>) = std::mem::take(&mut finding.evidence)
        .into_iter()
        .partition(|entry| entry.label == SUGGESTED_GIVEN_LABEL);
    finding.evidence = evidence;
    FindingPayload {
        finding,
        pin_node,
        sketches: sketches.into_iter().map(|entry| entry.value).collect(),
    }
}

/// Base URL of the published book's GENERATED check pages
/// (`book/src/checks/<id>.md` → `<base><id>.html`; mdBook `site-url`
/// `/cute-dbt/`). Rides the payload as [`CheckSpecPayload::book_href`]
/// and renders as a plain click-only `<a>` — never fetched at load, so
/// the zero-egress gate holds (the report makes zero requests until the
/// user deliberately leaves it).
const BOOK_CHECKS_BASE: &str = "https://breezy-bays-labs.github.io/cute-dbt/checks/";

/// The spec catalog entry for one registered check (cute-dbt#170) —
/// everything the inline rationale drawer ("what is this check?")
/// renders OFFLINE, denormalized from the [`HeuristicId`] spec statics
/// so the JS never reaches back into Rust. Carried once per check id in
/// [`ReportPayload::check_specs`], not per finding.
#[derive(Debug, Clone, Serialize)]
pub struct CheckSpecPayload {
    /// Human-facing display name (e.g. `Unexercised UNION arm`).
    pub name: &'static str,
    /// Check group (the dotted id's prefix).
    pub group: &'static str,
    /// Accuracy tier — labeled in the UI, never blended.
    pub tier: Tier,
    /// Recommended testing instrument.
    pub instrument: Instrument,
    /// Prose mirror of the trigger + satisfaction predicate.
    pub conditions: &'static [&'static str],
    /// Shapes the check deliberately stays silent (or `UNKNOWN`) on.
    pub exclusions: &'static [&'static str],
    /// Why the gap matters — embedded inline (zero-egress).
    pub rationale: &'static str,
    /// Outbound link to the generated book check page — click-only.
    pub book_href: String,
}

/// Build the [`CheckSpecPayload`] for one registered check.
fn check_spec_payload(id: HeuristicId) -> CheckSpecPayload {
    let spec = id.spec();
    CheckSpecPayload {
        name: spec.name,
        group: spec.group,
        tier: spec.tier,
        instrument: spec.instrument,
        conditions: spec.conditions,
        exclusions: spec.exclusions,
        rationale: spec.rationale,
        book_href: format!("{BOOK_CHECKS_BASE}{}.html", spec.id_str),
    }
}

/// One DAG node — stable id, display label, and role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodePayload {
    /// Stable node id — the key for the DAG (Mermaid node), the
    /// `compiled_sql` map, edge endpoints, and given→node binding. Always
    /// the engine's node name: a CTE alias for CTE nodes, or the
    /// collision-proof [`TERMINAL_NODE_NAME`] for the terminal. The model's
    /// bare name is NEVER the id (it rides in [`Self::label`]) — keeping it
    /// out of the id is what stops a self-named import CTE (`with orders as
    /// (...)` on the `orders` model) from collapsing into the terminal node
    /// (cute-dbt#155).
    pub id: String,
    /// Human-facing label for the DAG node + the node-detail title.
    /// `Some(model_name)` for the terminal (so it reads as the model, not
    /// the literal `(final select)`); `None` for CTE nodes, where the
    /// template falls back to [`Self::id`]. Omitted from JSON when `None`
    /// so CTE node payloads — and the byte-gated examples — stay minimal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Render-layer classification (see [`NodeRole`]).
    pub role: NodeRole,
}

/// One DAG edge — `from` and `to` are the [`NodePayload::id`] strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgePayload {
    /// Source node id (the [`NodePayload::id`] the edge starts from).
    pub from: String,
    /// Destination node id (the [`NodePayload::id`] the edge ends at).
    pub to: String,
    /// Edge classification driving the Mermaid edge color + legend
    /// entry (`from` / `inner` / `left` / `right` / `full` / `cross` /
    /// `union_all` / `union_distinct`).
    pub edge_type: EdgeType,
}

/// The full DAG carried in a [`ModelPayload`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DagPayload {
    /// CTE nodes plus the terminal model node, in stable rendering
    /// order.
    pub nodes: Vec<NodePayload>,
    /// Directed edges between [`Self::nodes`] entries, classified by
    /// [`EdgePayload::edge_type`].
    pub edges: Vec<EdgePayload>,
}

/// Column-header metadata for one fixture-table column (cute-dbt#165):
/// the model's authored column description plus the summarized
/// column-level data tests. Rendered by the template as the th tooltip
/// bubble — the payload is the complete renderable POD (Rust computes,
/// JS only renders).
///
/// Emitted only for columns that actually appear in the carrying
/// given/expect [`FixtureTable`] AND have at least a description or one
/// test (no empty bubbles); a column with neither simply has no map
/// entry, which the template reads as "no affordance".
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ColumnMetaPayload {
    /// Authored column description from the owning model node's
    /// `columns` map. Omitted from JSON when the column has tests but no
    /// description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Structured column-level data tests (built by
    /// [`column_test_payload`], the handoff README §2.2 display-string
    /// mapping): each entry carries the display `name` plus — distinctly —
    /// the `accepted_values` args (rendered as pills) or the
    /// relationships/range `detail` (muted mono). Deterministically
    /// ordered (name, values, detail, then test node id). Omitted from
    /// JSON when the column is described but untested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ColumnTestPayload>,
}

/// One column-scoped data test in display shape (cute-dbt#178, the
/// handoff README §2.2 contract). The JS renders `name` in the accent
/// color, each `values` entry as a chip, and `detail` as muted mono —
/// so the three are carried distinctly, never pre-joined.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ColumnTestPayload {
    /// Display name: `unique` / `not null` / `accepted values` /
    /// `relationships` / `accepted range` for the known built-ins (the
    /// §2.2 prose forms), else the package-qualified raw test name
    /// (identifiers are never prose-mangled).
    pub name: String,
    /// `accepted_values` args, one chip per authored value. Empty (and
    /// omitted from JSON) for every other test.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// Muted mono detail: `relationships` → `"model.field"`;
    /// `accepted_range` → `"0–100"` / `"≥ 0"` / `"≤ 1"`. `None` (and
    /// omitted) when the test carries no interpretable detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One entry of the report-level `manifest_nodes` lookup (cute-dbt#200,
/// handoff README §2.5–2.6): the model context the node-detail shelf
/// (cute-dbt#201) and the model-ref / expected-model hover cards
/// (cute-dbt#202) render — keyed by BARE model name in
/// [`ReportPayload::manifest_nodes`]. A model absent from the lookup is
/// the graceful no-hover (the JS contract); a present entry always has
/// at least one non-empty field (`build_manifest_nodes` skips
/// all-empty entries so bare synthetic fixtures stay byte-stable).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ManifestNodePayload {
    /// Authored model description ([`Node::description`] — the adapter
    /// already dropped dbt-core's empty-string unset shape).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `config.materialized` (`"view"` / `"table"` / `"incremental"` /
    /// …) — the already-ingested cute-dbt#145 accessor, re-plumbed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialized: Option<String>,
    /// Resolved model tags ([`Node::tags`] — the deduplicated top-level
    /// wire list).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The model's declared columns with their per-column context, in
    /// deterministic name order. Empty (key omitted) for models without
    /// a columns block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ManifestColumnPayload>,
    /// MODEL-LEVEL data tests (cute-dbt#200): ingested test nodes with
    /// `attached_node` = this model and `column_name` = `None`, mapped
    /// through the same §2.2 display vocabulary as column tests. A
    /// SECOND, model-scoped projection — the per-table
    /// `given/expected.column_meta` th-tooltips (#165/#166) are
    /// untouched.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub model_tests: Vec<ModelTestPayload>,
}

/// One declared column in a [`ManifestNodePayload`] — name, authored
/// description, declared `data_type`, and the column-scoped data tests
/// in the SHIPPED [`ColumnTestPayload`] display shape (cute-dbt#166/#189
/// — reused, never a parallel type).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ManifestColumnPayload {
    /// Column name as declared in the model's `columns` map.
    pub name: String,
    /// Authored column description (the #165 ingestion; empty-string
    /// unset shapes already dropped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Declared `data_type` from the already-ingested [`Node::columns`]
    /// map (`None` — key omitted — for untyped columns).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub column_type: Option<String>,
    /// Column-scoped data tests, the §2.2 display mapping (same entries
    /// the per-table `column_meta` carries for this column).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ColumnTestPayload>,
}

/// One MODEL-LEVEL data test in display shape (cute-dbt#200): the
/// [`column_test_payload`] §2.2 mapping reduced to `name` + `detail`
/// (known built-ins keep their prose names + detail; unknown tests carry
/// the package-qualified raw name with no detail — their open-ended arg
/// vocabularies stay uninterpreted, the v1 stance).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ModelTestPayload {
    /// Display name (the §2.2 vocabulary or the package-qualified raw
    /// test name).
    pub name: String,
    /// Muted mono detail when the test carries an interpretable one
    /// (`relationships` target / range bound). `None` — key omitted —
    /// otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Structured display mapping of a column-scoped generic test
/// (cute-dbt#165 → restructured for cute-dbt#178 per the handoff README
/// §2.2 table):
///
/// | dbt test | `name` | `values` / `detail` |
/// |---|---|---|
/// | `unique` | `unique` | — |
/// | `not_null` | `not null` | — |
/// | `accepted_values` | `accepted values` | `values` = the list (pills) |
/// | `relationships (to: ref('m'), field: f)` | `relationships` | `detail = "m.f"` |
/// | `accepted_range (min/max)` | `accepted range` | `detail = "0–100"` / `"≥ 0"` / `"≤ 1"` |
///
/// Any other test (incl. package tests like `dbt_expectations.*`)
/// carries its package-qualified raw name with no values/detail — their
/// arg vocabularies are open-ended and v1 does not interpret them.
/// `accepted_range` maps by bare name regardless of namespace (it
/// usually ships as `dbt_utils.accepted_range`).
#[must_use]
pub fn column_test_payload(tm: &TestMetadata) -> ColumnTestPayload {
    let qualified = match tm.namespace() {
        Some(ns) => format!("{ns}.{}", tm.name()),
        None => tm.name().to_owned(),
    };
    match tm.name() {
        "unique" => ColumnTestPayload {
            name: "unique".to_owned(),
            ..ColumnTestPayload::default()
        },
        "not_null" => ColumnTestPayload {
            name: "not null".to_owned(),
            ..ColumnTestPayload::default()
        },
        "accepted_values" => {
            let values = tm
                .kwargs()
                .get("values")
                .and_then(Value::as_array)
                .map(|values| values.iter().map(scalar_token).collect())
                .unwrap_or_default();
            ColumnTestPayload {
                name: "accepted values".to_owned(),
                values,
                detail: None,
            }
        }
        "relationships" => ColumnTestPayload {
            name: "relationships".to_owned(),
            values: Vec::new(),
            detail: relationships_detail(tm),
        },
        "accepted_range" => ColumnTestPayload {
            name: "accepted range".to_owned(),
            values: Vec::new(),
            detail: accepted_range_detail(tm),
        },
        _ => ColumnTestPayload {
            name: qualified,
            ..ColumnTestPayload::default()
        },
    }
}

/// `relationships` detail per README §2.2: `to: ref('m'), field: f` →
/// `"m.f"`. The `to` target unwraps a `ref('…')` / `source('…','…')`
/// jinja call to its last quoted name; a non-call target renders
/// verbatim. Field-less relationships show just the target.
fn relationships_detail(tm: &TestMetadata) -> Option<String> {
    let to = tm.kwargs().get("to").and_then(Value::as_str)?;
    let target = unquote_last_jinja_arg(to);
    match tm.kwargs().get("field").and_then(Value::as_str) {
        Some(field) => Some(format!("{target}.{field}")),
        None => Some(target),
    }
}

/// The last single-quoted argument of a jinja-ish call (`ref('m')` →
/// `m`; `source('raw', 'orders')` → `orders`), or the input verbatim
/// when no quoted argument is present.
fn unquote_last_jinja_arg(value: &str) -> String {
    let mut last: Option<&str> = None;
    let mut rest = value;
    while let Some(open) = rest.find('\'') {
        let tail = &rest[open + 1..];
        let Some(close) = tail.find('\'') else { break };
        last = Some(&tail[..close]);
        rest = &tail[close + 1..];
    }
    last.unwrap_or(value).to_owned()
}

/// `accepted_range` detail per README §2.2: both bounds → `"min–max"`
/// (en dash); min only → `"≥ min"`; max only → `"≤ max"`; neither →
/// `None`. Bounds render via [`scalar_token`] (authored JSON scalars).
fn accepted_range_detail(tm: &TestMetadata) -> Option<String> {
    let min = tm.kwargs().get("min_value").filter(|v| !v.is_null());
    let max = tm.kwargs().get("max_value").filter(|v| !v.is_null());
    match (min, max) {
        (Some(min), Some(max)) => Some(format!(
            "{}\u{2013}{}",
            scalar_token(min),
            scalar_token(max)
        )),
        (Some(min), None) => Some(format!("\u{2265} {}", scalar_token(min))),
        (None, Some(max)) => Some(format!("\u{2264} {}", scalar_token(max))),
        (None, None) => None,
    }
}

/// Display token for one `accepted_values` entry: a JSON string renders
/// bare (no quotes — the authored value, matching how dbt docs show the
/// list), any other scalar via its JSON rendering.
fn scalar_token(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Build the full column-metadata map for `model` (cute-dbt#165):
/// authored descriptions from the model node's `columns` map, plus the
/// **column-scoped** generic tests attached to it (manifest `test` nodes
/// with `attached_node == model` AND `column_name` set — dbt's
/// column-`tests:` shape). Model-level tests that merely take a column
/// argument carry `column_name: null` and are deliberately out of v1
/// scope. Only columns with a description and/or ≥1 test appear.
///
/// Deterministic: descriptions iterate a `BTreeMap`; tests are sorted by
/// (column, name, values, detail, test node id) before insertion —
/// `Manifest::nodes` is a `HashMap` with no inherent order.
#[must_use]
pub fn column_meta_for_model(
    current: &Manifest,
    model: &Node,
) -> BTreeMap<String, ColumnMetaPayload> {
    let mut meta: BTreeMap<String, ColumnMetaPayload> = BTreeMap::new();
    for (column, description) in model.column_descriptions() {
        meta.entry(column.clone()).or_default().description = Some(description.clone());
    }
    for (column, test) in attached_column_tests(current, model.id()) {
        meta.entry(column.to_owned()).or_default().tests.push(test);
    }
    meta
}

/// The column-scoped data tests attached to the node `attached` —
/// shared by the model/seed arm ([`column_meta_for_model`]) and the
/// source arm ([`column_meta_for_source`]) of the cute-dbt#165/#235
/// column-header tooltips. Deterministic: sorted by (column, name,
/// values, detail, test node id) — `Manifest::nodes` is a `HashMap`
/// with no inherent order.
fn attached_column_tests<'m>(
    current: &'m Manifest,
    attached: &NodeId,
) -> Vec<(&'m str, ColumnTestPayload)> {
    let mut tests: Vec<(&str, ColumnTestPayload, &str)> = current
        .nodes()
        .iter()
        .filter_map(|(id, node)| {
            if node.resource_type() != "test" || node.attached_node() != Some(attached) {
                return None;
            }
            let column = node.column_name()?;
            let tm = node.test_metadata()?;
            Some((column, column_test_payload(tm), id.as_str()))
        })
        .collect();
    tests.sort_by(|a, b| {
        (a.0, &a.1.name, &a.1.values, &a.1.detail, a.2).cmp(&(
            b.0,
            &b.1.name,
            &b.1.values,
            &b.1.detail,
            b.2,
        ))
    });
    tests
        .into_iter()
        .map(|(column, test, _)| (column, test))
        .collect()
}

/// A SOURCE's column-metadata map (cute-dbt#235) — the
/// [`column_meta_for_model`] twin for `source('a','b')` given inputs:
/// authored column descriptions from the ingested source `columns`
/// block, merged with column-scoped data tests whose `attached_node` is
/// the source. Only columns with a description and/or ≥1 test appear,
/// so a metadata-less source contributes nothing (honest degrade — the
/// JS renders no trigger, never an empty bubble).
fn column_meta_for_source(
    current: &Manifest,
    source: &SourceNode,
) -> BTreeMap<String, ColumnMetaPayload> {
    let mut meta: BTreeMap<String, ColumnMetaPayload> = BTreeMap::new();
    for (column, description) in source.column_descriptions() {
        meta.entry(column.clone()).or_default().description = Some(description.clone());
    }
    for (column, test) in attached_column_tests(current, source.id()) {
        meta.entry(column.to_owned()).or_default().tests.push(test);
    }
    meta
}

/// Resolve a unit-test GIVEN's `ref(...)` to its manifest node
/// (cute-dbt#235). Unlike [`resolve_target_model`] (a unit test's
/// `model:` target is always a model), a given's ref resolves over
/// dbt's full refable set — models, seeds, and snapshots (the committed
/// jaffle-shop fixture's `ref('raw_customers')` seed given is the real
/// wire shape; fusion validates given inputs as any ref/source/this,
/// `dbt-parser` `resolve_unit_tests.rs` @ `9977b6cb…`). Deterministic
/// under a leaf-name collision: the lexicographically smallest node id
/// wins (`model.*` sorts before `seed.*`/`snapshot.*`).
fn resolve_given_ref_node<'m>(current: &'m Manifest, ref_name: &str) -> Option<&'m Node> {
    const REFABLE: [&str; 3] = ["model", "seed", "snapshot"];
    current
        .nodes()
        .values()
        .filter(|node| {
            REFABLE.contains(&node.resource_type()) && leaf_segment(node.id().as_str()) == ref_name
        })
        .min_by(|a, b| a.id().cmp(b.id()))
}

/// Filter a model's full column-metadata map down to the columns that
/// actually appear in one rendered fixture table. `None` table (sql /
/// opaque / unloaded-external fixture — no grid, no headers) ⇒ empty map
/// (the `skip_serializing_if` then omits the key entirely).
fn column_meta_for_table(
    meta: &BTreeMap<String, ColumnMetaPayload>,
    table: Option<&FixtureTable>,
) -> BTreeMap<String, ColumnMetaPayload> {
    let Some(table) = table else {
        return BTreeMap::new();
    };
    table
        .columns
        .iter()
        .filter_map(|column| meta.get(column).map(|m| (column.clone(), m.clone())))
        .collect()
}

/// Build one model's [`ManifestNodePayload`] (cute-dbt#200): the
/// authored description + tags (#200 ingestion), the #145 `materialized`
/// accessor, every DECLARED column (the [`Node::columns`] map — name
/// order, deterministic) decorated with its #165 description and
/// column-scoped tests, and the model-level test grouping. Mostly
/// re-plumbing of already-ingested data — the genuinely new computation
/// is the model-level grouping in [`model_tests_for_model`].
fn manifest_node_payload(current: &Manifest, model: &Node) -> ManifestNodePayload {
    let meta = column_meta_for_model(current, model);
    // Declared columns drive the list; a meta-only key (a column-scoped
    // test whose column the model does not declare — possible only on
    // hand-built manifests) is appended via the BTreeMap union so no
    // ingested test silently disappears.
    let mut columns: BTreeMap<&String, ManifestColumnPayload> = model
        .columns()
        .iter()
        .map(|(name, data_type)| {
            (
                name,
                ManifestColumnPayload {
                    name: name.clone(),
                    description: None,
                    column_type: data_type.clone(),
                    tests: Vec::new(),
                },
            )
        })
        .collect();
    for (name, m) in &meta {
        let entry = columns
            .entry(name)
            .or_insert_with(|| ManifestColumnPayload {
                name: name.clone(),
                ..ManifestColumnPayload::default()
            });
        entry.description.clone_from(&m.description);
        entry.tests.clone_from(&m.tests);
    }
    ManifestNodePayload {
        description: model.description().map(str::to_owned),
        materialized: model.config().materialized().map(str::to_owned),
        tags: model.tags().to_vec(),
        columns: columns.into_values().collect(),
        model_tests: model_tests_for_model(current, model),
    }
}

/// The MODEL-LEVEL data tests attached to `model` (cute-dbt#200):
/// ingested generic-test nodes with `attached_node == model` AND
/// `column_name == None` (dbt's model-`data_tests:` shape — a
/// column-scoped test carries `column_name` and belongs to the per-column
/// projection instead). Singular (SQL-file) tests carry no
/// `test_metadata` — and on real manifests no `attached_node` either —
/// so they are out of v1 scope, exactly like the #165 column path.
/// Mapped through [`column_test_payload`] and reduced to `name` +
/// `detail` ([`ModelTestPayload`]); sorted by (name, detail, test node
/// id) — `Manifest::nodes` is a `HashMap` with no inherent order.
fn model_tests_for_model(current: &Manifest, model: &Node) -> Vec<ModelTestPayload> {
    let mut tests: Vec<(ModelTestPayload, &str)> = current
        .nodes()
        .iter()
        .filter_map(|(id, node)| {
            if node.resource_type() != "test"
                || node.attached_node() != Some(model.id())
                || node.column_name().is_some()
            {
                return None;
            }
            let tm = node.test_metadata()?;
            let mapped = column_test_payload(tm);
            Some((
                ModelTestPayload {
                    name: mapped.name,
                    detail: mapped.detail,
                },
                id.as_str(),
            ))
        })
        .collect();
    tests.sort_by(|a, b| (&a.0.name, &a.0.detail, a.1).cmp(&(&b.0.name, &b.0.detail, b.1)));
    tests.into_iter().map(|(t, _)| t).collect()
}

/// Build the report-level `manifest_nodes` lookup (cute-dbt#200), keyed
/// by BARE model name. Scope is deliberately narrow: the in-scope models
/// plus every model referenced by a rendered test's `given.input`
/// `ref()` — never the whole project graph. A `this` given resolves to
/// the in-scope target model (already present); `source(...)` inputs and
/// unresolvable refs contribute nothing (manifest `sources` are not
/// model nodes — the pill renders without a hover card, the graceful JS
/// contract). All-empty entries are skipped so bare synthetic manifests
/// keep the `manifest_nodes` key off the wire entirely.
fn build_manifest_nodes(
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
    model_tests: &HashMap<NodeId, Vec<(&str, &UnitTest)>>,
) -> BTreeMap<String, ManifestNodePayload> {
    let mut referenced: BTreeMap<String, &Node> = BTreeMap::new();
    for model_id in models_in_scope.iter() {
        let Some(model) = current.node(model_id) else {
            continue;
        };
        referenced.insert(leaf_segment(model.id().as_str()).to_owned(), model);
        for (_, unit_test) in model_tests.get(model_id).into_iter().flatten() {
            for given in unit_test.given() {
                let Some(upstream) = parse_ref_name(given.input())
                    .and_then(|ref_name| resolve_target_model(current, &NodeId::new(ref_name)))
                else {
                    continue;
                };
                referenced.insert(leaf_segment(upstream.id().as_str()).to_owned(), upstream);
            }
        }
    }
    referenced
        .into_iter()
        .filter_map(|(name, node)| {
            let payload = manifest_node_payload(current, node);
            (payload != ManifestNodePayload::default()).then_some((name, payload))
        })
        .collect()
}

/// One `given[i]` entry from a unit test, lifted into payload shape.
///
/// `bound_to_node` ties the given to an import-CTE node id when a match
/// was found (the node-detail panel's "given · ref('…')" table). When
/// `None`, the design surfaces the "no fixture provided" empty-state
/// copy on the bound node.
#[derive(Debug, Clone, Serialize)]
pub struct GivenPayload {
    /// Verbatim `input:` value from the unit-test `given[i]` entry
    /// (typically `ref('…')` or `source('…','…')`).
    pub input: String,
    /// Import-CTE node id this given binds to, when the engine
    /// successfully matched [`Self::input`] to a node in the model's
    /// CTE graph. `None` triggers the "no fixture provided" empty-state
    /// copy on the bound node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_to_node: Option<String>,
    /// The Rust-computed Current-view [`FixtureTable`] — the authoritative
    /// tabulated cells (authored `display` + canonical `key` per cell),
    /// computed in the domain via `table_from_manifest_rows` (cute-dbt#138).
    /// `None` for a non-tabulatable fixture (sql/opaque, or external-`fixture:`
    /// rows not in the manifest); the JS then renders the sql code block or the
    /// external-fixture affordance from [`rows`](Self::rows) /
    /// [`format`](Self::format) / [`fixture`](Self::fixture). When `Some`, the
    /// template is a PURE renderer of this POD — it no longer parses
    /// csv/dict in JS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<FixtureTable>,
    /// Tabular fixture rows lifted verbatim from the unit-test
    /// `given[i].rows` field (kept as serde `Value`). Retained for the
    /// non-tabulatable fallback only: the sql code block, and external-fixture
    /// detection (`rows == null`). When [`table`](Self::table) is `Some` the
    /// renderer ignores this field (cute-dbt#138). `Value::Null` when this
    /// given's data lives in an external [`fixture`](Self::fixture) file.
    pub rows: Value,
    /// Fixture format hint (`csv`, `yaml`, `dict`, etc.) when the
    /// manifest specified one; absent for inline-rows fixtures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Name of the external fixture file this given's rows live in (dbt's
    /// `fixture:` key), when set. `None` (key omitted) for inline-rows
    /// givens. When present **with `rows == Value::Null`**, the data is not
    /// in the manifest at all: the JS surfaces a "data in external fixture
    /// file: `<name>`" affordance and falls back to the cute-dbt#96 YAML
    /// text view instead of rendering a silently-empty grid (cute-dbt#126).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture: Option<String>,
    /// `true` when this given's `input` is the literal `this` — dbt's
    /// convention for mocking the model's own prior state on an incremental
    /// model (cute-dbt#145; fusion's own `input.as_str().eq("this")`
    /// discriminator). The template marks the given "prior model state".
    /// Serialized only when `true` (`skip_serializing_if =
    /// std::ops::Not::not`); a normal `ref(...)` / `source(...)` given omits
    /// the key.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_this: bool,
    /// Column-header metadata for this given's table (cute-dbt#165),
    /// keyed by column name — resolved against the model that OWNS the
    /// given's columns: the `ref(...)` input model (`this` resolves to
    /// the target model itself; `source(...)` inputs resolve to nothing —
    /// manifest `sources` are not ingested in v0.x). Only columns present
    /// in [`table`](Self::table) with a description and/or ≥1 column test
    /// appear; empty ⇒ the key is omitted and the template renders no
    /// affordance.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub column_meta: BTreeMap<String, ColumnMetaPayload>,
}

/// One unit test in the per-model payload.
#[derive(Debug, Clone, Serialize)]
pub struct TestPayload {
    /// Manifest unit-test id (`unit_test.<package>.<model>.<name>`) — the
    /// test selector's stable handle.
    pub id: String,
    /// User-facing test name (`UnitTest::name()`).
    pub name: String,
    /// `model:` reference verbatim from the manifest.
    pub target_model: String,
    /// `true` when this PR/diff **updated** this test (added or edited its
    /// definition); `false` when it is *context* — rendered only because
    /// its target model is in scope (cute-dbt#91). Always serialized (the
    /// report's JS foregrounds updated tests and toggles context on). The
    /// classifier rides on the existing in-scope selection — selection is
    /// unchanged; this is an additive label (ADR-3 / ADR-5).
    pub changed: bool,
    /// Optional human-readable test description from the manifest's
    /// `description:` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional `config.tags` list from the manifest (e.g. `["smoke",
    /// "nightly"]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Optional `config.meta` blob from the manifest (kept as serde
    /// `Value` to accept any JSON shape teams put under `meta`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// Optional `original_file_path` from the manifest — the on-disk
    /// location the test was defined in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defined_in: Option<String>,
    /// Raw YAML slice for this `unit_test` as authored — populated by
    /// the `gather_authoring_yaml` run-loop step (cute-dbt#69) when
    /// the project root is resolvable and the source file is readable.
    /// `None` when no project root is configured, the source file is
    /// missing, or the test entry cannot be located inside the file
    /// (defensive: a manifest can carry a `name` the source no longer
    /// contains).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_yaml: Option<String>,
    /// Inline diff of this test's authored YAML block (cute-dbt#96
    /// concern 2) — present only when the diff edited this test's own
    /// block (PR-diff mode, block present + aligned + touched). `None`
    /// (key omitted) for context tests, baseline mode, and edits that
    /// fall outside the block, so the drawer falls back to the plain
    /// authored-YAML view. Mirrors `authoring_yaml`'s threading exactly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaml_diff: Option<BlockDiff>,
    /// Cell-level data-table diff of this test's `given`/`expect` fixture
    /// rows (cute-dbt#98) — present only when the PR diff edited this
    /// test's own YAML block AND at least one fixture table carried a real
    /// cell change (PR-diff mode, block present + aligned + touched,
    /// non-opaque rows, `has_real_change()`). `None` (key omitted) for
    /// context tests, baseline mode, sql/opaque fixtures, and
    /// format-only / pure-reorder edits, so the given/expect grids default
    /// to the plain "Current" data view. Mirrors `yaml_diff`'s threading
    /// exactly; the JS defaults the per-table Current↔Diff toggle to Diff
    /// when this is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_diff: Option<UnitTestDataDiff>,
    /// dbt incremental-mode flag, lifted from
    /// `overrides.macros.is_incremental` (cute-dbt#145). `Some(true)` ⇒ the
    /// test exercises the incremental branch — the template shows a
    /// "incremental branch" mode badge AND the expect-semantics tooltip
    /// (`expect` is the rows merged/inserted, not the final table);
    /// `Some(false)` ⇒ explicit full-refresh branch (mode badge, NO
    /// tooltip — there `expect` IS the final table); `None` (key omitted)
    /// ⇒ no override. The mode badge renders only when the enclosing model
    /// is incremental ([`ModelPayload::is_incremental`]); the tooltip rides
    /// the authoritative bool (`=== true`), never the `this`-given proxy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_incremental_mode: Option<bool>,
    /// The FULL dbt `overrides` blob (cute-dbt#200): group (`"macros"` /
    /// `"vars"` / `"env_vars"`) → name → **native** value (serde `Value`
    /// passthrough — `true` / `7` / `0.05` stay bool/number on the wire,
    /// the cute-dbt#197 founder decision; never stringified). Drives the
    /// `overrides · N` badge + hover tooltip (cute-dbt#202; handoff
    /// README §2.6). `None` (key omitted — pre-#200 payloads stay
    /// byte-stable) when the test declares no effective override; the
    /// adapter already dropped null/empty groups. ADDITIVE context next
    /// to the lifted [`Self::is_incremental_mode`] flag (#145) and the
    /// #125 YAML-slice text diffs — both stay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<UnitTestOverrides>,
    /// Ordered list of fixture inputs for the test (`given[…]`).
    pub given: Vec<GivenPayload>,
    /// Expected result block (`expect`).
    pub expected: ExpectedPayload,
}

/// `expect` block lifted into payload shape.
#[derive(Debug, Clone, Serialize)]
pub struct ExpectedPayload {
    /// The Rust-computed Current-view [`FixtureTable`] for the `expect`
    /// fixture (authored `display` + canonical `key` per cell, cute-dbt#138).
    /// `None` for a non-tabulatable fixture (sql/opaque or external);
    /// otherwise the template renders this POD directly. Same contract as
    /// [`GivenPayload::table`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<FixtureTable>,
    /// Expected tabular rows lifted verbatim from the unit-test
    /// `expect.rows` field (serde `Value`). Retained for the non-tabulatable
    /// fallback only (sql code block; external-fixture detection). When
    /// [`table`](Self::table) is `Some` the renderer ignores this field
    /// (cute-dbt#138). `Value::Null` when the data lives in an external
    /// [`fixture`](Self::fixture) file.
    pub rows: Value,
    /// Expected-block format hint (`csv`, `yaml`, `dict`, etc.) when
    /// the manifest specified one; absent for inline-rows fixtures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Name of the external fixture file the `expect` rows live in (dbt's
    /// `fixture:` key), when set. `None` (key omitted) for inline-rows
    /// expects. Same external-fixture affordance contract as
    /// [`GivenPayload::fixture`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture: Option<String>,
    /// Column-header metadata for the expect table (cute-dbt#165), keyed
    /// by column name — resolved against the TARGET model (the expect
    /// table's columns are the model's output columns). Same emission
    /// contract as [`GivenPayload::column_meta`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub column_meta: BTreeMap<String, ColumnMetaPayload>,
}

/// The full JSON blob the askama template emits into the
/// `<script type="application/json" id="cute-dbt-data">` element.
#[derive(Debug, Clone, Serialize)]
pub struct ReportPayload {
    /// Human-readable baseline reference (filename or supplied label).
    pub baseline: String,
    /// One entry per model in `models_in_scope` (deterministic
    /// `BTreeSet` ordering inherited from the comparator).
    pub models: Vec<ModelPayload>,
    /// Report-level model-context lookup (cute-dbt#200), keyed by BARE
    /// model name: the in-scope models plus every model referenced by a
    /// rendered test's `given.input` `ref()` — NEVER the whole project
    /// graph. `source(...)` inputs and unresolvable refs contribute
    /// nothing; an absent entry is the graceful no-hover (the JS
    /// contract). Omitted from JSON when empty (all-empty entries are
    /// skipped too) so bare synthetic payloads stay byte-stable.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub manifest_nodes: BTreeMap<String, ManifestNodePayload>,
    /// Spec catalog for every check that appears in any model's
    /// `findings` (cute-dbt#170), keyed by dotted check id — the
    /// rationale drawer, tier vocabulary, and book link render from
    /// this. Omitted from JSON when no finding fired anywhere, so
    /// findings-free payloads (and the jaffle-shop golden) stay
    /// byte-stable.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub check_specs: BTreeMap<String, CheckSpecPayload>,
    /// The parsed working-tree dbt_project.yml (cute-dbt#266) —
    /// **standing metadata**, present on both scope arms whenever the
    /// file is readable + parseable under the resolved project root.
    /// Future consumers (explorer panes, provenance chips) read it from
    /// here; nothing in the current report chrome renders it directly.
    /// Omitted when absent so pre-#266 payloads stay byte-stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_definition: Option<ProjectDefinition>,
    /// The "Project definition changed" panel content (cute-dbt#266) —
    /// present exactly when dbt_project.yml is in the PR diff. The
    /// panel itself is server-rendered (the template's `project_panel`
    /// view); the payload carries the structured facts for downstream
    /// consumers + the BDD payload assertions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_change_panel: Option<ProjectChangePanel>,
}

/// Which scope source produced this report — selects the diff-scope
/// banner's provenance clause.
///
/// [`ScopeSource::Baseline`] renders "vs baseline manifest `<label>`";
/// [`ScopeSource::PrDiff`] renders "from PR file diff" (there is no
/// baseline manifest to name on the PR-diff path — naming one would be a
/// false statement, cute-dbt#85).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeSource {
    /// Scoped via `--baseline-manifest` (dbt `state:modified`).
    Baseline,
    /// Scoped via `--pr-diff` (a PR's `git diff --unified=0`).
    PrDiff,
}

// ---------------------------------------------------------------------
// Project-definition panel view (cute-dbt#266)
// ---------------------------------------------------------------------

/// One server-rendered row of the project-definition panel.
struct ProjectPanelRowView {
    /// Snake-case category key — the row's CSS hook + chip class
    /// (`vars`, `config_tree`, `dispatch`, `hooks`, `paths`,
    /// `identity`, `other`).
    category_key: &'static str,
    /// Human category chip text.
    category_label: &'static str,
    /// The changed key's display path (the domain change's `label`).
    label: String,
    /// `old → new` / `added: v` / `removed: v`, values as compact JSON
    /// (type-faithful: `"1"` and `1` read differently).
    detail: String,
    /// Honesty note rendered inside the row (empty ⇒ none). Vars rows
    /// state plainly "blast radius not attributed" (attribution is a
    /// later slice — the copy never implies coming-soon); dispatch rows
    /// state the project-wide/not-attributable fact.
    note: &'static str,
}

/// One raw diff line of the panel's Shape-A fallback row.
struct ProjectPanelLineView {
    /// `context` / `removed` / `added` — the CSS hook.
    kind: &'static str,
    /// The line text, sigil-free.
    text: String,
}

/// The server-rendered "Project definition changed" panel — built from
/// the domain [`ProjectChangePanel`] exactly when dbt_project.yml is in
/// the PR diff. Wording lives here (adapter); facts live in the domain
/// POD (the `ModelYamlOutcome` precedent).
struct ProjectPanelView {
    /// Categorized rows (empty in fallback mode and for a
    /// formatting-only edit).
    rows: Vec<ProjectPanelRowView>,
    /// `true` when categorization succeeded but found zero semantic
    /// changes — the panel says so instead of rendering nothing.
    is_empty_change: bool,
    /// Non-empty exactly in fallback mode: the explicit degrade copy
    /// ("could not categorize" / "could not reconstruct the previous
    /// version" / the absence note).
    fallback_copy: String,
    /// The fallback's raw diff lines (also carried on the absence-note
    /// arm — the hunks are known even when the file is not).
    fallback_lines: Vec<ProjectPanelLineView>,
}

/// Category key + chip label + per-row honesty note for one
/// [`ProjectChangeCategory`].
fn project_category_strings(
    category: ProjectChangeCategory,
) -> (&'static str, &'static str, &'static str) {
    match category {
        ProjectChangeCategory::Vars => (
            "vars",
            "vars",
            // Locked interim copy (shaping #262 v3): plain statement,
            // never "coming soon".
            "blast radius not attributed",
        ),
        ProjectChangeCategory::ConfigTree => ("config_tree", "config tree", ""),
        ProjectChangeCategory::Dispatch => (
            "dispatch",
            "dispatch",
            "macro search order changed — project-wide effect, not statically attributable",
        ),
        ProjectChangeCategory::Hooks => ("hooks", "hooks", ""),
        ProjectChangeCategory::Paths => ("paths", "paths", ""),
        ProjectChangeCategory::Identity => ("identity", "identity", ""),
        ProjectChangeCategory::Other => ("other", "other", ""),
    }
}

/// A change's `detail` string: both sides present → `old → new`;
/// one-sided → `added:` / `removed:`. Values render as compact JSON.
fn project_change_detail(change: &ProjectChange) -> String {
    let compact = |v: &Value| serde_json::to_string(v).unwrap_or_else(|_| "null".to_owned());
    match (&change.old, &change.new) {
        (Some(old), Some(new)) => format!("{} \u{2192} {}", compact(old), compact(new)),
        (None, Some(new)) => format!("added: {}", compact(new)),
        (Some(old), None) => format!("removed: {}", compact(old)),
        (None, None) => String::new(), // unreachable by construction
    }
}

/// The fallback arm's explicit copy — "could not categorize" /
/// "could not reconstruct the previous version" / the absence note.
fn project_fallback_copy(reason: ProjectFallbackReason) -> &'static str {
    match reason {
        ProjectFallbackReason::NewParseFailed => {
            "dbt_project.yml could not be parsed, so this change could not be \
             categorized — showing the raw diff."
        }
        ProjectFallbackReason::OldParseFailed => {
            "The previous version of dbt_project.yml could not be parsed, so this \
             change could not be categorized — showing the raw diff."
        }
        ProjectFallbackReason::OldNotReconstructable => {
            "Could not reconstruct the previous version of dbt_project.yml from \
             the diff, so this change could not be categorized — showing the raw diff."
        }
        ProjectFallbackReason::FileUnreadable => {
            "dbt_project.yml changed in this diff, but the file could not be read \
             from the project root — nothing to categorize. Showing the raw diff."
        }
    }
}

/// Build the server-rendered panel view from the domain panel POD.
fn project_panel_view(panel: &ProjectChangePanel) -> ProjectPanelView {
    match panel {
        ProjectChangePanel::Categorized { changes } => {
            let rows = changes
                .iter()
                .map(|change| {
                    let (category_key, category_label, note) =
                        project_category_strings(change.category);
                    ProjectPanelRowView {
                        category_key,
                        category_label,
                        label: change.label.clone(),
                        detail: project_change_detail(change),
                        note,
                    }
                })
                .collect::<Vec<_>>();
            ProjectPanelView {
                is_empty_change: rows.is_empty(),
                rows,
                fallback_copy: String::new(),
                fallback_lines: Vec::new(),
            }
        }
        ProjectChangePanel::Fallback { reason, raw } => ProjectPanelView {
            rows: Vec::new(),
            is_empty_change: false,
            fallback_copy: project_fallback_copy(*reason).to_owned(),
            fallback_lines: raw
                .iter()
                .map(|line: &DiffLine| ProjectPanelLineView {
                    kind: match line.kind {
                        DiffLineKind::Context => "context",
                        DiffLineKind::Removed => "removed",
                        DiffLineKind::Added => "added",
                    },
                    text: line.text.clone(),
                })
                .collect(),
        },
    }
}

/// askama template binding for the v0.1 report.
///
/// Asset values are pinned `&'static str` constants from
/// [`asset_embed`](crate::adapters::asset_embed) so the renderer cannot
/// drift from the vendored bundle. `payload_json` is pre-escaped by
/// [`payload_json_for_html_script`] so its `|safe` interpolation cannot
/// terminate the `<script>` carrier.
#[derive(Template)]
#[template(path = "report.html", escape = "html")]
struct ReportTemplate<'a> {
    sakura_css: &'a str,
    datatables_css: &'a str,
    /// First-party chassis CSS (cute-dbt#177) — the semantic-token +
    /// theme + style-pack + density layer that fills the template's
    /// custom `<style>` block. [`REPORT_CSS`], not a vendored asset.
    report_css: &'a str,
    jquery_js: &'a str,
    datatables_js: &'a str,
    mermaid_js: &'a str,
    /// Vendored Cytoscape UMD bundle (cute-dbt#180) — the second graph
    /// engine behind the settings-panel Mermaid ⇄ Cytoscape picker
    /// (Mermaid stays the static default). [`CYTOSCAPE_JS`].
    cytoscape_js: &'a str,
    /// First-party report interaction engine (cute-dbt#178) — the
    /// model/test selectors, DAG, unified + split diff renderers, fixture
    /// grids and settings wiring that fill the template's engine
    /// `<script>` block. [`INTERACTION_JS`], not a vendored asset.
    interaction_js: &'a str,
    /// SHARED appearance engine (cute-dbt#242) — reads + applies the
    /// persisted `cute-dbt.appearance.v1` appearance on `<html>`;
    /// identical bytes on every page family. [`APPEARANCE_JS`], not a
    /// vendored asset.
    appearance_js: &'a str,
    /// Report-only appearance settings UI (cute-dbt#178, re-layered at
    /// cute-dbt#242) — the settings panel's controls + `DataTables`
    /// reflow + DAG-engine dispatch over the shared engine.
    /// [`THEME_JS`], not a vendored asset.
    theme_js: &'a str,
    /// First-party Cytoscape DAG engine (cute-dbt#180) — preset layout,
    /// canvas-text labels, hover card, click-to-highlight lineage.
    /// [`CYTO_DAG_JS`], not a vendored asset.
    cyto_dag_js: &'a str,
    favicon_data_uri: &'a str,
    /// Report title — substituted into both `<title>` (head) and
    /// `<h1>` (header). Resolved by the cli layer from
    /// `cli.config.report.title`, falling back to
    /// [`crate::domain::DEFAULT_REPORT_TITLE`] when no config is supplied.
    report_title: &'a str,
    /// Optional report subtitle (PR 14 / cute-dbt#24). When
    /// `Some(...)`, the template renders a new
    /// `<p class="report-subtitle">` element immediately after the
    /// `<h1>`. When `None`, the element is omitted entirely (no empty
    /// DOM node).
    report_subtitle: Option<&'a str>,
    /// Server-rendered banner text — a single contiguous string the
    /// `report_generation.feature` and `tests/run_loop.rs` assertions
    /// can grep against without the static HTML's span boundaries
    /// breaking the substring. JS may rewrite the `.diff-scope-text`
    /// element at boot, but the static fallback is the contract.
    banner_text: &'a str,
    /// Human-readable baseline reference (the `--baseline-manifest`
    /// path verbatim in v0.1) — rendered as plain text inside the
    /// diff-scope banner's `.diff-scope-baseline` element. Empty on the
    /// PR-diff path (the banner omits the baseline clause entirely).
    baseline_label: &'a str,
    /// `true` when the report was scoped from a PR file diff. Selects the
    /// banner's provenance clause: PR-diff → "from PR file diff";
    /// baseline → "vs baseline manifest `<label>`" (cute-dbt#85).
    is_pr_diff: bool,
    /// JSON payload, pre-escaped for safe interpolation inside
    /// `<script type="application/json">` via [`payload_json_for_html_script`].
    /// The template emits this with `|safe`; the safety property is the
    /// Rust-side escape, not askama's HTML filter.
    payload_json: &'a str,
    /// The server-rendered "Project definition changed" panel
    /// (cute-dbt#266) — `Some` exactly when dbt_project.yml is in the
    /// PR diff; `None` keeps the section out of the DOM entirely.
    project_panel: Option<ProjectPanelView>,
}

/// Serialize `payload` to JSON for safe embedding inside an HTML
/// `<script type="application/json">` block.
///
/// HTML5's script-data state terminates on `</` followed by an ASCII
/// alpha character; the script-data-double-escape state begins on
/// `<!--`. A naive `serde_json` serialization of a payload containing
/// `</script>` or `<!--` would allow a manifest-derived string to
/// break out of the JSON carrier.
///
/// Escape `<` to its `<` Unicode form whenever it is followed by
/// `/`, `!`, `?`, `=`, or an ASCII letter — every tag-opening shape
/// plus the scanner-hostile `<=`. `</` and `<!--` are the sequences
/// that matter under HTML5's script-data state machine; the `<letter` /
/// `<?` forms are inert in a real browser but read as markup to
/// non-HTML5 tag scanners (cute-dbt#170: the check-spec catalog put
/// prose like `WHERE <right>.<key> IS NULL` on the wire, which the
/// `tl`-based test extractors parsed as a tag, corrupting payload
/// extraction). `<=` joined the set at cute-dbt#200: authored model /
/// column descriptions carry SQL-ish prose like `encounter_start_at <=
/// current_timestamp` (the committed playground fixture), which `tl`
/// also mis-scans. A bare `<` followed by a space or digit
/// (compiled-SQL comparisons) stays raw. The `\uXXXX` form is a
/// documented JSON escape (RFC 8259 §7) so the output remains a valid
/// JSON document that `JSON.parse(...)` decodes back to the original
/// characters.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] if serialization
/// fails — unreachable in practice for the payload shapes this module
/// emits (all fields are concrete `Serialize` types known at compile
/// time).
fn payload_json_for_html_script(payload: &ReportPayload) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(payload)?;
    let mut out = String::with_capacity(json.len() + 16);
    let mut chars = json.chars().peekable();
    while let Some(c) = chars.next() {
        let tag_opener = matches!(
            chars.peek(),
            Some('/' | '!' | '?' | '=' | 'a'..='z' | 'A'..='Z')
        );
        if c == '<' && tag_opener {
            out.push_str("\\u003c");
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// Compose the banner text rendered into the diff-scope section.
///
/// Empty `models_in_scope` → exactly `BANNER_EMPTY_SCOPE` (the locked
/// `"0 unit tests in scope"` contract string from
/// [`crate::domain::state`]). Non-empty → `"Showing N unit test(s) in scope"`
/// — the same wording the design's JS produces at boot.
#[must_use]
pub fn compose_banner_text(in_scope: &InScopeSet) -> String {
    if in_scope.is_empty() {
        BANNER_EMPTY_SCOPE.to_owned()
    } else {
        let n = in_scope.len();
        let noun = if n == 1 { "unit test" } else { "unit tests" };
        format!("Showing {n} {noun} in scope")
    }
}

/// Build the full report payload from the run loop's outputs.
///
/// `baseline_label` is interpolated verbatim into the diff-scope banner
/// (e.g. the `--baseline-manifest` path string). The payload's
/// `models` list mirrors `models_in_scope` order (deterministic
/// `BTreeSet` traversal). Each in-scope model carries **all** the unit
/// tests targeting it (via [`index_tests_for_models`]), not just the
/// in-scope ones — so the report's All-tests mode and per-model total
/// count work (cute-dbt#91 widening). Each test is tagged `changed`
/// (updated) when its id is in the `changed` set, else context. A model
/// not present in `current.nodes()` is silently skipped — the comparator
/// should not have surfaced it, but belt-and-braces.
///
/// `changed` sits in position 2 (where the dropped in-scope set used to
/// be) so the widening leaves existing call sites textually stable; the
/// in-scope set is no longer consumed here (the banner reads it in
/// [`render_report`]).
#[must_use]
// `authoring_yaml` is always built by the cli layer with the default
// hasher; clippy::implicit_hasher would have us generalize over
// BuildHasher for no real-world benefit.
#[allow(clippy::implicit_hasher)]
pub fn build_payload(
    current: &Manifest,
    changed: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    baseline_label: &str,
) -> ReportPayload {
    build_payload_with_externals(
        current,
        changed,
        models_in_scope,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        model_yaml,
        data_diffs,
        &HashMap::new(),
        baseline_label,
        &CheckPolicy::default(),
        &ProjectFacts::default(),
    )
}

/// Like [`build_payload`] but inlines any external fixture files read for
/// each in-scope test (cute-dbt#126). `external_fixtures` is keyed by test
/// id; an absent entry (or an absent given ordinal within it) leaves that
/// given/expect on its inline-manifest path. The cli's run loop builds the
/// map from the `ProjectFileReader`; baseline mode + every render path with
/// no external `fixture:` files use the [`build_payload`] convenience.
///
/// `check_policy` (cute-dbt#171) is the resolved display policy applied to
/// each model's findings AFTER supersedes resolution (the cli builds it
/// from `--config` `[checks]` + scanned SQL pragmas; the [`build_payload`]
/// convenience passes `CheckPolicy::default()` — everything displayed,
/// nothing suppressed).
///
/// `project_facts` (cute-dbt#266) carries the parsed dbt_project.yml
/// (standing metadata) + the diff-gated panel content; the
/// [`build_payload`] convenience passes `ProjectFacts::default()` (both
/// absent — the pre-#266 payload shape, byte-identical).
#[must_use]
// `too_many_arguments`: see `render_report` — the composition root threads
// the run loop's already-built artifacts straight through.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn build_payload_with_externals(
    current: &Manifest,
    changed: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    baseline_label: &str,
    check_policy: &CheckPolicy<HeuristicId>,
    project_facts: &ProjectFacts,
) -> ReportPayload {
    let model_tests = index_tests_for_models(current, models_in_scope);
    let empty: Vec<(&str, &UnitTest)> = Vec::new();
    let mut models = Vec::new();
    for model_id in models_in_scope.iter() {
        let Some(model) = current.node(model_id) else {
            continue;
        };
        let tests = model_tests.get(model_id).unwrap_or(&empty).as_slice();
        models.push(build_model_payload(
            current,
            model,
            tests,
            changed,
            authoring_yaml,
            yaml_diffs,
            sql_diffs,
            model_yaml,
            data_diffs,
            external_fixtures,
            check_policy,
        ));
    }
    // cute-dbt#170 — the spec catalog covers exactly the checks that
    // fired (suppressed findings included: they render, quietly).
    let mut check_specs = BTreeMap::new();
    for model in &models {
        for finding in &model.findings {
            let id = finding.finding.check;
            check_specs
                .entry(id.as_str().to_owned())
                .or_insert_with(|| check_spec_payload(id));
        }
    }
    ReportPayload {
        baseline: baseline_label.to_owned(),
        models,
        // cute-dbt#200 — the model-context lookup for the shelf/hover
        // cards, scoped to the in-scope models + their tests' ref()-ed
        // upstreams.
        manifest_nodes: build_manifest_nodes(current, models_in_scope, &model_tests),
        check_specs,
        // cute-dbt#266 — standing metadata + the diff-gated panel facts.
        project_definition: project_facts.definition.clone(),
        project_change_panel: project_facts.panel.clone(),
    }
}

/// Render the report payload + asset bundle to `out`.
///
/// Returns the underlying [`io::Error`] when the output path cannot be
/// written. Template rendering itself is infallible at runtime — askama
/// generates compile-time-checked code — so a render failure surfaces
/// as a build failure, not a runtime branch.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when the rendered HTML cannot be
/// written to `out`. A template-rendering failure would surface as an
/// `io::Error` with [`io::ErrorKind::Other`] carrying the askama error
/// string; in practice the template is statically checked so this
/// branch is unreachable in v0.1 — the explicit mapping exists to keep
/// the run-loop signature monomorphic on `io::Error`.
// See `build_payload` for the rationale on the implicit-hasher allow.
// `too_many_arguments`: the render composition root threads the run-loop's
// already-built artifacts (manifest, scope sets, authoring YAML, inline
// diffs, banner) straight through; a params struct would add indirection
// that buys nothing at this single call site.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn render_report(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
) -> io::Result<()> {
    render_report_with_externals(
        out,
        current,
        in_scope,
        models_in_scope,
        changed,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        model_yaml,
        data_diffs,
        &HashMap::new(),
        baseline_label,
        scope_source,
        report_title,
        report_subtitle,
        &CheckPolicy::default(),
        &ProjectFacts::default(),
    )
}

/// Like [`render_report`] but inlines any external fixture files read for
/// the in-scope tests (cute-dbt#126). The cli's run loop calls this with
/// the `gather_external_fixtures` map; [`render_report`] is the
/// no-external-fixtures convenience used by baseline mode + the tests.
///
/// # Errors
///
/// Same as [`render_report`]: the underlying [`io::Error`] when the
/// rendered HTML cannot be written to `out`.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn render_report_with_externals(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
    check_policy: &CheckPolicy<HeuristicId>,
    project_facts: &ProjectFacts,
) -> io::Result<()> {
    let payload = build_payload_with_externals(
        current,
        changed,
        models_in_scope,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        model_yaml,
        data_diffs,
        external_fixtures,
        baseline_label,
        check_policy,
        project_facts,
    );
    // The empty-scope banner contract reads the TRUE in-scope set, not the
    // widened render set or the changed subset (cute-dbt#91).
    let banner_text = compose_banner_text(in_scope);
    let payload_json = payload_json_for_html_script(&payload)
        .map_err(|err| io::Error::other(format!("payload serialization: {err}")))?;
    let template = ReportTemplate {
        sakura_css: SAKURA_CSS,
        datatables_css: DATATABLES_CSS,
        report_css: REPORT_CSS,
        jquery_js: JQUERY_JS,
        datatables_js: DATATABLES_JS,
        mermaid_js: MERMAID_JS,
        cytoscape_js: CYTOSCAPE_JS,
        interaction_js: INTERACTION_JS,
        appearance_js: APPEARANCE_JS,
        theme_js: THEME_JS,
        cyto_dag_js: CYTO_DAG_JS,
        favicon_data_uri: FAVICON_DATA_URI,
        report_title,
        report_subtitle,
        banner_text: &banner_text,
        baseline_label,
        is_pr_diff: scope_source == ScopeSource::PrDiff,
        payload_json: &payload_json,
        project_panel: project_facts.panel.as_ref().map(project_panel_view),
    };
    let html = template
        .render()
        .map_err(|err| io::Error::other(format!("render: {err}")))?;
    fs::write(out, html)
}

/// Build a [`ModelPayload`] for one in-scope model. `current` is the
/// whole manifest — needed to resolve each given's input model and the
/// column-scoped tests for the cute-dbt#165 column-header metadata.
#[allow(clippy::too_many_arguments)] // mirrors render_report's rationale
fn build_model_payload(
    current: &Manifest,
    model: &Node,
    tests: &[(&str, &UnitTest)],
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    check_policy: &CheckPolicy<HeuristicId>,
) -> ModelPayload {
    let bare_name = leaf_segment(model.id().as_str()).to_owned();
    let compiled_code = model.compiled_code().unwrap_or_default();
    let graph = parse_cte_graph(compiled_code).unwrap_or_default();
    let is_recursive = graph.is_recursive();
    let nodes = build_node_payloads(&graph, &bare_name);
    let edges = build_edge_payloads(&graph);
    let compiled_sql = build_compiled_sql(&graph, &bare_name, compiled_code);
    let raw_sql = model
        .raw_code()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    // SQL diff is keyed by the model's FULL node id (the
    // `reconstruct_model_sql_diffs` key), not the bare name.
    let sql_diff = sql_diffs.get(model.id().as_str()).cloned();
    // cute-dbt#247 — the Model-YAML gather outcome is keyed by the FULL
    // node id too (the `gather_model_yaml` key); the payload mapping
    // resolves it to the sliced block or a truthful degrade placeholder.
    let model_yaml = model_yaml_payload(model_yaml.get(model.id().as_str()), &bare_name);
    // cute-dbt#165 — the target model's full column-metadata map, built
    // once and shared by every test's expect table (and `this` givens).
    let target_column_meta = column_meta_for_model(current, model);
    let test_payloads = tests
        .iter()
        .map(|(id, ut)| {
            build_test_payload(
                id,
                ut,
                &graph,
                changed,
                authoring_yaml.get(*id),
                yaml_diffs.get(*id),
                data_diffs.get(*id),
                external_fixtures.get(*id),
                current,
                &target_column_meta,
            )
        })
        .collect();
    ModelPayload {
        name: bare_name,
        // cute-dbt#179 — the full project-relative source path for the
        // Model-SQL code-card header (None on synthetic manifests; the
        // template JS falls back to `<name>.sql`).
        path: model.original_file_path().map(str::to_owned),
        // cute-dbt#200 — the authored model description (None — key
        // omitted — for undescribed models, so pre-#200 payloads stay
        // byte-stable).
        description: model.description().map(str::to_owned),
        dag: DagPayload { nodes, edges },
        compiled_sql,
        raw_sql,
        sql_diff,
        model_yaml,
        tests: test_payloads,
        is_recursive,
        is_incremental: model.config().materialized() == Some("incremental"),
        // cute-dbt#169 — the check engine runs here, during payload
        // assembly over models_in_scope (the parse_ctes precedent: the
        // run loop's per-model work happens one stage downstream). The
        // already-parsed graph rides along so graph-fact checks
        // (cute-dbt#172) reuse the single parse pass.
        // cute-dbt#171 — the display policy applies strictly AFTER
        // model_findings' evaluate-all → resolve-supersedes pipeline:
        // selection removes, suppression marks (reason rides into the
        // payload). The default policy is a no-op.
        // cute-dbt#170 — each finding is then wrapped into its render
        // shape (pin target resolved against the parsed graph; sketch
        // evidence lifted into copyable code blocks).
        findings: apply_check_policy(model_findings(current, model, Some(&graph)), check_policy)
            .into_iter()
            .map(|finding| finding_payload(&graph, finding))
            .collect(),
    }
}

/// Build [`NodePayload`]s for every graph node.
///
/// `id` is always the engine's node name (stable + unique within a model);
/// the terminal node's display [`label`](NodePayload::label) is the model's
/// file name (`<model>.sql`), never its id — so a CTE that shares the
/// model's name neither collapses into the terminal (distinct ids) nor
/// reads ambiguously on the graph (the `.sql` suffix marks the model's own
/// final select apart from a same-named import CTE) (cute-dbt#155).
fn build_node_payloads(graph: &CteGraph, model_name: &str) -> Vec<NodePayload> {
    graph
        .nodes()
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let role = classify_node_role(graph, idx);
            let label = (role == NodeRole::Final).then(|| format!("{model_name}.sql"));
            NodePayload {
                id: node.name().to_owned(),
                label,
                role,
            }
        })
        .collect()
}

/// Build [`EdgePayload`]s, keyed by each endpoint's stable node id.
fn build_edge_payloads(graph: &CteGraph) -> Vec<EdgePayload> {
    graph
        .edges()
        .iter()
        .map(|edge| EdgePayload {
            from: endpoint_id(graph, edge.from()),
            to: endpoint_id(graph, edge.to()),
            edge_type: edge.edge_type(),
        })
        .collect()
}

/// Resolve a graph-node index to its stable rendered id — the engine's
/// node name (a CTE alias, or [`TERMINAL_NODE_NAME`] for the terminal).
fn endpoint_id(graph: &CteGraph, index: usize) -> String {
    graph
        .nodes()
        .get(index)
        .map(|node| node.name().to_owned())
        .unwrap_or_default()
}

/// Build the `compiled_sql` map: per-node `raw_sql` keyed by the stable
/// node id (a CTE alias, or [`TERMINAL_NODE_NAME`] for the terminal).
///
/// The empty-graph branch (a model with no `WITH` clause emits no nodes)
/// falls back to the full compiled code keyed by the model's bare name so
/// the renderer still surfaces SOMETHING; that key is never reached by the
/// node-keyed lookup (there are no nodes), so it cannot collide.
fn build_compiled_sql(
    graph: &CteGraph,
    model_name: &str,
    full_compiled_code: &str,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if graph.is_empty() {
        map.insert(model_name.to_owned(), full_compiled_code.to_owned());
        return map;
    }
    for node in graph.nodes() {
        if let Some(sql) = node.raw_sql() {
            map.insert(node.name().to_owned(), sql.to_owned());
        }
    }
    map
}

/// One external fixture file loaded for a given/expect (cute-dbt#126).
///
/// Produced by the cli `gather_external_fixtures` step from the
/// `ProjectFileReader` port; consumed by `build_test_payload` to
/// **inline** the file content into the render payload so an external
/// fixture renders identically to an inline one. `text` is the raw file
/// body (becomes the payload `rows` String — drives the sql code-block
/// fallback + suppresses the external-fixture affordance); `table` is the
/// parsed grid POD (`None` for a non-literal sql file → code block);
/// `format` is the *effective* format (manifest `format:`, else derived
/// from the path extension) so the template's format-aware branches behave
/// even when an engine omits `format` on an external fixture.
#[derive(Debug, Clone)]
pub struct LoadedFixture {
    /// Raw file body read from the working tree.
    pub text: String,
    /// Effective `format` (manifest value or extension-derived).
    pub format: Option<String>,
    /// Parsed grid, or `None` for a non-literal-sql / non-tabulatable file.
    pub table: Option<FixtureTable>,
}

/// The external fixtures successfully READ for one unit test (cute-dbt#126).
///
/// `given` is keyed by the given's **source ordinal** (its position in the
/// test's `given:` list) — the same identity the cell-diff binds on
/// (cute-dbt#131) — because a test may mix inline and external givens, and
/// two givens may share a fixture path. Only successfully-read external
/// fixtures appear: an unreadable one is simply absent, so the payload
/// keeps `rows: null` + `fixture` and the template shows the #98 affordance.
#[derive(Debug, Clone, Default)]
pub struct ExternalFixtures {
    /// Loaded external givens, keyed by source ordinal.
    pub given: BTreeMap<usize, LoadedFixture>,
    /// The loaded external `expect`, when present.
    pub expect: Option<LoadedFixture>,
}

/// Resolve a given/expect's `(rows, table, format)` payload triple —
/// inlining a loaded external fixture (cute-dbt#126) when one was read,
/// else the inline manifest values. With a [`LoadedFixture`] the payload
/// `rows` carries the file text (so the template renders it like an inline
/// fixture and the external affordance is suppressed) and `table`/`format`
/// come from the load; the caller retains `fixture` as provenance.
fn resolve_fixture_payload(
    rows: &Value,
    format: Option<&str>,
    fixture: Option<&str>,
    loaded: Option<&LoadedFixture>,
) -> (Value, Option<FixtureTable>, Option<String>) {
    match loaded {
        Some(lf) => (
            Value::String(lf.text.clone()),
            lf.table.clone(),
            lf.format.clone(),
        ),
        None => (
            rows.clone(),
            current_view_table(rows, format, fixture),
            format.map(str::to_owned),
        ),
    }
}

/// Build a single test's payload, including import-CTE binding for each
/// given. `changed` is the set of updated test ids — `id`'s membership
/// sets [`TestPayload::changed`] (cute-dbt#91). `external` carries any
/// external fixture files read for this test (cute-dbt#126), inlined into
/// the given/expect payloads. `current` + `target_column_meta` feed the
/// cute-dbt#165 column-header metadata: the expect table (and a `this`
/// given) read the target model's map; a `ref(...)` given resolves its
/// own input model's map.
#[allow(clippy::too_many_arguments)] // mirrors render_report's rationale
fn build_test_payload(
    id: &str,
    unit_test: &UnitTest,
    graph: &CteGraph,
    changed: &InScopeSet,
    authoring_yaml: Option<&UnitTestYamlBlock>,
    yaml_diff: Option<&BlockDiff>,
    data_diff: Option<&UnitTestDataDiff>,
    external: Option<&ExternalFixtures>,
    current: &Manifest,
    target_column_meta: &BTreeMap<String, ColumnMetaPayload>,
) -> TestPayload {
    let given = unit_test
        .given()
        .iter()
        .enumerate()
        .map(|(ordinal, g)| {
            // Bind the given to a leaf CTE: `ref(...)` matches directly
            // by model name; `source(...)` resolves through the
            // manifest's sources map first (cute-dbt#57) because dbt
            // pre-resolves source refs at compile time. The two arms are
            // disjoint by construction (an input parses as at most one
            // shape), so `or_else` is chain order, not precedence.
            let bound_to_node = parse_ref_name(g.input())
                .and_then(|ref_name| find_import_node_id(graph, ref_name))
                .or_else(|| {
                    parse_source_ref(g.input()).and_then(|(source_name, table_name)| {
                        find_source_import_node_id(current, graph, source_name, table_name)
                    })
                });
            let loaded = external.and_then(|e| e.given.get(&ordinal));
            let (rows, table, format) =
                resolve_fixture_payload(g.rows(), g.format(), g.fixture(), loaded);
            let is_this = g.input() == "this";
            // cute-dbt#165/#235 — the node that OWNS this given's
            // columns: the target model for `this` (prior model state),
            // the resolved refable node (model / seed / snapshot) for a
            // `ref(...)` input, or the manifest source for a
            // `source(...)` input. Unresolvable inputs contribute
            // nothing (empty map → key omitted → no trigger, never an
            // empty bubble).
            let column_meta = if is_this {
                column_meta_for_table(target_column_meta, table.as_ref())
            } else if let Some(ref_name) = parse_ref_name(g.input()) {
                resolve_given_ref_node(current, ref_name)
                    .map(|input_node| {
                        let meta = column_meta_for_model(current, input_node);
                        column_meta_for_table(&meta, table.as_ref())
                    })
                    .unwrap_or_default()
            } else if let Some((source_name, table_name)) = parse_source_ref(g.input()) {
                current
                    .source_by_name(source_name, table_name)
                    .map(|source| {
                        let meta = column_meta_for_source(current, source);
                        column_meta_for_table(&meta, table.as_ref())
                    })
                    .unwrap_or_default()
            } else {
                BTreeMap::new()
            };
            GivenPayload {
                input: g.input().to_owned(),
                bound_to_node,
                table,
                rows,
                format,
                fixture: g.fixture().map(str::to_owned),
                is_this,
                column_meta,
            }
        })
        .collect();
    let (expect_rows, expect_table, expect_format) = resolve_fixture_payload(
        unit_test.expect().rows(),
        unit_test.expect().format(),
        unit_test.expect().fixture(),
        external.and_then(|e| e.expect.as_ref()),
    );
    let expect_column_meta = column_meta_for_table(target_column_meta, expect_table.as_ref());
    TestPayload {
        id: id.to_owned(),
        name: unit_test.name().to_owned(),
        target_model: unit_test.model().as_str().to_owned(),
        changed: changed.contains(id),
        description: unit_test.description().map(str::to_owned),
        tags: unit_test.tags().map(<[String]>::to_vec),
        meta: unit_test.meta().cloned(),
        defined_in: unit_test.original_file_path().map(str::to_owned),
        authoring_yaml: authoring_yaml.map(|b| b.raw.clone()),
        yaml_diff: yaml_diff.cloned(),
        data_diff: data_diff.cloned(),
        is_incremental_mode: unit_test.is_incremental_mode(),
        // cute-dbt#200 — the full grouped overrides blob, native scalar
        // values preserved (the adapter already dropped empty groups).
        overrides: unit_test.overrides().cloned(),
        given,
        expected: ExpectedPayload {
            table: expect_table,
            rows: expect_rows,
            format: expect_format,
            fixture: unit_test.expect().fixture().map(str::to_owned),
            column_meta: expect_column_meta,
        },
    }
}

/// Compute the Current-view [`FixtureTable`] POD for a fixture's `rows` +
/// `format` (cute-dbt#138): the authoritative tabulated cells the template
/// renders directly, so the JS never parses csv/dict.
///
/// Returns `None` for the two non-tabulatable cases so the JS falls back to
/// its sql / external-fixture affordances:
///
/// 1. **External fixture** — `fixture:` is set AND `rows` is `Value::Null`
///    (the data is not in the manifest). A `Null` `rows` would otherwise
///    normalize to the *empty* table, hiding the "data in external file"
///    affordance behind a silently-empty grid (cute-dbt#126). We gate on the
///    `fixture:`+`Null` pair (NOT on `rows == Null` alone — a genuinely empty
///    inline fixture still tabulates to the empty grid).
/// 2. **non-literal sql / opaque** — [`table_from_manifest_rows`] returns
///    `None` for a sql `rows` that is NOT a literal-row `SELECT … UNION ALL`
///    (cute-dbt#137); the JS renders the sql code block. A literal-row sql
///    tabulates here just like dict/csv.
fn current_view_table(
    rows: &Value,
    format: Option<&str>,
    fixture: Option<&str>,
) -> Option<FixtureTable> {
    if fixture.is_some() && rows.is_null() {
        return None; // external fixture → JS affordance, not an empty grid
    }
    table_from_manifest_rows(rows, format)
}

/// Locate the leaf CTE node that binds to `ref_name`.
///
/// Two-pass match — both case-insensitive:
///
/// 1. **Name match** (the design's sample-data convention): an
///    import-CTE whose own name equals `ref_name`. Pass-1 is strict —
///    the node must also classify as [`NodeRole::Import`], so a
///    transform CTE that happens to share a name with the queried
///    `ref()` cannot spuriously bind.
/// 2. **Body match** (dbt's idiomatic compiled-SQL shape and the
///    multi-ref messy-import case): a leaf CTE (no incoming edges,
///    not the terminal) whose body references a table whose leaf
///    identifier equals `ref_name`. dbt-compiled SQL commonly carries
///    `with source as (select * from "db"."schema"."MODEL")`, where
///    the CTE name is the unwrapper convention (`source`, `src_*`,
///    etc.) and the model name lives only inside the body. The messy
///    import case (cute-dbt#34) further generalises this: a single
///    CTE body may reference multiple `ref()` targets (e.g. via
///    `UNION ALL` or `JOIN`), which the engine classifies as
///    `Transform` rather than `Import` because the body is not a
///    single `SELECT … FROM <relation>`. Pass-2 widens the gate to any
///    leaf node with engine-extracted
///    [`body_leaf_table_refs`](crate::domain::CteNode::body_leaf_table_refs);
///    the presence of those refs is the binding signal regardless of
///    the structural [`NodeRole`] badge the DAG renders.
///
/// Returns the matching node's name (the payload's stable node id),
/// or `None` when neither pass matches. The renderer (and the
/// template JS) call this once per `given[].input` so a unit test
/// whose multiple `ref()` givens all live inside one CTE body see
/// every given bound to that single node id (cute-dbt#34).
fn find_import_node_id(graph: &CteGraph, ref_name: &str) -> Option<String> {
    let target = ref_name.to_ascii_lowercase();
    // Pass 1: name match (design's convention; strict role gate).
    if let Some((_, node)) = graph.nodes().iter().enumerate().find(|(idx, node)| {
        node.name().eq_ignore_ascii_case(&target)
            && classify_node_role(graph, *idx) == NodeRole::Import
    }) {
        return Some(node.name().to_owned());
    }
    // Pass 2: body match — any leaf CTE with the ref in its
    // engine-extracted body_leaf_table_refs (catches dbt's `source`
    // unwrapper shape AND the messy multi-ref shape).
    for (idx, node) in graph.nodes().iter().enumerate() {
        if !is_leaf_binding_candidate(graph, idx) {
            continue;
        }
        // The engine already lowercases body_leaf_table_refs at extract
        // time (cte_engine.rs::push_leaf), so the case-fold here is
        // belt-and-braces — defends pass-2 against any future engine
        // change that ships raw-case refs, and keeps the contract
        // symmetric with pass-1's `eq_ignore_ascii_case` (Gemini PR 17).
        if node
            .body_leaf_table_refs()
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&target))
        {
            return Some(node.name().to_owned());
        }
    }
    None
}

/// Locate the leaf CTE node that binds to a `source('a', 'b')` given
/// (cute-dbt#57) — the source-side sibling of [`find_import_node_id`].
///
/// dbt compiles `{{ source('a', 'b') }}` into the resolved relation at
/// `dbt compile` time, so the compiled SQL the CTE engine sees never
/// carries the literal `source(...)` form — only the resolved relation
/// (`"db"."schema"."table"`). Binding therefore walks the manifest's
/// `sources` map:
///
/// 1. Resolve `(source_name, table_name)` — the two authored `source()`
///    arguments — against [`Manifest::source_by_name`]. The lookup is on
///    `source_name` + `name`, **not** `identifier` (the args are the
///    YAML names; `identifier` may be overridden to differ).
/// 2. Derive the match token from the hit: the physical `identifier`
///    (quote-stripped — dbt preserves embedded quote characters
///    verbatim, the reserved-word `"GROUP"` case), falling back to the
///    last dot-segment of `relation_name`, falling back to `name`
///    (dbt's own identifier default).
/// 3. Feed the token through the existing two-pass
///    [`find_import_node_id`]: the CTE engine already reduces every
///    FROM/JOIN leaf to the lowercased, quote-stripped **last
///    identifier** of the relation, and the compiled-SQL relation text
///    and the manifest `relation_name` render from the same relation
///    object in both engines — so the leaf token match is exactly the
///    normalization the ref path already uses. No FQN machinery.
///
/// Returns `None` when the pair is missing from the sources map or no
/// leaf CTE references the relation — the same fail-open empty-state as
/// an unresolvable `ref` (sources need no preflight: they are referenced
/// by models, never analyzed themselves).
fn find_source_import_node_id(
    manifest: &Manifest,
    graph: &CteGraph,
    source_name: &str,
    table_name: &str,
) -> Option<String> {
    let source = manifest.source_by_name(source_name, table_name)?;
    let token = source_relation_token(source);
    if token.is_empty() {
        return None;
    }
    find_import_node_id(graph, token)
}

/// The leaf table identifier a resolved [`SourceNode`] is expected to
/// appear as inside a compiled CTE body: `identifier` when present and
/// non-empty (quote-stripped), else the last dot-segment of
/// `relation_name` (quote-stripped; naive split — a dot embedded in a
/// quoted identifier is out of scope, the identifier-first chain makes
/// this fallback rare), else the source's `name`. Returns a borrowed
/// slice of the [`SourceNode`]'s own fields — every branch is a
/// quote-strip view, so no allocation is needed (Gemini PR 204).
fn source_relation_token(source: &SourceNode) -> &str {
    let identifier = source
        .identifier()
        .map(strip_ident_quotes)
        .filter(|ident| !ident.is_empty());
    let from_relation = || {
        source
            .relation_name()
            .and_then(|relation| relation.rsplit('.').next())
            .map(strip_ident_quotes)
            .filter(|segment| !segment.is_empty())
    };
    identifier
        .or_else(from_relation)
        .unwrap_or_else(|| strip_ident_quotes(source.name()))
}

/// Strip the common SQL identifier quoting characters from both ends of
/// `s`: double quotes (ANSI / dbt `relation_name`), backticks (`BigQuery`
/// / `MySQL`), and square brackets (T-SQL). Interior characters are kept
/// verbatim; lowercasing is the caller's concern
/// ([`find_import_node_id`] case-folds both sides).
fn strip_ident_quotes(s: &str) -> &str {
    s.trim()
        .trim_matches(|c| c == '"' || c == '`')
        .trim_start_matches('[')
        .trim_end_matches(']')
}

/// `true` when the node at `index` is a candidate for pass-2 binding —
/// i.e. a leaf CTE that may carry engine-extracted body-leaf refs.
///
/// A binding candidate is any non-terminal node with zero incoming
/// edges. We deliberately do not require [`NodeRole::Import`] here:
/// the import classification narrows to single-source bodies, which
/// excludes the multi-ref shapes this PR (cute-dbt#34) needs to bind
/// (`UNION ALL`, `JOIN`, derived subqueries). Leaf-ness alone is the
/// binding contract; structural shape is the DAG-badge contract.
fn is_leaf_binding_candidate(graph: &CteGraph, index: usize) -> bool {
    let Some(node) = graph.nodes().get(index) else {
        return false;
    };
    if node.name() == TERMINAL_NODE_NAME {
        return false;
    }
    !graph.edges().iter().any(|edge| edge.to() == index)
}

/// Build a map from in-scope model id to **all** unit tests targeting it
/// in the current manifest (cute-dbt#91 widening).
///
/// Resolved via [`resolve_tested_model`] (the engine-resolved
/// `tested_node_unique_id` when present, the bare `model:` name
/// otherwise — cute-dbt#254). Unlike the prior in-scope-only indexer, this
/// enumerates every unit test whose resolved target is one of
/// `models_in_scope`, regardless of whether the test is itself in scope —
/// so a model that entered scope solely via a changed test also carries
/// its non-updated (context) siblings, making the report's All-tests mode
/// and per-model total count work.
///
/// `current.unit_tests()` is a `HashMap` with no inherent order, so each
/// model's list is sorted by unit-test id — the deterministic
/// `BTreeSet`-over-id order the prior indexer produced, which the golden
/// snapshot and the example byte-identity gate depend on.
#[must_use]
pub fn index_tests_for_models<'m>(
    current: &'m Manifest,
    models_in_scope: &ModelInScopeSet,
) -> HashMap<NodeId, Vec<(&'m str, &'m UnitTest)>> {
    let mut map: HashMap<NodeId, Vec<(&'m str, &'m UnitTest)>> = HashMap::new();
    for (test_id, unit_test) in current.unit_tests() {
        let Some(model) = resolve_tested_model(current, unit_test) else {
            continue;
        };
        if models_in_scope.contains(model.id()) {
            map.entry(model.id().clone())
                .or_default()
                .push((test_id.as_str(), unit_test));
        }
    }
    for tests in map.values_mut() {
        tests.sort_by(|a, b| a.0.cmp(b.0));
    }
    map
}

/// Final `.`-delimited segment of a node id (`model.shop.x` → `x`).
fn leaf_segment(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Checksum, CteEdge, CteNode, DEFAULT_REPORT_TITLE, DependsOn, DiffLine, DiffLineKind,
        EdgeType, Manifest, ManifestMetadata, NodeConfig, NodeId, UnitTest, UnitTestExpect,
        UnitTestGiven,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap};

    // ===== parse_ref_name =====

    #[test]
    fn parse_ref_name_extracts_single_quoted_name() {
        assert_eq!(parse_ref_name("ref('stg_orders')"), Some("stg_orders"));
    }

    #[test]
    fn parse_ref_name_tolerates_surrounding_whitespace() {
        assert_eq!(parse_ref_name("  ref('a')  "), Some("a"));
    }

    #[test]
    fn parse_ref_name_accepts_case_variant_keyword() {
        assert_eq!(parse_ref_name("REF('A')"), Some("A"));
        assert_eq!(parse_ref_name("Ref('b')"), Some("b"));
        // Any byte casing matches the keyword now (case-insensitive).
        assert_eq!(parse_ref_name("rEf('c')"), Some("c"));
    }

    #[test]
    fn parse_ref_name_tolerates_whitespace_between_ref_and_paren() {
        // Jinja's `{{ ref(...) }}` macro accepts whitespace between the
        // keyword and the opening paren; the YAML-stored verbatim form
        // may carry it.
        assert_eq!(parse_ref_name("ref ('x')"), Some("x"));
        assert_eq!(parse_ref_name("REF\t('Y')"), Some("Y"));
        assert_eq!(parse_ref_name("ref   ('z')"), Some("z"));
    }

    #[test]
    fn parse_ref_name_returns_none_on_missing_parens() {
        assert_eq!(parse_ref_name("ref'stg_orders'"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_empty_inner() {
        assert_eq!(parse_ref_name("ref('')"), None);
        assert_eq!(parse_ref_name("ref(\"\")"), None);
    }

    #[test]
    fn parse_ref_name_accepts_double_quoted_name() {
        // dbt accepts both quote styles in an authored given `input:`
        // and fusion ships the authored string VERBATIM on the manifest
        // wire (cute-dbt#245 — verified against a real fusion
        // 2.0.0-preview.177 compile of the dogfood project).
        assert_eq!(parse_ref_name("ref(\"x\")"), Some("x"));
        assert_eq!(
            parse_ref_name("ref(\"stg_payments\")"),
            Some("stg_payments")
        );
        // Same keyword/whitespace tolerances as the single-quoted form.
        assert_eq!(parse_ref_name("REF (\"Y\")"), Some("Y"));
        assert_eq!(parse_ref_name("  ref(\"a\")  "), Some("a"));
    }

    #[test]
    fn parse_ref_name_returns_none_on_mixed_quotes() {
        // Open/close must be the SAME character — a mixed pair is not a
        // valid Python/Jinja string literal. Fail-open (cute-dbt#245).
        assert_eq!(parse_ref_name("ref(\"x')"), None);
        assert_eq!(parse_ref_name("ref('x\")"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_unmatched_quotes() {
        assert_eq!(parse_ref_name("ref('x"), None);
        assert_eq!(parse_ref_name("ref(x')"), None);
        assert_eq!(parse_ref_name("ref(\"x"), None);
        assert_eq!(parse_ref_name("ref(x\")"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_non_ref_input() {
        assert_eq!(parse_ref_name("source('a', 'b')"), None);
        assert_eq!(parse_ref_name(""), None);
        assert_eq!(parse_ref_name("plain_table"), None);
    }

    // ===== parse_source_ref (cute-dbt#57) =====

    #[test]
    fn parse_source_ref_extracts_both_single_quoted_names() {
        assert_eq!(
            parse_source_ref("source('synthea_raw', 'patients')"),
            Some(("synthea_raw", "patients")),
        );
    }

    #[test]
    fn parse_source_ref_tolerates_whitespace_variants() {
        // Surrounding, keyword↔paren, and around-the-comma whitespace —
        // mirrors parse_ref_name's Jinja-tolerance.
        assert_eq!(parse_source_ref("  source('a', 'b')  "), Some(("a", "b")));
        assert_eq!(parse_source_ref("source ('a','b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("SOURCE\t('a' , 'b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("source( 'a','b' )"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_accepts_case_variant_keyword() {
        assert_eq!(parse_source_ref("SOURCE('A', 'B')"), Some(("A", "B")));
        assert_eq!(parse_source_ref("Source('a', 'b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("sOuRcE('a', 'b')"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_returns_none_on_missing_parens_or_arg() {
        assert_eq!(parse_source_ref("source'a','b'"), None);
        assert_eq!(
            parse_source_ref("source('a')"),
            None,
            "one arg is not a source ref"
        );
        assert_eq!(parse_source_ref("source('a', 'b'"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_empty_names() {
        assert_eq!(parse_source_ref("source('', 'b')"), None);
        assert_eq!(parse_source_ref("source('a', '')"), None);
        assert_eq!(parse_source_ref("source('', '')"), None);
        assert_eq!(parse_source_ref("source(\"\", \"b\")"), None);
        assert_eq!(parse_source_ref("source(\"a\", \"\")"), None);
    }

    #[test]
    fn parse_source_ref_accepts_double_quoted_names() {
        // Same cute-dbt#245 evidence as parse_ref_name: both quote
        // styles are engine-valid authored forms and ship verbatim on
        // the manifest wire.
        assert_eq!(
            parse_source_ref("source(\"synthea_raw\", \"patients\")"),
            Some(("synthea_raw", "patients")),
        );
        assert_eq!(parse_source_ref("SOURCE (\"a\", \"b\")"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_accepts_mixed_style_args() {
        // The matching-quote rule is PER ARGUMENT — each arg is its own
        // string literal, so the two args may use different styles.
        assert_eq!(parse_source_ref("source(\"a\", 'b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("source('a', \"b\")"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_returns_none_on_three_argument_calls() {
        // dbt's source() takes exactly two arguments — a malformed
        // 3-arg call must reject outright, never strip the second
        // fragment to a garbage pair like ("a", "b','c") (CodeRabbit
        // PR #248). Pinned for BOTH quote styles.
        assert_eq!(parse_source_ref("source('a','b','c')"), None);
        assert_eq!(parse_source_ref("source(\"a\",\"b\",\"c\")"), None);
        assert_eq!(parse_source_ref("source('a', 'b', 'c')"), None);
        // Trailing comma = an empty third part — same rejection.
        assert_eq!(parse_source_ref("source('a','b',)"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_mixed_quotes_within_an_arg() {
        // A mixed open/close pair inside EITHER argument fails that
        // arg's matching-quote strip — fail-open (cute-dbt#245).
        assert_eq!(parse_source_ref("source(\"a', 'b')"), None);
        assert_eq!(parse_source_ref("source('a\", 'b')"), None);
        assert_eq!(parse_source_ref("source('a', \"b')"), None);
        assert_eq!(parse_source_ref("source('a', 'b\")"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_unmatched_quotes() {
        // A comma inside a quoted name leaves the first split fragment
        // with an unbalanced quote — both quote styles fail the strip
        // (the fail-open-by-construction property, cute-dbt#245).
        assert_eq!(parse_source_ref("source('a, 'b')"), None);
        assert_eq!(parse_source_ref("source('a', b')"), None);
        assert_eq!(parse_source_ref("source(\"a, \"b\")"), None);
        assert_eq!(parse_source_ref("source(\"a\", b\")"), None);
        assert_eq!(parse_source_ref("source(\"a,b\", \"c\")"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_non_source_input() {
        assert_eq!(parse_source_ref("ref('stg_orders')"), None);
        assert_eq!(parse_source_ref("this"), None);
        assert_eq!(parse_source_ref(""), None);
        assert_eq!(parse_source_ref("plain_table"), None);
    }

    // ===== double-quoted given rendering (cute-dbt#245) =====

    #[test]
    fn render_report_binds_double_quoted_given_and_populates_column_meta() {
        // cute-dbt#245 AC4: fusion ships an authored `ref("…")` given
        // input VERBATIM on the manifest wire; the rendered report must
        // bind it to its import CTE (`bound_to_node`) and resolve the
        // input model's column metadata exactly like the single-quoted
        // form. Asserted end-to-end through render_report so the proof
        // covers the full payload-into-HTML path, not just the parser.
        let compiled = "with stg_payments_src as (select * from raw_payments) \
                        select * from stg_payments_src";
        let target = model_node("model.shop.orders", "body", Some(compiled));
        let mut src_desc = BTreeMap::new();
        src_desc.insert(
            "payment_id".to_owned(),
            "Double-quoted given column marker".to_owned(),
        );
        let src = model_node("model.shop.stg_payments_src", "s", Some("select 1"))
            .with_column_descriptions(src_desc);
        let ut = UnitTest::new(
            "test_dq",
            NodeId::new("orders"),
            vec![UnitTestGiven::new(
                "ref(\"stg_payments_src\")",
                json!([{ "payment_id": 1 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![target, src], vec![("unit_test.shop.test_dq", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_dq".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_double_quoted_given_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &in_scope,
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("\"bound_to_node\":\"stg_payments_src\""),
            "a double-quoted given binds to its import CTE in the rendered payload",
        );
        assert!(
            html.contains("Double-quoted given column marker"),
            "a double-quoted given resolves its input model's column_meta",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== source_relation_token / strip_ident_quotes (cute-dbt#57) =====

    fn source_node(
        identifier: Option<&str>,
        relation_name: Option<&str>,
        name: &str,
    ) -> SourceNode {
        SourceNode::new(
            NodeId::new(format!("source.shop.raw.{name}")),
            "raw",
            name,
            identifier.map(str::to_owned),
            "main",
            Some("memory".to_owned()),
            relation_name.map(str::to_owned),
        )
    }

    #[test]
    fn source_relation_token_prefers_the_identifier() {
        let s = source_node(
            Some("patients_v2"),
            Some("\"memory\".\"main\".\"patients_v2\""),
            "patients",
        );
        assert_eq!(source_relation_token(&s), "patients_v2");
    }

    #[test]
    fn source_relation_token_strips_embedded_identifier_quotes() {
        // dbt preserves a reserved-word identifier verbatim INCLUDING
        // its quote characters (the zendesk `"GROUP"` case).
        let s = source_node(Some("\"GROUP\""), None, "group");
        assert_eq!(source_relation_token(&s), "GROUP");
    }

    #[test]
    fn source_relation_token_falls_back_to_relation_name_last_segment() {
        let s = source_node(
            None,
            Some("\"memory\".\"main\".\"patients\""),
            "patients_yaml",
        );
        assert_eq!(source_relation_token(&s), "patients");
    }

    #[test]
    fn source_relation_token_falls_back_to_the_source_name_field() {
        // The fusion-minimal entry: no identifier, no relation_name —
        // dbt defaults the physical identifier to `name`.
        let s = source_node(None, None, "patients");
        assert_eq!(source_relation_token(&s), "patients");
    }

    #[test]
    fn strip_ident_quotes_handles_each_quoting_dialect() {
        assert_eq!(strip_ident_quotes("\"orders\""), "orders");
        assert_eq!(strip_ident_quotes("`orders`"), "orders");
        assert_eq!(strip_ident_quotes("[orders]"), "orders");
        assert_eq!(strip_ident_quotes("  orders  "), "orders");
        assert_eq!(strip_ident_quotes("orders"), "orders");
    }

    // ===== bind_import_to_given =====

    fn sample_given(input: &str) -> UnitTestGiven {
        UnitTestGiven::new(input, json!([]), None, None)
    }

    fn unit_test_with_givens(givens: Vec<UnitTestGiven>) -> UnitTest {
        UnitTest::new(
            "t",
            NodeId::new("model.shop.x"),
            givens,
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    #[test]
    fn bind_import_matches_ref_name_to_cte_name() {
        let ut = unit_test_with_givens(vec![sample_given("ref('stg_orders')")]);
        let bound = bind_import_to_given(&ut, "stg_orders");
        assert!(bound.is_some());
        assert_eq!(bound.unwrap().input(), "ref('stg_orders')");
    }

    #[test]
    fn bind_import_is_case_insensitive() {
        let ut = unit_test_with_givens(vec![sample_given("ref('Stg_Orders')")]);
        assert!(bind_import_to_given(&ut, "stg_orders").is_some());
        assert!(bind_import_to_given(&ut, "STG_ORDERS").is_some());
    }

    #[test]
    fn bind_import_returns_none_on_no_match() {
        let ut = unit_test_with_givens(vec![sample_given("ref('stg_orders')")]);
        assert!(bind_import_to_given(&ut, "stg_customers").is_none());
    }

    #[test]
    fn bind_import_returns_none_on_empty_givens() {
        let ut = unit_test_with_givens(vec![]);
        assert!(bind_import_to_given(&ut, "any").is_none());
    }

    #[test]
    fn bind_import_picks_first_match_when_multiple() {
        let ut = unit_test_with_givens(vec![
            sample_given("ref('stg_orders')"),
            sample_given("ref('stg_orders')"),
        ]);
        let bound = bind_import_to_given(&ut, "stg_orders");
        assert!(bound.is_some());
    }

    // ===== classify_node_role =====

    /// Construct a transform-shaped `CteNode` for fixtures: shape facts
    /// default to `is_simple_from_shape = false` and empty refs.
    fn cte_node(name: &str, raw_sql: Option<&str>) -> CteNode {
        CteNode::new(name, None, raw_sql.map(str::to_owned), None)
    }

    /// Construct an import-shaped `CteNode` for fixtures — the renderer
    /// classifies these as [`NodeRole::Import`] when they have no
    /// incoming edges. The engine populates these facts via AST walk;
    /// tests pin them directly (cute-dbt#40).
    fn import_cte_node(name: &str, raw_sql: &str, body_leaf_refs: &[&str]) -> CteNode {
        cte_node(name, Some(raw_sql)).with_shape_facts(
            true,
            body_leaf_refs.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    #[test]
    fn classify_terminal_node_as_final() {
        let graph = CteGraph::new(
            vec![
                cte_node("stg_orders", Some("select * from x")),
                cte_node(TERMINAL_NODE_NAME, Some("select * from stg_orders")),
            ],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        );
        assert_eq!(classify_node_role(&graph, 1), NodeRole::Final);
    }

    #[test]
    fn classify_simple_from_select_with_no_incoming_as_import() {
        let graph = CteGraph::new(
            vec![import_cte_node(
                "stg_orders",
                "select id from raw.orders",
                &["orders"],
            )],
            vec![],
        );
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Import);
    }

    #[test]
    fn classify_node_with_incoming_edges_as_transform() {
        // Even when the node carries simple-from-shape facts, an
        // incoming edge takes precedence — the node is consumed by
        // another CTE, so it is part of the transform pipeline.
        let graph = CteGraph::new(
            vec![
                import_cte_node("a", "select 1 from x", &["x"]),
                import_cte_node("b", "select * from a", &["a"]),
            ],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        );
        assert_eq!(classify_node_role(&graph, 1), NodeRole::Transform);
    }

    #[test]
    fn classify_node_with_join_as_transform() {
        // JOIN body — engine would set is_simple_from_shape=false;
        // the default `cte_node` helper preserves that fact.
        let graph = CteGraph::new(
            vec![cte_node(
                "with_join",
                Some("select * from a join b on a.k = b.k"),
            )],
            vec![],
        );
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Transform);
    }

    #[test]
    fn classify_out_of_bounds_index_defaults_to_transform() {
        let graph = CteGraph::default();
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Transform);
        assert_eq!(classify_node_role(&graph, 99), NodeRole::Transform);
    }

    #[test]
    fn classify_node_without_simple_from_shape_is_transform() {
        // A node whose engine-computed `is_simple_from_shape` is `false`
        // (the default for the bare constructor — covers nodes without
        // raw_sql and nodes whose body the engine did not classify as
        // single-source) falls back to `Transform`.
        let graph = CteGraph::new(vec![cte_node("stg_x", None)], vec![]);
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Transform);
    }

    // ===== edge_type_wire_key =====

    #[test]
    fn edge_type_wire_key_matches_serde_snake_case_for_every_variant() {
        // Pin every variant's wire key against the serde-emitted
        // snake_case string. If a new EdgeType variant lands and serde
        // gives it a different snake_case shape than the JS expects, this
        // test fails before the rendered report ships a broken color
        // legend.
        for et in [
            EdgeType::From,
            EdgeType::Inner,
            EdgeType::Left,
            EdgeType::Right,
            EdgeType::Full,
            EdgeType::Cross,
            EdgeType::UnionAll,
            EdgeType::UnionDistinct,
        ] {
            let key = edge_type_wire_key(et);
            let serde_value: String = serde_json::to_string(&et)
                .expect("EdgeType serializes")
                .trim_matches('"')
                .to_owned();
            assert_eq!(key, serde_value, "wire-key drift for {et:?}");
        }
    }

    // ===== compose_banner_text =====

    #[test]
    fn compose_banner_text_empty_scope_is_the_locked_constant() {
        assert_eq!(compose_banner_text(&InScopeSet::new()), BANNER_EMPTY_SCOPE);
    }

    #[test]
    fn compose_banner_text_single_test_uses_singular_noun() {
        let one = InScopeSet::from_iter(["unit_test.shop.a".to_owned()]);
        assert_eq!(compose_banner_text(&one), "Showing 1 unit test in scope");
    }

    #[test]
    fn compose_banner_text_multiple_tests_use_plural_noun() {
        let many =
            InScopeSet::from_iter(["unit_test.shop.a".to_owned(), "unit_test.shop.b".to_owned()]);
        assert_eq!(compose_banner_text(&many), "Showing 2 unit tests in scope");
    }

    // ===== build_payload + render integration =====

    fn checksum(value: &str) -> Checksum {
        Checksum::new("sha256", value)
    }

    fn model_node(id: &str, body: &str, compiled: Option<&str>) -> Node {
        model_node_with_raw(id, body, compiled, None)
    }

    fn model_node_with_raw(
        id: &str,
        body: &str,
        compiled: Option<&str>,
        raw: Option<&str>,
    ) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            checksum(body),
            compiled.map(str::to_owned),
            raw.map(str::to_owned),
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn simple_unit_test(model_bare: &str, name: &str) -> UnitTest {
        UnitTest::new(
            name,
            NodeId::new(model_bare),
            vec![UnitTestGiven::new(
                format!("ref('{model_bare}_src')"),
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            Some("a description".to_owned()),
            DependsOn::default(),
            Some(vec!["finance".to_owned()]),
            Some(json!({"owner": "team"})),
            Some("models/x.yml".to_owned()),
        )
    }

    fn manifest_for(nodes: Vec<Node>, tests: Vec<(&str, UnitTest)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            tests.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
            HashMap::new(),
        )
    }

    fn manifest_with_sources(
        nodes: Vec<Node>,
        tests: Vec<(&str, UnitTest)>,
        sources: Vec<SourceNode>,
    ) -> Manifest {
        manifest_for(nodes, tests)
            .with_sources(sources.into_iter().map(|s| (s.id().clone(), s)).collect())
    }

    #[test]
    fn build_payload_threads_baseline_label_through() {
        let manifest = manifest_for(vec![], vec![]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &ModelInScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "main@a1f3c7e",
        );
        assert_eq!(payload.baseline, "main@a1f3c7e");
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_emits_one_model_entry_per_model_in_scope() {
        let compiled = "with stg_orders_src as (select * from raw_orders) \
                        select * from stg_orders_src";
        let node = model_node("model.shop.stg_orders", "body", Some(compiled));
        let ut = simple_unit_test("stg_orders", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        let model = &payload.models[0];
        assert_eq!(model.name, "stg_orders");
        assert_eq!(model.tests.len(), 1);
        assert_eq!(model.tests[0].name, "test_one");
        assert!(
            model.tests[0].changed,
            "test_one is in the changed set → tagged updated",
        );
    }

    #[test]
    fn model_payload_threads_the_models_original_file_path() {
        // cute-dbt#179 — the Model-SQL code-card header shows the model's
        // full project-relative path (`models/…/x.sql`, never just the
        // filename). `path` rides `Node::original_file_path`.
        let node = Node::new(
            NodeId::new("model.shop.stg_orders"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            Some("models/staging/stg_orders.sql".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        let ut = simple_unit_test("stg_orders", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(
            payload.models[0].path.as_deref(),
            Some("models/staging/stg_orders.sql"),
            "ModelPayload.path carries the manifest original_file_path verbatim",
        );
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            json.contains(r#""path":"models/staging/stg_orders.sql""#),
            "the full path is on the wire for the code-card header",
        );
    }

    #[test]
    fn model_payload_path_is_omitted_when_the_manifest_carries_none() {
        // cute-dbt#179 — synthetic / pre-1.8 manifests carry no
        // original_file_path: the key is omitted from the wire (the JS
        // falls back to `<name>.sql`, the cute-dbt#155 terminal label).
        let node = model_node("model.shop.stg_orders", "body", Some("select 1"));
        let ut = simple_unit_test("stg_orders", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert!(payload.models[0].path.is_none());
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            !json.contains(r#""path""#),
            "absent original_file_path ⇒ no path key on the wire (older fixtures stay stable)",
        );
    }

    #[test]
    fn build_payload_carries_findings_for_an_unbacked_unique_key() {
        // cute-dbt#169 — the check engine runs during payload assembly:
        // an in-scope model declaring config.unique_key with no backing
        // uniqueness test surfaces an UNCOVERED grain finding on its
        // ModelPayload, serialized with the dotted check id.
        let mut config = BTreeMap::new();
        config.insert("materialized".to_owned(), json!("incremental"));
        config.insert("unique_key".to_owned(), json!("order_id"));
        let node = Node::new(
            NodeId::new("model.shop.orders_rollup"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        assert_eq!(json["findings"][0]["check"], "grain.unique-key-unbacked");
        assert_eq!(json["findings"][0]["verdict"]["status"], "uncovered");
        assert_eq!(json["findings"][0]["model_id"], "model.shop.orders_rollup");
    }

    #[test]
    fn build_payload_omits_the_findings_key_when_no_check_fires() {
        // The serde skip keeps every pre-#169 payload byte-stable: a
        // model tripping no check carries NO `findings` key at all.
        let node = model_node("model.shop.plain", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.plain")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        assert!(
            json.get("findings").is_none(),
            "empty findings must be serde-skipped; got {json}"
        );
    }

    /// A model tripping BOTH registered checks: `config.unique_key` with
    /// no backing uniqueness test (grain, UNCOVERED) and a UNION whose
    /// arms no unit test feeds (union, UNCOVERED with sketches).
    fn findings_surface_payload() -> ReportPayload {
        let mut config = BTreeMap::new();
        config.insert("unique_key".to_owned(), json!("event_id"));
        let compiled = "with arm_a as (select * from src_a), \
                        arm_b as (select * from src_b), \
                        unioned as (select * from arm_a union all select * from arm_b) \
                        select * from unioned";
        let node = Node::new(
            NodeId::new("model.shop.events"),
            "model",
            checksum("body"),
            Some(compiled.to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.events")]);
        build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        )
    }

    #[test]
    fn finding_payload_pins_bracketed_constructs_to_the_named_node() {
        // cute-dbt#170 — `union[unioned]` resolves to the `unioned` CTE
        // node; the model-level grain construct pins the terminal node.
        let payload = findings_surface_payload();
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        let findings = json["findings"].as_array().expect("findings present");
        let union = findings
            .iter()
            .find(|f| f["check"] == "union.arm-coverage")
            .expect("union finding fires");
        assert_eq!(union["pin_node"], "unioned");
        let grain = findings
            .iter()
            .find(|f| f["check"] == "grain.unique-key-unbacked")
            .expect("grain finding fires");
        assert_eq!(
            grain["pin_node"], TERMINAL_NODE_NAME,
            "model-level constructs pin the terminal node"
        );
    }

    #[test]
    fn finding_payload_pins_qualified_join_constructs_to_the_consumer_node() {
        // cute-dbt#173 constructs are `left_join[<consumer>:<right>]` —
        // the consumer CTE is the pin target.
        let compiled = "with customers as (select * from src_customers), \
                        orders as (select * from src_orders), \
                        joined as (select orders.id, customers.email from orders \
                        left join customers on orders.customer_id = customers.id) \
                        select * from joined";
        let node = model_node("model.shop.order_emails", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_emails")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        let join = json["findings"]
            .as_array()
            .expect("findings present")
            .iter()
            .find(|f| {
                f["construct"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("left_join["))
            })
            .cloned()
            .expect("a join finding fires on the LEFT JOIN model");
        assert_eq!(
            join["pin_node"], "joined",
            "the qualified construct pins the consumer CTE: {join}"
        );
    }

    #[test]
    fn finding_payload_omits_pin_node_when_the_graph_is_empty() {
        // A `select 1` model has no CTE graph — no pin affordance.
        let mut config = BTreeMap::new();
        config.insert("unique_key".to_owned(), json!("order_id"));
        let node = Node::new(
            NodeId::new("model.shop.flat"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.flat")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        assert!(
            json["findings"][0].get("pin_node").is_none(),
            "empty graph ⇒ pin_node omitted; got {json}"
        );
    }

    #[test]
    fn finding_payload_lifts_suggested_given_sketches_out_of_evidence() {
        // cute-dbt#170 — the union check's `suggested given` evidence
        // entries become the copyable `sketches` array; the remaining
        // evidence list never carries that label.
        let payload = findings_surface_payload();
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        let union = json["findings"]
            .as_array()
            .expect("findings present")
            .iter()
            .find(|f| f["check"] == "union.arm-coverage")
            .cloned()
            .expect("union finding fires");
        let sketches = union["sketches"].as_array().expect("sketches lifted");
        assert!(!sketches.is_empty(), "uncovered arms carry sketches");
        assert!(
            sketches
                .iter()
                .all(|s| s.as_str().is_some_and(|s| s.starts_with("- input: ref('"))),
            "each sketch is the copy-pasteable given-row YAML: {sketches:?}"
        );
        let labels: Vec<&str> = union["evidence"]
            .as_array()
            .expect("evidence stays present")
            .iter()
            .filter_map(|e| e["label"].as_str())
            .collect();
        assert!(
            !labels.contains(&"suggested given"),
            "sketch entries are LIFTED, not duplicated: {labels:?}"
        );
    }

    #[test]
    fn report_payload_carries_the_check_spec_catalog_for_fired_checks() {
        // cute-dbt#170 — the rationale drawer renders offline from
        // `check_specs`; the book link is a plain click-only href.
        let payload = findings_surface_payload();
        let json = serde_json::to_value(&payload).expect("serialize");
        let specs = json["check_specs"]
            .as_object()
            .expect("check_specs present when findings fired");
        assert_eq!(specs.len(), 2, "exactly the fired checks: {specs:?}");
        let grain = &specs["grain.unique-key-unbacked"];
        assert_eq!(grain["tier"], "total");
        assert_eq!(grain["instrument"], "data-test");
        assert!(
            grain["rationale"].as_str().is_some_and(|r| !r.is_empty()),
            "rationale embeds inline (zero-egress)"
        );
        assert!(
            grain["conditions"]
                .as_array()
                .is_some_and(|c| !c.is_empty()),
            "conditions embed inline"
        );
        assert_eq!(
            grain["book_href"],
            "https://breezy-bays-labs.github.io/cute-dbt/checks/grain.unique-key-unbacked.html",
        );
        assert_eq!(
            specs["union.arm-coverage"]["tier"], "high",
            "tier vocabulary rides per check, never blended"
        );
    }

    #[test]
    fn report_payload_omits_check_specs_when_no_finding_fires() {
        // Findings-free payloads (the jaffle-shop golden) stay
        // byte-stable: no `check_specs` key at all.
        let node = model_node("model.shop.plain", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.plain")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        assert!(
            json.get("check_specs").is_none(),
            "no findings ⇒ no check_specs key; got {json}"
        );
    }

    #[test]
    fn build_payload_carries_raw_sql_when_node_has_raw_code() {
        // cute-dbt#47 — verify the per-model payload surfaces `raw_code`
        // verbatim into `raw_sql` so the template's Model SQL section can
        // render it. Jinja content (refs + comments) is preserved.
        let raw = "{# header #}\nselect * from {{ ref('upstream') }}";
        let compiled = "select * from raw_upstream";
        let node = model_node_with_raw("model.shop.stg_x", "body", Some(compiled), Some(raw));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert_eq!(payload.models[0].raw_sql.as_deref(), Some(raw));
    }

    #[test]
    fn build_payload_carries_sql_diff_keyed_by_full_model_id() {
        // cute-dbt#111 — a model whose full node id is in the `sql_diffs`
        // map surfaces `ModelPayload.sql_diff`. Keyed by the FULL id (the
        // `reconstruct_model_sql_diffs` key), not the bare name.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let mut sql_diffs = HashMap::new();
        sql_diffs.insert(
            "model.shop.dim_x".to_owned(),
            BlockDiff {
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    text: "select 1".to_owned(),
                    emphasis: None,
                }],
            },
        );
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &sql_diffs,
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        let sd = payload.models[0]
            .sql_diff
            .as_ref()
            .expect("sql_diff present for the keyed model");
        assert_eq!(sd.lines.len(), 1);
        assert_eq!(sd.lines[0].kind, DiffLineKind::Added);
    }

    #[test]
    fn build_payload_sql_diff_is_none_when_model_not_in_diff_map() {
        // No entry for this model → sql_diff omitted (skip_serializing_if).
        // The baseline path passes an empty map, so baseline reports never
        // carry a sql_diff key — the manifest-digest snapshot is unmoved.
        let node = model_node("model.shop.dim_y", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_y")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].sql_diff.is_none());
        // And it is omitted from the JSON wire (not `"sql_diff":null`).
        let json = serde_json::to_string(&payload.models[0]).expect("serialize");
        assert!(
            !json.contains("sql_diff"),
            "absent sql_diff must be omitted from the wire; got {json}",
        );
    }

    // ----- cute-dbt#247: the Model-YAML section payload -----

    #[test]
    fn build_payload_carries_model_yaml_found_keyed_by_full_model_id() {
        // A Found gather outcome (keyed by the FULL node id, the
        // `gather_model_yaml` key) surfaces the sliced block + the schema
        // file path; no degrade copy.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let raw_block = "  - name: dim_x\n    description: a model";
        let mut model_yaml = HashMap::new();
        model_yaml.insert(
            "model.shop.dim_x".to_owned(),
            crate::domain::ModelYamlOutcome::Found {
                path: "models/schema.yml".to_owned(),
                block: UnitTestYamlBlock::new(raw_block.to_owned(), 2, 2, 3),
                diff: None,
            },
        );
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &model_yaml,
            &HashMap::new(),
            "baseline.json",
        );
        let my = payload.models[0]
            .model_yaml
            .as_ref()
            .expect("model_yaml present for the keyed model");
        assert_eq!(my.path.as_deref(), Some("models/schema.yml"));
        assert_eq!(my.raw.as_deref(), Some(raw_block));
        assert!(my.diff.is_none());
        assert!(my.missing.is_none());
    }

    #[test]
    fn build_payload_model_yaml_carries_the_attached_diff() {
        // A Found outcome with an attached inline diff (the PrDiff arm's
        // `attach_model_yaml_diffs`) rides into the payload — the section's
        // Diff view.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let mut model_yaml = HashMap::new();
        model_yaml.insert(
            "model.shop.dim_x".to_owned(),
            crate::domain::ModelYamlOutcome::Found {
                path: "models/schema.yml".to_owned(),
                block: UnitTestYamlBlock::new("  - name: dim_x".to_owned(), 2, 2, 2),
                diff: Some(BlockDiff {
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Added,
                        text: "    description: new".to_owned(),
                        emphasis: None,
                    }],
                }),
            },
        );
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &model_yaml,
            &HashMap::new(),
            "",
        );
        let my = payload.models[0].model_yaml.as_ref().expect("model_yaml");
        let diff = my.diff.as_ref().expect("attached diff rides the payload");
        assert_eq!(diff.lines[0].kind, DiffLineKind::Added);
    }

    #[test]
    fn build_payload_model_yaml_is_omitted_without_a_gather_entry() {
        // No gather outcome for this model (a direct render / explore
        // path) → key omitted from the wire, the template hides the
        // section; pre-#247 payload shapes stay byte-stable.
        let node = model_node("model.shop.dim_y", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_y")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert!(payload.models[0].model_yaml.is_none());
        let json = serde_json::to_string(&payload.models[0]).expect("serialize");
        assert!(
            !json.contains("model_yaml"),
            "absent model_yaml must be omitted from the wire; got {json}",
        );
    }

    #[test]
    fn model_yaml_payload_degrades_truthfully_per_outcome() {
        // Every degrade arm yields a `missing` copy naming exactly what is
        // absent — the section never renders empty or misleading. The
        // wording lives HERE (Rust computes, JS only renders), so these
        // facts pin it.
        use crate::domain::ModelYamlOutcome as O;

        let no_patch = model_yaml_payload(Some(&O::NoPatchPath), "dim_x").expect("payload");
        assert!(no_patch.raw.is_none() && no_patch.path.is_none());
        let msg = no_patch.missing.expect("degrade copy");
        assert!(
            msg.contains("No schema file declares this model"),
            "names the absence: {msg}"
        );

        let no_root = model_yaml_payload(
            Some(&O::NoProjectRoot {
                path: "models/schema.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        assert_eq!(no_root.path.as_deref(), Some("models/schema.yml"));
        let msg = no_root.missing.expect("degrade copy");
        assert!(msg.contains("models/schema.yml"), "names the file: {msg}");
        assert!(
            msg.contains("--project-root"),
            "names the remediation flag: {msg}"
        );

        let missing_file = model_yaml_payload(
            Some(&O::FileMissing {
                path: "models/gone.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        let msg = missing_file.missing.expect("degrade copy");
        assert!(msg.contains("models/gone.yml"), "names the file: {msg}");
        assert!(msg.contains("not found"), "states the failure: {msg}");

        let unreadable = model_yaml_payload(
            Some(&O::Unreadable {
                path: "models/locked.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        let msg = unreadable.missing.expect("degrade copy");
        assert!(msg.contains("models/locked.yml"), "names the file: {msg}");
        assert!(
            msg.contains("could not be read"),
            "states the failure: {msg}"
        );

        let not_found = model_yaml_payload(
            Some(&O::EntryNotFound {
                path: "models/schema.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        let msg = not_found.missing.expect("degrade copy");
        assert!(msg.contains("dim_x"), "names the model: {msg}");
        assert!(msg.contains("models/schema.yml"), "names the file: {msg}");

        assert!(
            model_yaml_payload(None, "dim_x").is_none(),
            "no gather outcome → no payload (section hidden)"
        );
    }

    #[test]
    fn build_payload_raw_sql_is_none_when_node_has_no_raw_code() {
        // Defensive — older manifests / hand-crafted fixtures lacking
        // `raw_code` produce `raw_sql = None`, which the template handler
        // hides the section for.
        let node = model_node("model.shop.stg_y", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_y")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].raw_sql.is_none());
    }

    #[test]
    fn build_payload_raw_sql_is_none_when_raw_code_is_empty_string() {
        // dbt populates `raw_code: ""` for some node types (e.g. seeds);
        // treat empty string identically to `None` so the template doesn't
        // render an empty Model SQL section.
        let node = model_node_with_raw("model.shop.stg_z", "body", Some("select 1"), Some(""));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_z")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].raw_sql.is_none());
    }

    #[test]
    fn build_payload_emits_empty_tests_for_modified_model_with_no_unit_tests() {
        let compiled = "select 1";
        let node = model_node("model.shop.no_test", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let in_scope = InScopeSet::new();
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.no_test")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].tests.is_empty());
    }

    #[test]
    fn build_payload_skips_a_model_missing_from_manifest_nodes() {
        let manifest = manifest_for(vec![], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.ghost")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_widens_to_all_tests_on_in_scope_model_tagging_changed() {
        // cute-dbt#91 widening (replaces the obsolete in-scope-id-missing-
        // from-manifest test — the indexer is now models-driven, so that
        // defensive path no longer exists). A model in scope carries EVERY
        // unit test targeting it — both the updated (changed) ones and the
        // context (unchanged) siblings — each tagged via the `changed`
        // set. This is the modest render-scope widening that makes the
        // report's All-tests mode + per-model total count work.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let updated = simple_unit_test("dim_x", "test_updated");
        let context = simple_unit_test("dim_x", "test_context");
        let manifest = manifest_for(
            vec![node],
            vec![
                ("unit_test.shop.test_updated", updated),
                ("unit_test.shop.test_context", context),
            ],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        // Only test_updated is changed; test_context is a non-updated
        // sibling carried purely by the widening.
        let changed = InScopeSet::from_iter(["unit_test.shop.test_updated".to_owned()]);
        let payload = build_payload(
            &manifest,
            &changed,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(payload.models.len(), 1);
        let tests = &payload.models[0].tests;
        assert_eq!(
            tests.len(),
            2,
            "both the updated test and its context sibling are carried",
        );
        let updated_p = tests
            .iter()
            .find(|t| t.name == "test_updated")
            .expect("updated test present");
        let context_p = tests
            .iter()
            .find(|t| t.name == "test_context")
            .expect("context sibling present");
        assert!(updated_p.changed, "the updated test is tagged changed");
        assert!(
            !context_p.changed,
            "the context sibling is tagged not-changed"
        );
    }

    #[test]
    fn build_payload_skips_an_in_scope_test_whose_target_model_is_missing() {
        // Defensive `index_in_scope_tests_by_model` path: a unit test
        // exists in the manifest but its `model:` selector resolves to
        // None (target model not in manifest). The indexer skips it.
        let ut = UnitTest::new(
            "test_ghost",
            NodeId::new("ghost_model"),
            vec![],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![], vec![("unit_test.shop.test_ghost", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_ghost".to_owned()]);
        let models = ModelInScopeSet::new();
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        // No models in scope → no payload entries; the indexer's None
        // branch simply doesn't contribute.
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_terminal_node_carries_model_name_as_label_not_id() {
        // cute-dbt#155: the terminal node's *id* is the stable engine name
        // (`TERMINAL_NODE_NAME`); the model's bare name rides as a *display
        // label* only. Keeping the model name out of the id is what stops a
        // self-named import CTE from colliding with the terminal.
        let compiled = "with src as (select * from raw_x) select * from src";
        let node = model_node("model.shop.final_one", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.final_one")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        let terminal = model
            .dag
            .nodes
            .iter()
            .find(|n| n.role == NodeRole::Final)
            .expect("terminal node present");
        assert_eq!(
            terminal.id, TERMINAL_NODE_NAME,
            "terminal id is the stable engine name, not the model name",
        );
        assert_eq!(
            terminal.label.as_deref(),
            Some("final_one.sql"),
            "the terminal's DISPLAY label is the model's `.sql` file name",
        );
    }

    #[test]
    fn build_payload_compiled_sql_keyed_by_node_id() {
        let compiled = "with stg_x_src as (select * from raw_x) select * from stg_x_src";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        assert!(
            model.compiled_sql.contains_key("stg_x_src"),
            "import node compiled SQL keyed by CTE name: {:?}",
            model.compiled_sql.keys().collect::<Vec<_>>(),
        );
        assert!(
            model.compiled_sql.contains_key(TERMINAL_NODE_NAME),
            "terminal node compiled SQL keyed by the stable terminal id (cute-dbt#155)",
        );
    }

    #[test]
    fn self_named_import_cte_does_not_collapse_into_the_terminal() {
        // cute-dbt#155 regression: a model named `orders` whose import CTE
        // is *also* named `orders` (the idiomatic `with orders as (...)`)
        // must render TWO distinct DAG nodes — not collapse into one node
        // with a spurious `orders ↔ final` cycle and the terminal's SQL
        // clobbering the import CTE's body in the compiled_sql map.
        let compiled = "with orders as (select * from raw_orders), \
                              final as (select * from orders) \
                         select * from final";
        let node = model_node("model.shop.orders", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];

        // (1) every node id is unique — the core invariant the bug violated.
        let ids: Vec<&str> = model.dag.nodes.iter().map(|n| n.id.as_str()).collect();
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "node ids must be unique, got {ids:?}"
        );

        // (2) the import CTE and the terminal are SEPARATE nodes with the
        //     correct roles (no Import/Final inversion).
        let import = model
            .dag
            .nodes
            .iter()
            .find(|n| n.id == "orders")
            .expect("import CTE node keyed by its own name");
        assert_eq!(
            import.role,
            NodeRole::Import,
            "import keeps the Import role"
        );
        let terminal = model
            .dag
            .nodes
            .iter()
            .find(|n| n.role == NodeRole::Final)
            .expect("terminal node present");
        assert_ne!(
            terminal.id, "orders",
            "terminal id must NOT collide with the import CTE",
        );
        assert_eq!(
            terminal.label.as_deref(),
            Some("orders.sql"),
            "terminal DISPLAYS the model's `.sql` file name — visually distinct \
             from the same-named import CTE",
        );

        // (3) no self-cycle: no edge whose endpoints are the same id, and
        //     specifically not the spurious `final -> orders` back-edge.
        assert!(
            model.dag.edges.iter().all(|e| e.from != e.to),
            "no edge may point a node at itself: {:?}",
            model
                .dag
                .edges
                .iter()
                .map(|e| (&e.from, &e.to))
                .collect::<Vec<_>>(),
        );
        assert!(
            !model
                .dag
                .edges
                .iter()
                .any(|e| e.from == "final" && e.to == "orders"),
            "the spurious final->orders back-edge must be gone",
        );

        // (4) the import node shows ITS OWN body, not the terminal's.
        let import_sql = &model.compiled_sql["orders"];
        assert!(
            import_sql.contains("raw_orders"),
            "import node keeps its own SQL: {import_sql}",
        );
        assert!(
            !import_sql.contains("from final"),
            "import node SQL must not be overwritten by the terminal's: {import_sql}",
        );
    }

    #[test]
    fn build_payload_empty_graph_falls_back_to_full_compiled_code() {
        // A `select 1` body has no WITH clause → empty CteGraph. The
        // compiled_sql map still carries the model's body keyed by the
        // bare name so the renderer surfaces SOMETHING.
        let compiled = "select 1";
        let node = model_node("model.shop.flat", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.flat")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(
            payload.models[0].compiled_sql.get("flat").unwrap(),
            compiled
        );
    }

    #[test]
    fn build_payload_handles_unparseable_compiled_code_gracefully() {
        // The engine returns CteError::Parse for garbage SQL. The renderer
        // treats that as an empty graph, NOT a hard failure — the report
        // still ships, the model card just has no DAG.
        let node = model_node("model.shop.broken", "body", Some("not valid sql {"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.broken")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(payload.models.len(), 1);
        // Empty graph → empty nodes/edges, but compiled_sql carries the
        // original body keyed by bare name.
        assert!(payload.models[0].dag.nodes.is_empty());
        assert_eq!(
            payload.models[0].compiled_sql.get("broken").unwrap(),
            "not valid sql {",
        );
    }

    #[test]
    fn build_payload_test_carries_metadata_fields() {
        let compiled = "select 1";
        let node = model_node("model.shop.m", "body", Some(compiled));
        let ut = simple_unit_test("m", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(test.description.as_deref(), Some("a description"));
        assert_eq!(test.tags.as_ref().unwrap(), &vec!["finance".to_owned()]);
        assert!(test.meta.is_some());
        assert_eq!(test.defined_in.as_deref(), Some("models/x.yml"));
    }

    #[test]
    fn build_payload_given_binds_import_cte_when_ref_matches() {
        let compiled = "with stg_orders_src as (select * from raw_orders) \
                        select * from stg_orders_src";
        let node = model_node("model.shop.stg_orders", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("stg_orders"),
            vec![UnitTestGiven::new(
                "ref('stg_orders_src')",
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("stg_orders_src"),
            "ref('stg_orders_src') binds to its matching import-CTE node",
        );
    }

    #[test]
    fn build_payload_given_binds_import_cte_via_body_table_reference() {
        // The dbt-idiomatic shape: import CTE is named `source` (the
        // unwrapper convention), but its body references the model the
        // unit test mocks. The renderer's pass-2 body match must catch
        // this — pass-1 name match misses it.
        let compiled = "with source as (\
                          select * from \"jaffle_shop\".\"main\".\"raw_customers\"\
                        ) select customer_id, first_name from source";
        let node = model_node("model.shop.stg_customers", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("stg_customers"),
            vec![UnitTestGiven::new(
                "ref('raw_customers')",
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_customers")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("source"),
            "ref('raw_customers') binds to the import-CTE `source` via its body table reference",
        );
    }

    #[test]
    fn build_payload_messy_multi_ref_cte_binds_every_given_to_one_node() {
        // cute-dbt#34 medium scope: a single CTE whose body references
        // MULTIPLE `ref()` targets (here via UNION ALL) must surface ALL
        // matching unit-test givens, not just the first. The engine
        // classifies this body as Transform (multi-source, not the
        // import-CTE shape) but populates body_leaf_table_refs with both
        // leaves; pass-2 binds against any leaf node with those refs.
        let compiled = "with raw_union as (\
                          select id, kind from \"db\".\"schema\".\"raw_orders\" \
                          union all \
                          select id, kind from \"db\".\"schema\".\"raw_returns\"\
                        ) select * from raw_union";
        let node = model_node("model.shop.union_model", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("union_model"),
            vec![
                UnitTestGiven::new("ref('raw_orders')", json!([{"id": 1}]), None, None),
                UnitTestGiven::new("ref('raw_returns')", json!([{"id": 9}]), None, None),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.union_model")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("raw_union"),
            "ref('raw_orders') binds to the multi-ref CTE via body-leaf match; \
             got bound_to_node={:?}",
            test.given[0].bound_to_node,
        );
        assert_eq!(
            test.given[1].bound_to_node.as_deref(),
            Some("raw_union"),
            "ref('raw_returns') binds to the SAME multi-ref CTE; \
             got bound_to_node={:?}",
            test.given[1].bound_to_node,
        );
    }

    #[test]
    fn build_payload_messy_join_cte_binds_both_givens() {
        // Variant of the multi-ref case where the messy CTE body is a
        // single SELECT with a JOIN — also not the import shape, also
        // populates body_leaf_table_refs with both joined leaves.
        let compiled = "with joined_src as (\
                          select o.id, c.name \
                          from \"db\".\"schema\".\"raw_orders\" o \
                          inner join \"db\".\"schema\".\"raw_customers\" c on c.id = o.cid\
                        ) select * from joined_src";
        let node = model_node("model.shop.join_model", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("join_model"),
            vec![
                UnitTestGiven::new("ref('raw_orders')", json!([]), None, None),
                UnitTestGiven::new("ref('raw_customers')", json!([]), None, None),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.join_model")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(test.given[0].bound_to_node.as_deref(), Some("joined_src"));
        assert_eq!(test.given[1].bound_to_node.as_deref(), Some("joined_src"));
    }

    #[test]
    fn build_payload_pass_1_requires_both_name_match_and_import_role() {
        // Pass-1 matching is `name == target AND role == Import`. If a
        // graph contains an import CTE with the WRONG name and a
        // transform CTE with the RIGHT name, neither pass-1 candidate
        // should bind: the import is wrong-named, and the transform's
        // role disqualifies it even though the name matches. With the
        // role check loosened (e.g. `||` instead of `&&`) the wrong-name
        // import would spuriously bind.
        //
        // Body match must also fail here so the test isolates pass-1:
        // the import's raw_sql references an unrelated table, and the
        // transform CTE's body refers only to other CTEs.
        let compiled = "with not_target as (select * from \"db\".\"schema\".\"unrelated\"), \
             target as (select * from not_target) \
             select * from target";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("x"),
            vec![UnitTestGiven::new("ref('target')", json!([]), None, None)],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert!(
            test.given[0].bound_to_node.is_none(),
            "matching name on a non-import node must NOT bind (role gate honored); \
             got bound_to_node={:?}",
            test.given[0].bound_to_node,
        );
    }

    #[test]
    fn build_payload_given_does_not_bind_when_no_matching_import_cte() {
        let compiled = "select 1";
        let node = model_node("model.shop.flat", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("flat"),
            vec![UnitTestGiven::new(
                "ref('nonexistent')",
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.flat")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert!(test.given[0].bound_to_node.is_none());
    }

    // ===== source() given binding (cute-dbt#57) =====

    /// The canonical staging-model shape: `with source as (select * from
    /// {{ source('synthea_raw', 'patients') }})` compiles to a quoted
    /// three-part relation inside an import CTE named `source`.
    fn patients_source_model() -> Node {
        let compiled = "with source as (\
                          select * from \"memory\".\"main\".\"patients\"\
                        ) select id, name from source";
        model_node("model.shop.stg_patients", "body", Some(compiled))
    }

    fn source_given_unit_test(input: &str) -> UnitTest {
        UnitTest::new(
            "test_one",
            NodeId::new("stg_patients"),
            vec![UnitTestGiven::new(input, json!([]), None, None)],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    fn bound_node_for(manifest: &Manifest) -> Option<String> {
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_patients")]);
        let payload = build_payload(
            manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        payload.models[0].tests[0].given[0].bound_to_node.clone()
    }

    #[test]
    fn build_payload_source_given_binds_via_resolved_relation_body_match() {
        // The cute-dbt#57 vertical: `source('synthea_raw','patients')`
        // resolves through the manifest sources map to the physical
        // identifier `patients`, which pass-2 finds in the `source`
        // CTE's body-leaf refs (the compiled relation and the manifest
        // relation_name render from the same relation object).
        let manifest = manifest_with_sources(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients".to_owned()),
                "main",
                Some("memory".to_owned()),
                Some("\"memory\".\"main\".\"patients\"".to_owned()),
            )],
        );
        assert_eq!(
            bound_node_for(&manifest).as_deref(),
            Some("source"),
            "source('synthea_raw','patients') binds to the import CTE via the resolved relation",
        );
    }

    #[test]
    fn build_payload_source_given_binds_through_an_identifier_override() {
        // The YAML `name` and the physical `identifier` differ — the
        // lookup must run on (source_name, name) while the body match
        // must run on `identifier`.
        let compiled = "with src as (\
                          select * from \"memory\".\"main\".\"patients_v2\"\
                        ) select id from src";
        let node = model_node("model.shop.stg_patients", "body", Some(compiled));
        let manifest = manifest_with_sources(
            vec![node],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients_v2".to_owned()),
                "main",
                Some("memory".to_owned()),
                Some("\"memory\".\"main\".\"patients_v2\"".to_owned()),
            )],
        );
        assert_eq!(bound_node_for(&manifest).as_deref(), Some("src"));
    }

    #[test]
    fn build_payload_source_given_binds_with_a_fusion_minimal_source_entry() {
        // Fusion-style entry: identifier and relation_name keys absent.
        // The token falls back to the source's `name` (dbt's identifier
        // default), which still matches the compiled body leaf.
        let manifest = manifest_with_sources(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                None,
                "main",
                None,
                None,
            )],
        );
        assert_eq!(bound_node_for(&manifest).as_deref(), Some("source"));
    }

    #[test]
    fn build_payload_source_given_does_not_bind_when_pair_is_unresolvable() {
        // No matching (source_name, name) in the sources map — the given
        // stays unbound and the node-detail panel keeps its empty-state
        // copy (fail-open, no PreflightError; sources need no preflight).
        let manifest = manifest_with_sources(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('other_block', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients".to_owned()),
                "main",
                Some("memory".to_owned()),
                Some("\"memory\".\"main\".\"patients\"".to_owned()),
            )],
        );
        assert!(bound_node_for(&manifest).is_none());
    }

    #[test]
    fn build_payload_source_given_does_not_bind_without_a_sources_map() {
        // Pre-#57 shape: the manifest carries no sources block at all.
        let manifest = manifest_for(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
        );
        assert!(bound_node_for(&manifest).is_none());
    }

    #[test]
    fn build_payload_binds_a_ref_given_and_a_source_given_in_one_test() {
        // The cute-dbt#57 AC shape: one model with BOTH a ref()-based
        // import CTE and a source()-based import CTE; the unit test
        // mocks both inputs and each given binds to its own CTE node.
        let compiled = "with orders as (\
                          select * from \"memory\".\"main\".\"raw_orders\"\
                        ), patients as (\
                          select * from \"memory\".\"raw\".\"patients\"\
                        ) select o.id, p.name from orders o join patients p on p.id = o.pid";
        let node = model_node("model.shop.mixed_inputs", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("mixed_inputs"),
            vec![
                UnitTestGiven::new("ref('raw_orders')", json!([{"id": 1}]), None, None),
                UnitTestGiven::new(
                    "source('synthea_raw', 'patients')",
                    json!([{"id": 1, "name": "x"}]),
                    None,
                    None,
                ),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_with_sources(
            vec![node],
            vec![("unit_test.shop.test_one", ut)],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients".to_owned()),
                "raw",
                Some("memory".to_owned()),
                Some("\"memory\".\"raw\".\"patients\"".to_owned()),
            )],
        );
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.mixed_inputs")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("orders"),
            "the ref() given binds to the ref-based import CTE",
        );
        assert_eq!(
            test.given[1].bound_to_node.as_deref(),
            Some("patients"),
            "the source() given binds to the source-based import CTE",
        );
    }

    #[test]
    fn build_payload_propagates_is_recursive_flag() {
        // A WITH RECURSIVE query: the engine flags the graph; the
        // renderer threads that to the model payload so the template can
        // surface a "recursive arm omitted" banner.
        let compiled = "with recursive r(n) as (select 1 union all select n+1 from r) \
                        select * from r";
        let node = model_node("model.shop.rec", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.rec")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(payload.models[0].is_recursive);
    }

    // ===== cute-dbt#145: incremental-model unit-test semantics =====

    /// Build a model node whose `config.materialized == "incremental"`.
    fn incremental_model_node(id: &str, compiled: Option<&str>) -> Node {
        let mut cfg = BTreeMap::new();
        cfg.insert("materialized".to_owned(), json!("incremental"));
        Node::new(
            NodeId::new(id),
            "model",
            checksum("body"),
            compiled.map(str::to_owned),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(cfg, false),
            None,
            BTreeMap::new(),
        )
    }

    #[test]
    fn build_payload_marks_incremental_model_and_not_table() {
        // Incremental model ⇒ is_incremental true.
        let inc = incremental_model_node("model.shop.order_events", Some("select 1"));
        let manifest = manifest_for(vec![inc], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_events")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(
            payload.models[0].is_incremental,
            "config.materialized==incremental ⇒ ModelPayload.is_incremental"
        );

        // Table model (NodeConfig::default — no materialized key) ⇒ false.
        let tbl = model_node("model.shop.orders", "body", Some("select 1"));
        let manifest = manifest_for(vec![tbl], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(
            !payload.models[0].is_incremental,
            "table model ⇒ not incremental"
        );
    }

    #[test]
    fn build_payload_threads_incremental_mode_and_this_given() {
        let node = incremental_model_node("model.shop.order_events", Some("select 1"));
        let ut = UnitTest::new(
            "test_order_events_incremental",
            NodeId::new("order_events"),
            vec![sample_given("this"), sample_given("ref('stg_orders')")],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_incremental_mode(Some(true));
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.t", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_events")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.is_incremental_mode,
            Some(true),
            "overrides mode threaded onto TestPayload"
        );
        assert!(test.given[0].is_this, "input 'this' ⇒ is_this");
        assert!(!test.given[1].is_this, "ref(...) given ⇒ not is_this");
    }

    /// The Rust→JS key contract (advisor-flagged blind spot): the template's
    /// JS reads `m.is_incremental`, `t.is_incremental_mode`, and
    /// `given.is_this` by those EXACT `snake_case` keys — no `#[serde(rename)]`.
    /// An incremental-mode test's payload emits all three; a plain table/ref
    /// payload OMITS them (`skip_serializing_if`), so non-incremental
    /// fixtures stay byte-identical on the wire.
    #[test]
    fn payload_serde_wire_shape_for_incremental_keys() {
        // Incremental model + incremental-mode test + `this` given ⇒ keys present.
        let node = incremental_model_node("model.shop.order_events", Some("select 1"));
        let ut = UnitTest::new(
            "t",
            NodeId::new("order_events"),
            vec![sample_given("this")],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_incremental_mode(Some(true));
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.t", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_events")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0]).unwrap();
        assert!(
            json.contains("\"is_incremental\":true"),
            "model key: {json}"
        );
        assert!(
            json.contains("\"is_incremental_mode\":true"),
            "test key: {json}"
        );
        assert!(json.contains("\"is_this\":true"), "given key: {json}");

        // Non-incremental: table model + ref given + no override ⇒ all three OMITTED.
        let tbl = model_node("model.shop.orders", "body", Some("select 1"));
        let ut2 = UnitTest::new(
            "t2",
            NodeId::new("orders"),
            vec![sample_given("ref('stg_orders')")],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        ); // no with_incremental_mode ⇒ None
        let manifest = manifest_for(vec![tbl], vec![("unit_test.shop.t2", ut2)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t2".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0]).unwrap();
        // "is_incremental" is a substring of "is_incremental_mode", so its
        // absence guards both model + test keys at once.
        assert!(
            !json.contains("is_incremental"),
            "non-incremental model omits is_incremental + is_incremental_mode: {json}"
        );
        assert!(
            !json.contains("is_this"),
            "ref(...) given omits is_this: {json}"
        );
    }

    // ===== payload_json_for_html_script =====

    #[test]
    fn payload_json_escapes_closing_script_tag_via_unicode() {
        let payload = ReportPayload {
            baseline: "</script><script>alert(1)</script>".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("</script>"),
            "no raw </script> survives in: {serialized}",
        );
        assert!(
            serialized.contains("\\u003c/script>"),
            "`</` is replaced with `\\u003c/` in: {serialized}",
        );
    }

    #[test]
    fn payload_json_escapes_html_comment_open_via_unicode() {
        let payload = ReportPayload {
            baseline: "x<!--hostile-->y".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("<!--"),
            "no raw <!-- survives in: {serialized}",
        );
        assert!(
            serialized.contains("\\u003c!--"),
            "`<!` is replaced with `\\u003c!` in: {serialized}",
        );
    }

    #[test]
    fn payload_json_leaves_bare_left_angle_alone() {
        // Only `</` and `<!` are dangerous in HTML5 script-data state;
        // a bare `<` followed by a space or other char is fine.
        let payload = ReportPayload {
            baseline: "a < b".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(serialized.contains("a < b"), "bare `<` is preserved");
    }

    #[test]
    fn payload_json_escapes_le_comparisons_for_sloppy_scanners() {
        // cute-dbt#200 — authored model/column descriptions carry SQL-ish
        // prose like `encounter_start_at <= current_timestamp` (the
        // committed playground fixture); `tl`-based extractors mis-scan a
        // raw `<=`, so it joins the #170 escape set. Round-trips intact.
        let payload = ReportPayload {
            baseline: "encounter_start_at <= current_timestamp".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("<="),
            "no raw <= survives: {serialized}"
        );
        assert!(
            serialized.contains("\\u003c="),
            "`<=` escapes to `\\u003c=`: {serialized}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("escaped output is valid JSON");
        assert_eq!(
            parsed["baseline"],
            serde_json::Value::String("encounter_start_at <= current_timestamp".to_owned()),
        );
    }

    #[test]
    fn payload_json_escapes_tag_like_angle_brackets() {
        // cute-dbt#170 — check-spec prose like `WHERE <right>.<key> IS
        // NULL` rides the payload now; `<letter` shapes must not read as
        // markup to tag scanners (the tl-based test extractors choked on
        // them), while staying JSON-decodable to the original text.
        let payload = ReportPayload {
            baseline: "filters WHERE <right>.<key> IS NULL <?".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("<right>") && !serialized.contains("<key>"),
            "no raw tag-like sequence survives: {serialized}"
        );
        assert!(
            serialized.contains("\\u003cright>") && serialized.contains("\\u003c?"),
            "tag openers escape to \\u003c: {serialized}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("escaped output is valid JSON");
        assert_eq!(
            parsed["baseline"],
            serde_json::Value::String("filters WHERE <right>.<key> IS NULL <?".to_owned()),
        );
    }

    #[test]
    fn payload_json_output_is_round_trippable_through_json_parse() {
        // The Unicode escape must remain valid JSON; serde_json round-trips
        // it back to the original string.
        let original = ReportPayload {
            baseline: "</script><!--end".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
        };
        let serialized = payload_json_for_html_script(&original).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("escaped output is valid JSON");
        assert_eq!(
            parsed["baseline"],
            serde_json::Value::String("</script><!--end".to_owned()),
            "round-trip recovers the original baseline value",
        );
    }

    // ===== render_report: report_title + report_subtitle threading =====

    #[test]
    fn render_report_default_title_renders_into_title_and_h1() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_title_default_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("<title>cute-dbt report</title>"),
            "default title in <title>: {}",
            html.lines()
                .find(|l| l.contains("<title>"))
                .unwrap_or("<not found>"),
        );
        assert!(
            html.contains("<h1>cute-dbt report</h1>"),
            "default title in <h1>: {}",
            html.lines()
                .find(|l| l.contains("<h1>"))
                .unwrap_or("<not found>"),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_custom_title_overrides_both_surfaces() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_title_custom_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            "Q3 unit test review",
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("<title>Q3 unit test review</title>"),
            "custom title in <title>: {}",
            html.lines()
                .find(|l| l.contains("<title>"))
                .unwrap_or("<not found>"),
        );
        assert!(
            html.contains("<h1>Q3 unit test review</h1>"),
            "custom title in <h1>: {}",
            html.lines()
                .find(|l| l.contains("<h1>"))
                .unwrap_or("<not found>"),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_absent_subtitle_omits_the_subtitle_element() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_no_subtitle_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            !html.contains("class=\"report-subtitle\""),
            "subtitle element omitted when None"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_present_subtitle_renders_the_subtitle_element() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_with_subtitle_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            "Q3 review",
            Some("PR 1234 / staging diff"),
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("<p class=\"report-subtitle\">PR 1234 / staging diff</p>"),
            "subtitle element rendered with text: {}",
            html.lines()
                .find(|l| l.contains("report-subtitle"))
                .unwrap_or("<not found>"),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_xss_in_title_is_html_escaped() {
        // askama's `html` escape filter (template default) prevents a
        // hostile title containing `<script>` from breaking out of the
        // <title> / <h1> text nodes.
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_xss_title_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            "<script>alert(1)</script>",
            None,
        )
        .expect("render writes");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        // The escaped form appears; the raw form does NOT appear in the
        // chrome (it may appear inside inlined script bodies — strip
        // those first, mirroring the egress test pattern).
        let mut chrome = html.clone();
        for asset in [
            SAKURA_CSS,
            DATATABLES_CSS,
            JQUERY_JS,
            DATATABLES_JS,
            MERMAID_JS,
        ] {
            chrome = chrome.replace(asset, "<<inlined-asset>>");
        }
        assert!(
            !chrome.contains("<script>alert(1)</script>"),
            "raw <script> never appears in the chrome"
        );
        // askama's html escape may use entity names (&lt;) or numeric
        // (&#60; / &#x3c;); accept any escaped form. The title literal
        // `alert(1)` must still appear (escapes leave text alone) so
        // the title is reachable in the rendered output.
        assert!(
            chrome.contains("alert(1)"),
            "escaped title still carries its text payload"
        );
        let has_escaped_lt =
            chrome.contains("&lt;") || chrome.contains("&#60;") || chrome.contains("&#x3c;");
        assert!(
            has_escaped_lt,
            "some escaped < entity appears in the chrome (askama html filter)"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== render_report: diff-scope banner provenance (cute-dbt#85) =====

    #[test]
    fn render_report_baseline_banner_names_the_baseline_manifest() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_banner_baseline_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("vs baseline manifest"),
            "baseline banner names the baseline manifest",
        );
        assert!(
            !html.contains("from PR file diff"),
            "baseline banner does not claim a PR-diff provenance",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_pr_diff_banner_omits_the_baseline_manifest_clause() {
        // On the PR-diff path there is no baseline manifest — rendering
        // "vs baseline manifest …" would be a false statement (cute-dbt#85).
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_banner_pr_diff_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            ScopeSource::PrDiff,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            !html.contains("vs baseline manifest"),
            "PR-diff banner must NOT name a baseline manifest",
        );
        assert!(
            html.contains("from PR file diff"),
            "PR-diff banner states its provenance",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== render_report end-to-end =====

    #[test]
    fn render_report_writes_valid_html_with_inlined_assets() {
        let compiled = "with src as (select * from raw_x) select * from src";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(html.starts_with("<!DOCTYPE html>") || html.starts_with("<!doctype html>"));
        assert!(html.contains(SAKURA_CSS), "Sakura inlined");
        assert!(html.contains(JQUERY_JS), "jQuery inlined");
        assert!(html.contains(DATATABLES_JS), "DataTables JS inlined");
        assert!(html.contains(DATATABLES_CSS), "DataTables CSS inlined");
        assert!(html.contains(MERMAID_JS), "Mermaid inlined");
        assert!(
            html.contains("href=\"data:,\""),
            "favicon is data: URI; got: {}",
            html.lines().find(|l| l.contains("favicon")).unwrap_or(""),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_inlines_payload_as_application_json_script() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_json_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "lab1@aaaaaaa",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes");
        let html = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            html.contains("id=\"cute-dbt-data\""),
            "json blob carrier present",
        );
        assert!(
            html.contains("lab1@aaaaaaa"),
            "baseline label visible in HTML",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_does_not_emit_external_resource_constructs() {
        // Local belt-and-braces guard for the zero-egress invariant.
        // The canonical proof is the structured resource-ref lint job
        // plus the headless-browser network-block test tracked at
        // `breezy-bays-labs/cute-dbt#12`; this test is the fast local
        // signal that runs on every `cargo test` until that lands.
        //
        // Patterns cover the loading constructs the structured lint
        // will reject: `<script src>`, `<link href>`, `<img src>`,
        // CSS `@import`, CSS `url(`, protocol-relative `//`, and bare
        // `http://` / `https://`. The chrome is measured AFTER
        // stripping the six inlined vendored asset bodies so we don't
        // false-positive on the bundles' inert URL literals.
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_egress_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes");
        let mut chrome = std::fs::read_to_string(&tmp).unwrap();
        for asset in [
            SAKURA_CSS,
            DATATABLES_CSS,
            JQUERY_JS,
            DATATABLES_JS,
            MERMAID_JS,
            CYTOSCAPE_JS,
        ] {
            chrome = chrome.replace(asset, "<<inlined-asset>>");
        }
        assert!(!chrome.contains("<script src"), "no <script src> in chrome");
        assert!(!chrome.contains("<link href"), "no <link href> in chrome");
        assert!(
            !chrome.contains("<img"),
            "no <img> in chrome (we emit no images)",
        );
        assert!(!chrome.contains(" src=\""), "no src= attribute in chrome");
        assert!(!chrome.contains("@import"), "no CSS @import in chrome");
        assert!(!chrome.contains("url("), "no CSS url() in chrome");
        assert!(!chrome.contains("http://"), "no http URL in chrome");
        assert!(!chrome.contains("https://"), "no https URL in chrome");
        assert!(!chrome.contains("\"//"), "no protocol-relative reference");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_xss_payload_in_unit_test_tag_does_not_escape_script_block() {
        // A hostile string in a unit-test's `tags` (which is verbatim YAML
        // metadata in the dbt manifest — no SQL parser sits between it
        // and the payload) must NOT break out of the JSON
        // `<script type="application/json">` carrier. askama's `| json`
        // filter escapes `<`, `>`, `&`, and `'`; this test pins that the
        // payload-side injection is structurally prevented.
        let hostile = "</script><script>alert(1)</script>";
        let compiled = "select 1";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("x"),
            vec![],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            Some(vec![hostile.to_owned()]),
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_xss_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &in_scope,
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        // `</script>` legitimately appears inside the inlined asset
        // bodies; strip those before scanning the chrome.
        let mut chrome = html.clone();
        for asset in [
            SAKURA_CSS,
            DATATABLES_CSS,
            JQUERY_JS,
            DATATABLES_JS,
            MERMAID_JS,
        ] {
            chrome = chrome.replace(asset, "<<inlined-asset>>");
        }
        assert!(
            !chrome.contains("<script>alert(1)</script>"),
            "hostile script tag must not survive into the chrome: {chrome}",
        );
        // The payload carrier is exactly one `<script type="application/json">`
        // …`</script>` block; no second `</script>` smuggled in by the
        // hostile tag value can close it early.
        let payload_open = "<script type=\"application/json\" id=\"cute-dbt-data\">";
        let payload_count = chrome.matches(payload_open).count();
        assert_eq!(payload_count, 1, "exactly one payload carrier open tag");
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== leaf_segment =====

    #[test]
    fn leaf_segment_strips_qualifying_prefix() {
        assert_eq!(leaf_segment("model.shop.x"), "x");
        assert_eq!(leaf_segment("x"), "x");
        assert_eq!(leaf_segment(""), "");
    }

    // ===== cute-dbt#98 — data_diff + fixture wire shape =====

    use crate::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind, UnitTestDataDiff,
    };

    /// A `TestPayload` carrying a `data_diff` serializes the cell-diff with the
    /// EXACT JS contract the renderer will branch on (cute-dbt#138): each cell
    /// side is a `Cell` carrying BOTH `display` (the authored token) AND `key`
    /// (an adjacently-tagged `{"t": <type>, "v": <value>}` `CellValue`). The
    /// `Absent` key is `{"t":"absent"}` (no `"v"`), and the row/column enums
    /// are lowercase tokens. This pins the dual-axis wire shape independently
    /// of the domain's own round-trip test (which proves Rust↔Rust; this proves
    /// the JSON the template consumes) — cute-dbt#139 reads both axes here.
    #[test]
    fn data_diff_payload_wire_ships_both_display_and_key_axes() {
        let data_diff = UnitTestDataDiff {
            given: vec![NamedTableDiff {
                ordinal: 0,
                input: "ref('a')".into(),
                diff: FixtureTableDiff {
                    columns: vec![
                        DiffColumn {
                            name: "id".into(),
                            status: ColumnStatus::Present,
                        },
                        DiffColumn {
                            name: "city".into(),
                            status: ColumnStatus::Added,
                        },
                    ],
                    rows: vec![RowChange {
                        kind: RowChangeKind::Modified,
                        cells: vec![
                            // A format-only cell: authored "1.00" on the NEW
                            // side, "1" on the OLD side, but BOTH key to
                            // Number("1") → changed: false (the headline #138
                            // case: the diff shows the authored value yet does
                            // NOT flag).
                            CellChange {
                                old: Cell::with_display("1".into(), CellValue::Number("1".into())),
                                new: Cell::with_display(
                                    "1.00".into(),
                                    CellValue::Number("1".into()),
                                ),
                                changed: false,
                            },
                            CellChange {
                                old: Cell::new(CellValue::Absent),
                                new: Cell::new(CellValue::Str("NYC".into())),
                                changed: true,
                            },
                        ],
                    }],
                },
            }],
            expect: None,
        };
        let json = serde_json::to_value(&data_diff).expect("data_diff serializes");
        let cells = &json["given"][0]["diff"]["rows"][0]["cells"];
        // BOTH axes ship: `display` is the authored token; `key` is the
        // adjacently-tagged CellValue {"t":"number","v":"1"}.
        assert_eq!(
            cells[0]["new"]["display"], "1.00",
            "authored token survives"
        );
        assert_eq!(cells[0]["new"]["key"]["t"], "number");
        assert_eq!(cells[0]["new"]["key"]["v"], "1", "key is canonical");
        assert_eq!(cells[0]["old"]["display"], "1");
        assert_eq!(cells[0]["old"]["key"]["v"], "1");
        // A format-only change is NOT flagged (keys equal) yet shows the
        // authored NEW display.
        assert_eq!(cells[0]["changed"], false);
        // Str cell: display mirrors the key string verbatim.
        assert_eq!(cells[1]["new"]["display"], "NYC");
        assert_eq!(cells[1]["new"]["key"]["t"], "str");
        assert_eq!(cells[1]["new"]["key"]["v"], "NYC");
        // Absent key is a bare unit variant — tag only, NO "v" key; its
        // display is the empty string.
        assert_eq!(cells[1]["old"]["key"]["t"], "absent");
        assert!(
            cells[1]["old"]["key"].get("v").is_none(),
            "Absent key serializes with no \"v\" key; got {}",
            cells[1]["old"]["key"],
        );
        assert_eq!(cells[1]["old"]["display"], "");
        // Row-kind + column-status enums are lowercase tokens.
        assert_eq!(json["given"][0]["diff"]["rows"][0]["kind"], "modified");
        assert_eq!(json["given"][0]["diff"]["columns"][0]["status"], "present");
        assert_eq!(json["given"][0]["diff"]["columns"][1]["status"], "added");
        // The precomputed `changed` verdict rides on each cell.
        assert_eq!(cells[1]["changed"], true);
    }

    /// `build_test_payload` sets `data_diff` from the threaded map; the field
    /// appears on the wire only when present (`skip_serializing_if` mirror of
    /// `yaml_diff` / `sql_diff`). Absent → the JSON omits the key entirely,
    /// keeping baseline-mode reports byte-stable.
    #[test]
    fn build_test_payload_omits_data_diff_when_absent() {
        let ut = simple_unit_test("m", "t");
        let graph = CteGraph::default();
        let changed = InScopeSet::new();
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &graph,
            &changed,
            None,
            None,
            None,
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        assert!(payload.data_diff.is_none());
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            !json.contains("data_diff"),
            "absent data_diff must be omitted from the wire; got {json}",
        );
    }

    /// When a `data_diff` is threaded, `build_test_payload` carries it and the
    /// wire key appears.
    #[test]
    fn build_test_payload_carries_data_diff_when_present() {
        let ut = simple_unit_test("m", "t");
        let graph = CteGraph::default();
        let changed = InScopeSet::new();
        let data_diff = UnitTestDataDiff {
            given: vec![],
            expect: Some(FixtureTableDiff {
                columns: vec![DiffColumn {
                    name: "id".into(),
                    status: ColumnStatus::Present,
                }],
                rows: vec![RowChange {
                    kind: RowChangeKind::Added,
                    cells: vec![CellChange {
                        old: Cell::new(CellValue::Absent),
                        new: Cell::new(CellValue::Number("9".into())),
                        changed: true,
                    }],
                }],
            }),
        };
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &graph,
            &changed,
            None,
            None,
            Some(&data_diff),
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        assert_eq!(payload.data_diff.as_ref(), Some(&data_diff));
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            json.contains("data_diff"),
            "present data_diff is on the wire"
        );
    }

    // ===== cute-dbt#165 — column-header metadata =====

    /// A column-scoped generic-test node attached to `model_id`.
    fn column_test_node(id: &str, model_id: &str, column: &str, tm: TestMetadata) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            checksum("t"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            Some(column.to_owned()),
            Some(NodeId::new(model_id)),
            Some(tm),
        )
    }

    /// Shorthand: a name-only `ColumnTestPayload`.
    fn bare_test(name: &str) -> ColumnTestPayload {
        ColumnTestPayload {
            name: name.to_owned(),
            ..ColumnTestPayload::default()
        }
    }

    #[test]
    fn column_test_payload_maps_names_and_accepted_values() {
        // cute-dbt#178 — the handoff README §2.2 display-string mapping.
        // Built-ins, bare prose names.
        assert_eq!(
            column_test_payload(&TestMetadata::new("unique", None, Value::Null)),
            bare_test("unique")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new("not_null", None, Value::Null)),
            bare_test("not null")
        );
        // Unknown package test → package-qualified RAW identifier, no
        // values/detail (open-ended arg vocabularies stay uninterpreted).
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "expect_column_values_to_be_between",
                Some("dbt_expectations".to_owned()),
                json!({ "min_value": 0 }),
            )),
            bare_test("dbt_expectations.expect_column_values_to_be_between")
        );
        // accepted_values → the authored values list as distinct chips.
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_values",
                None,
                json!({ "values": ["placed", "shipped", 3] }),
            )),
            ColumnTestPayload {
                name: "accepted values".to_owned(),
                values: vec!["placed".to_owned(), "shipped".to_owned(), "3".to_owned()],
                detail: None,
            }
        );
        // accepted_values with no / empty values degrades to the bare name.
        assert_eq!(
            column_test_payload(&TestMetadata::new("accepted_values", None, Value::Null)),
            bare_test("accepted values")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_values",
                None,
                json!({ "values": [] })
            )),
            bare_test("accepted values")
        );
    }

    #[test]
    fn column_test_payload_maps_relationships_and_accepted_range_details() {
        // cute-dbt#178 — the README §2.2 detail mappings.
        // relationships → "model.field" detail (ref('…') unwrapped).
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "relationships",
                None,
                json!({ "to": "ref('customers')", "field": "customer_id" }),
            )),
            ColumnTestPayload {
                name: "relationships".to_owned(),
                values: Vec::new(),
                detail: Some("customers.customer_id".to_owned()),
            }
        );
        // relationships with a missing field still names the target; a
        // source('…','…') unwraps to its LAST quoted argument.
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "relationships",
                None,
                json!({ "to": "source('raw', 'customers')" }),
            ))
            .detail
            .as_deref(),
            Some("customers")
        );
        // a non-call `to` target renders verbatim.
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "relationships",
                None,
                json!({ "to": "customers", "field": "id" }),
            ))
            .detail
            .as_deref(),
            Some("customers.id")
        );
        // accepted_range → range detail; maps by bare name regardless of
        // namespace (it usually ships as dbt_utils.accepted_range).
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                json!({ "min_value": 0, "max_value": 100 }),
            )),
            ColumnTestPayload {
                name: "accepted range".to_owned(),
                values: Vec::new(),
                detail: Some("0\u{2013}100".to_owned()),
            }
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                json!({ "min_value": 0 }),
            ))
            .detail
            .as_deref(),
            Some("\u{2265} 0")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                json!({ "max_value": 1 }),
            ))
            .detail
            .as_deref(),
            Some("\u{2264} 1")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                Value::Null,
            )),
            bare_test("accepted range")
        );
    }

    #[test]
    fn column_meta_for_model_merges_descriptions_and_column_scoped_tests() {
        let mut descriptions = BTreeMap::new();
        descriptions.insert("id".to_owned(), "Primary key".to_owned());
        descriptions.insert("note".to_owned(), "Free text".to_owned());
        let model = model_node("model.shop.dim_x", "x", Some("select 1"))
            .with_column_descriptions(descriptions);
        let manifest = manifest_for(
            vec![
                model.clone(),
                column_test_node(
                    "test.shop.unique_dim_x_id",
                    "model.shop.dim_x",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
                column_test_node(
                    "test.shop.not_null_dim_x_id",
                    "model.shop.dim_x",
                    "id",
                    TestMetadata::new("not_null", None, Value::Null),
                ),
                // tested-but-undescribed column → tests-only entry
                column_test_node(
                    "test.shop.not_null_dim_x_status",
                    "model.shop.dim_x",
                    "status",
                    TestMetadata::new("not_null", None, Value::Null),
                ),
                // attached to ANOTHER model → excluded
                column_test_node(
                    "test.shop.unique_other_id",
                    "model.shop.other",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
                // model-level test (no column_name) → excluded (v1 scope)
                Node::new(
                    NodeId::new("test.shop.model_level"),
                    "test",
                    checksum("t"),
                    None,
                    None,
                    DependsOn::default(),
                    None,
                    NodeConfig::default(),
                    None,
                    BTreeMap::new(),
                )
                .with_test_attachment(
                    None,
                    Some(NodeId::new("model.shop.dim_x")),
                    Some(TestMetadata::new("custom_model_check", None, Value::Null)),
                ),
                // singular test (no test_metadata) → excluded
                Node::new(
                    NodeId::new("test.shop.singular"),
                    "test",
                    checksum("t"),
                    None,
                    None,
                    DependsOn::default(),
                    None,
                    NodeConfig::default(),
                    None,
                    BTreeMap::new(),
                )
                .with_test_attachment(
                    Some("id".to_owned()),
                    Some(NodeId::new("model.shop.dim_x")),
                    None,
                ),
            ],
            vec![],
        );
        let model_ref = manifest.node(&NodeId::new("model.shop.dim_x")).unwrap();
        let meta = column_meta_for_model(&manifest, model_ref);

        let id_meta = meta.get("id").expect("id has metadata");
        assert_eq!(id_meta.description.as_deref(), Some("Primary key"));
        // Sorted by display name → deterministic regardless of HashMap order.
        assert_eq!(
            id_meta.tests,
            vec![bare_test("not null"), bare_test("unique")]
        );

        let note_meta = meta.get("note").expect("described-only column present");
        assert_eq!(note_meta.description.as_deref(), Some("Free text"));
        assert!(note_meta.tests.is_empty());

        let status_meta = meta.get("status").expect("tested-only column present");
        assert!(status_meta.description.is_none());
        assert_eq!(status_meta.tests, vec![bare_test("not null")]);

        assert_eq!(meta.len(), 3, "no entries beyond id/note/status: {meta:?}");
    }

    #[test]
    fn build_test_payload_attaches_column_meta_per_table_owner() {
        // Target model dim_x (expect cols id/status) + input model stg_src
        // (given col src_id). The expect map must come from dim_x, the
        // given map from stg_src, each filtered to its own table's columns.
        let mut target_desc = BTreeMap::new();
        target_desc.insert("id".to_owned(), "Primary key".to_owned());
        target_desc.insert("not_in_fixture".to_owned(), "never rendered".to_owned());
        let target = model_node("model.shop.dim_x", "x", Some("select 1"))
            .with_column_descriptions(target_desc);
        let mut src_desc = BTreeMap::new();
        src_desc.insert("src_id".to_owned(), "Source key".to_owned());
        let src = model_node("model.shop.stg_src", "s", Some("select 1"))
            .with_column_descriptions(src_desc);
        let manifest = manifest_for(
            vec![
                target.clone(),
                src,
                column_test_node(
                    "test.shop.unique_dim_x_id",
                    "model.shop.dim_x",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
            ],
            vec![],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "ref('stg_src')",
                json!([{ "src_id": 1, "unknown_col": 2 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(
                json!([{ "id": 1, "status": "ok" }]),
                Some("dict".to_owned()),
                None,
            ),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let target_meta = column_meta_for_model(&manifest, &target);
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &target_meta,
        );

        // Expect side: dim_x meta filtered to the expect table's columns —
        // `id` (described + tested) is present; `status` (no meta) and
        // `not_in_fixture` (not a table column) are absent.
        let expect_meta = &payload.expected.column_meta;
        assert_eq!(
            expect_meta.get("id").and_then(|m| m.description.as_deref()),
            Some("Primary key")
        );
        assert_eq!(
            expect_meta.get("id").map(|m| m.tests.as_slice()),
            Some(&[bare_test("unique")][..])
        );
        assert!(!expect_meta.contains_key("status"));
        assert!(!expect_meta.contains_key("not_in_fixture"));

        // Given side: stg_src meta — its own description, never dim_x's.
        let given_meta = &payload.given[0].column_meta;
        assert_eq!(
            given_meta
                .get("src_id")
                .and_then(|m| m.description.as_deref()),
            Some("Source key")
        );
        assert!(!given_meta.contains_key("unknown_col"));
        assert!(!given_meta.contains_key("id"));
    }

    #[test]
    fn build_test_payload_this_given_uses_target_model_meta() {
        let mut target_desc = BTreeMap::new();
        target_desc.insert("id".to_owned(), "Primary key".to_owned());
        let target = model_node("model.shop.dim_x", "x", Some("select 1"))
            .with_column_descriptions(target_desc);
        let manifest = manifest_for(vec![target.clone()], vec![]);
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "this",
                json!([{ "id": 1 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 2 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let target_meta = column_meta_for_model(&manifest, &target);
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &target_meta,
        );
        assert_eq!(
            payload.given[0]
                .column_meta
                .get("id")
                .and_then(|m| m.description.as_deref()),
            Some("Primary key"),
            "a `this` given is the model's own prior state — target meta applies",
        );
    }

    #[test]
    fn build_test_payload_attaches_seed_column_meta_for_a_seed_ref_given() {
        // cute-dbt#235 — dbt's ref() resolves over the refable set
        // (models, seeds, snapshots), and a unit-test given may ref a
        // seed (the committed jaffle-shop fixture's
        // `ref('raw_customers')` is exactly this shape). The given's
        // column tooltips must ride the SEED's declared metadata the
        // same way a model-ref given rides its input model's.
        let mut seed_desc = BTreeMap::new();
        seed_desc.insert("customer_id".to_owned(), "Seed primary key".to_owned());
        let seed = Node::new(
            NodeId::new("seed.shop.raw_customers"),
            "seed",
            checksum("seed"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_column_descriptions(seed_desc);
        let target = model_node("model.shop.dim_x", "x", Some("select 1"));
        let manifest = manifest_for(
            vec![
                target,
                seed,
                column_test_node(
                    "test.shop.unique_raw_customers_customer_id",
                    "seed.shop.raw_customers",
                    "customer_id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
            ],
            vec![],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "ref('raw_customers')",
                json!([{ "customer_id": 1, "undocumented": 2 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 1 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &BTreeMap::new(),
        );
        let given_meta = &payload.given[0].column_meta;
        assert_eq!(
            given_meta
                .get("customer_id")
                .and_then(|m| m.description.as_deref()),
            Some("Seed primary key"),
            "a seed-ref given resolves the seed's column descriptions",
        );
        assert_eq!(
            given_meta.get("customer_id").map(|m| m.tests.as_slice()),
            Some(&[bare_test("unique")][..]),
            "column-scoped tests attached to the seed ride along",
        );
        assert!(
            !given_meta.contains_key("undocumented"),
            "honest degrade: an undeclared fixture column carries no meta",
        );
    }

    #[test]
    fn build_test_payload_attaches_source_column_meta_for_a_source_given() {
        // cute-dbt#235 — a `source('a','b')` given resolves the SOURCE's
        // declared column descriptions (fusion `ManifestSource.columns`,
        // verified on the committed playground fixture's
        // `synthea_raw.patients.Id`) plus any column-scoped data tests
        // attached to the source node.
        let mut src_desc = BTreeMap::new();
        src_desc.insert("Id".to_owned(), "Unique patient identifier".to_owned());
        let source = SourceNode::new(
            NodeId::new("source.shop.raw.patients"),
            "raw",
            "patients",
            None,
            "main",
            None,
            None,
        )
        .with_column_descriptions(src_desc);
        let target = model_node("model.shop.dim_x", "x", Some("select 1"));
        let manifest = manifest_with_sources(
            vec![
                target,
                column_test_node(
                    "test.shop.not_null_raw_patients_Id",
                    "source.shop.raw.patients",
                    "Id",
                    TestMetadata::new("not_null", None, Value::Null),
                ),
            ],
            vec![],
            vec![source],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "source('raw', 'patients')",
                json!([{ "Id": "a-1", "FIRST": "Ada" }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 1 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &BTreeMap::new(),
        );
        let given_meta = &payload.given[0].column_meta;
        assert_eq!(
            given_meta.get("Id").and_then(|m| m.description.as_deref()),
            Some("Unique patient identifier"),
            "a source given resolves the source's column descriptions",
        );
        assert_eq!(
            given_meta.get("Id").map(|m| m.tests.as_slice()),
            Some(&[bare_test("not null")][..]),
            "column-scoped tests attached to the source ride along",
        );
        assert!(
            !given_meta.contains_key("FIRST"),
            "honest degrade: an undescribed source column carries no meta",
        );
    }

    #[test]
    fn source_given_with_no_declared_columns_keeps_column_meta_empty() {
        // cute-dbt#235 honest degrade at the payload level: a source
        // with no declared columns contributes NOTHING — the empty map
        // is omitted from the wire and the JS renders no trigger
        // (never an empty bubble).
        let source = SourceNode::new(
            NodeId::new("source.shop.raw.events"),
            "raw",
            "events",
            None,
            "main",
            None,
            None,
        );
        let manifest = manifest_with_sources(
            vec![model_node("model.shop.dim_x", "x", Some("select 1"))],
            vec![],
            vec![source],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "source('raw', 'events')",
                json!([{ "event_id": 1 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 1 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &BTreeMap::new(),
        );
        assert!(
            payload.given[0].column_meta.is_empty(),
            "no declared source columns → empty (omitted) column_meta",
        );
    }

    #[test]
    fn column_meta_is_omitted_from_the_wire_when_empty() {
        // A model with NO column metadata: the payload key must be absent
        // (skip_serializing_if) so pre-#165 reports stay byte-stable in
        // shape and the template's `colMeta || {}` read stays minimal.
        let ut = simple_unit_test("m", "t");
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(
            !json.contains("column_meta"),
            "empty column_meta must be omitted from the wire; got {json}",
        );
    }

    /// The external-fixture guard signal (cute-dbt#98, #126): an external
    /// `given`/`expect` carries `rows: null` + a `fixture` name. The payload
    /// surfaces `fixture` (so the JS can show the affordance + YAML fallback)
    /// and `rows` is JSON `null` — the two facts the JS needs to AVOID
    /// rendering a silently-empty grid. An inline-rows given omits `fixture`.
    #[test]
    fn external_fixture_given_carries_fixture_name_and_null_rows() {
        let ut = UnitTest::new(
            "t",
            NodeId::new("model.shop.m"),
            vec![UnitTestGiven::new(
                "ref('a')",
                Value::Null,
                Some("csv".to_owned()),
                Some("stg_a_fixture".to_owned()),
            )],
            UnitTestExpect::new(json!([{"id": 1}]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let graph = CteGraph::default();
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &graph,
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        // The external given: fixture name present, rows null.
        assert_eq!(payload.given[0].fixture.as_deref(), Some("stg_a_fixture"));
        assert!(
            payload.given[0].rows.is_null(),
            "external-fixture given has null rows (data not in manifest)",
        );
        // The inline expect: fixture omitted, rows present.
        assert!(payload.expected.fixture.is_none());
        assert!(payload.expected.rows.is_array());
        // Wire shape: fixture key present on the given, absent on the expect.
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(
            json.contains("stg_a_fixture"),
            "given fixture is on the wire"
        );
        let expect_json = serde_json::to_string(&payload.expected).expect("serialize expect");
        assert!(
            !expect_json.contains("fixture"),
            "inline expect omits the fixture key; got {expect_json}",
        );
    }

    // ===== cute-dbt#200 — manifest_nodes + overrides + description =====

    /// A model-level (no `column_name`) generic-test node attached to
    /// `model_id`.
    fn model_test_node(id: &str, model_id: &str, tm: TestMetadata) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            checksum("t"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(None, Some(NodeId::new(model_id)), Some(tm))
    }

    /// A described + tagged + typed-columns model node for the
    /// `manifest_nodes` tests.
    fn context_rich_model(id: &str) -> Node {
        let mut config = BTreeMap::new();
        config.insert("materialized".to_owned(), Value::from("incremental"));
        let mut columns = BTreeMap::new();
        columns.insert("id".to_owned(), Some("bigint".to_owned()));
        columns.insert("status".to_owned(), None);
        let mut descriptions = BTreeMap::new();
        descriptions.insert("id".to_owned(), "Primary key".to_owned());
        Node::new(
            NodeId::new(id),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            columns,
        )
        .with_column_descriptions(descriptions)
        .with_model_metadata(
            Some("One row per payer.".to_owned()),
            vec!["marts".to_owned(), "finance".to_owned()],
        )
    }

    /// The demo-payload-shaped grouped overrides (native scalars).
    fn grouped_overrides() -> crate::domain::UnitTestOverrides {
        let mut overrides = crate::domain::UnitTestOverrides::new();
        overrides.insert(
            "macros".to_owned(),
            [("is_incremental".to_owned(), json!(true))].into(),
        );
        overrides.insert(
            "vars".to_owned(),
            [
                ("encounter_lookback_days".to_owned(), json!(7)),
                ("dq_quarantine_threshold".to_owned(), json!(0.05)),
            ]
            .into(),
        );
        overrides
    }

    #[test]
    fn manifest_nodes_entry_carries_the_full_model_context() {
        let model = context_rich_model("model.shop.dim_payers");
        let manifest = manifest_for(
            vec![
                model,
                column_test_node(
                    "test.shop.unique_dim_payers_id",
                    "model.shop.dim_payers",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
                // Known built-in at MODEL level → §2.2 prose name + detail.
                model_test_node(
                    "test.shop.relationships_dim_payers",
                    "model.shop.dim_payers",
                    TestMetadata::new(
                        "relationships",
                        None,
                        json!({ "to": "ref('stg_payers')", "field": "payer_id" }),
                    ),
                ),
                // Unknown package test at MODEL level → package-qualified
                // raw name, no detail (open-ended kwargs uninterpreted).
                model_test_node(
                    "test.shop.unique_combo_dim_payers",
                    "model.shop.dim_payers",
                    TestMetadata::new(
                        "unique_combination_of_columns",
                        Some("dbt_utils".to_owned()),
                        json!({ "combination_of_columns": ["id", "status"] }),
                    ),
                ),
            ],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_payers")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let entry = payload
            .manifest_nodes
            .get("dim_payers")
            .expect("in-scope model keyed by BARE name");
        assert_eq!(entry.description.as_deref(), Some("One row per payer."));
        assert_eq!(entry.materialized.as_deref(), Some("incremental"));
        assert_eq!(entry.tags, ["marts".to_owned(), "finance".to_owned()]);
        assert_eq!(
            entry.columns,
            vec![
                ManifestColumnPayload {
                    name: "id".to_owned(),
                    description: Some("Primary key".to_owned()),
                    column_type: Some("bigint".to_owned()),
                    tests: vec![bare_test("unique")],
                },
                ManifestColumnPayload {
                    name: "status".to_owned(),
                    description: None,
                    column_type: None,
                    tests: vec![],
                },
            ],
            "declared columns in name order, decorated with #165 meta"
        );
        assert_eq!(
            entry.model_tests,
            vec![
                ModelTestPayload {
                    name: "dbt_utils.unique_combination_of_columns".to_owned(),
                    detail: None,
                },
                ModelTestPayload {
                    name: "relationships".to_owned(),
                    detail: Some("stg_payers.payer_id".to_owned()),
                },
            ],
            "model-level tests via the §2.2 mapping, sorted by name"
        );
    }

    #[test]
    fn manifest_nodes_include_ref_ed_upstreams_and_exclude_unrelated_models() {
        let target = model_node("model.shop.dim_x", "x", Some("select 1"));
        let upstream = model_node("model.shop.stg_src", "s", Some("select 1"))
            .with_model_metadata(Some("Staged source.".to_owned()), Vec::new());
        // Described but neither in scope nor ref()-ed → must NOT appear
        // (never the whole project graph).
        let unrelated = model_node("model.shop.dim_unrelated", "u", Some("select 1"))
            .with_model_metadata(Some("Unrelated.".to_owned()), Vec::new());
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![
                UnitTestGiven::new("ref('stg_src')", json!([]), None, None),
                // `this` resolves to the (already-present) target model.
                UnitTestGiven::new("this", json!([]), None, None),
                // source(...) inputs contribute nothing (not model nodes).
                UnitTestGiven::new("source('raw', 'orders')", json!([]), None, None),
                // An unresolvable ref contributes nothing (graceful).
                UnitTestGiven::new("ref('not_a_model')", json!([]), None, None),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(
            vec![target, upstream, unrelated],
            vec![("unit_test.shop.t", ut)],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(
            payload.manifest_nodes.keys().collect::<Vec<_>>(),
            ["stg_src"],
            "the ref()-ed upstream appears; the unrelated model does not \
             (dim_x itself is all-empty context → skipped)"
        );
        assert_eq!(
            payload.manifest_nodes["stg_src"].description.as_deref(),
            Some("Staged source.")
        );
    }

    #[test]
    fn manifest_nodes_key_is_omitted_when_every_entry_is_empty() {
        // Bare synthetic models (no description/tags/materialized/columns/
        // attached tests) must keep the manifest_nodes key OFF the wire —
        // the pre-#200 byte-stability contract.
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let ut = simple_unit_test("m", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(
            payload.manifest_nodes.is_empty(),
            "all-empty entries are skipped"
        );
        let json = serde_json::to_string(&payload).unwrap();
        assert!(
            !json.contains("manifest_nodes"),
            "empty lookup omits the key entirely: {json}"
        );
    }

    #[test]
    fn test_payload_overrides_round_trip_groups_and_native_scalar_types() {
        // The cute-dbt#197 founder decision, asserted at the WIRE level:
        // the serialized payload carries JSON bool/number/string scalars —
        // never their stringified forms.
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let ut = simple_unit_test("m", "test_one").with_overrides(Some(grouped_overrides()));
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(test.overrides.as_ref(), Some(&grouped_overrides()));
        let json = serde_json::to_string(test).unwrap();
        assert!(
            json.contains(r#""overrides":{"macros":{"is_incremental":true},"vars":{"dq_quarantine_threshold":0.05,"encounter_lookback_days":7}}"#),
            "grouped map with native scalars (bool/float/int), deterministic order: {json}"
        );
        assert!(
            !json.contains(r#""is_incremental":"true""#),
            "never stringified: {json}"
        );
    }

    #[test]
    fn test_payload_omits_the_overrides_key_when_none() {
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let ut = simple_unit_test("m", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0].tests[0]).unwrap();
        assert!(
            !json.contains("overrides"),
            "no-override tests stay byte-stable: {json}"
        );
    }

    #[test]
    fn model_payload_threads_the_model_description() {
        let node = model_node("model.shop.m", "body", Some("select 1"))
            .with_model_metadata(Some("One row per order.".to_owned()), Vec::new());
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(
            payload.models[0].description.as_deref(),
            Some("One row per order.")
        );
    }

    #[test]
    fn model_payload_omits_the_description_key_when_none() {
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0]).unwrap();
        assert!(
            !json.contains("description"),
            "undescribed models stay byte-stable: {json}"
        );
    }
}
