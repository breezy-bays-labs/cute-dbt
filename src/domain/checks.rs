//! Check engine — the coverage-intelligence walking skeleton
//! (cute-dbt#169, epic cute-dbt#168).
//!
//! One central `heuristics!` macro block is the **single source of
//! truth** for every registered check: it declares the spec metadata and
//! the detector fn adjacently and generates, per registry,
//!
//! - the check-id enum (production: [`HeuristicId`]),
//! - `SPECS` — one [`HeuristicSpec`] per variant, in declaration order,
//! - the id→detector pairing as an **exhaustive `match` with no wildcard
//!   arm** ([`CheckId::detect`]).
//!
//! Drift is impossible by mechanism, not by review: a spec without a
//! detector (or vice versa) fails the macro expansion; a dangling
//! `supersedes` entry references a nonexistent enum **variant** and fails
//! to compile; the committed `heuristics/registry.toml` + book check
//! pages are GENERATED from `SPECS` and byte-gated in CI
//! (`tests/heuristics_ledger.rs` — the example-report-check pattern).
//! Supersedes acyclicity is a cheap total unit test
//! ([`supersedes_is_acyclic`]).
//!
//! ## Verdict model
//!
//! Every check is a pair: a *construct trigger* (the manifest/AST shape)
//! and a *satisfaction predicate*. The engine emits a [`Finding`] — a
//! verdict per **(construct, check)** — never just gap findings:
//!
//! - [`Verdict::Covered`] `{ by }` — attribution is free: the predicate
//!   knows which test(s) satisfied it;
//! - [`Verdict::Uncovered`] — the recommendation fires;
//! - [`Verdict::Unknown`] — the predicate is not statically decidable for
//!   this construct (honest tier: never nagged as a gap).
//!
//! `SUPPRESSED` is **display-layer only** and deliberately not a variant:
//! suppressed/disabled checks still evaluate and still participate in
//! supersedes resolution. The pipeline order is fixed —
//! [`evaluate_all`] → [`resolve_supersedes`] → [`filter_for_display`]
//! (downstream) — so disabling a superseding check can never resurrect
//! the superseded finding on the very construct it misreads.
//!
//! ## v0.1 registry
//!
//! Two checks ship today:
//!
//! - `grain.unique-key-unbacked` (TOTAL, the cute-dbt#169 walking
//!   skeleton) — a model declares `config.unique_key` (merge/
//!   delete+insert semantics depend on it) but no enabled uniqueness
//!   data test covers a column set ⊆ the key. Wire shapes verified
//!   against dbt-fusion `9977b6cbb1b761065536300037560d8e3c037011`
//!   (`DbtUniqueKey` in `dbt-schemas/src/schemas/common.rs`;
//!   test-kwargs extraction in
//!   `dbt-parser/src/resolve/primary_key_inference.rs`) and against the
//!   committed `playground-current.json` fixture
//!   (`tests/check_engine.rs`).
//! - `union.arm-coverage` (HIGH, cute-dbt#172 — catalog class C3) — a
//!   model UNIONs N arms and the unit-test givens leave one or more
//!   arms provably unexercised at the fixture-input level. Consumes the
//!   EXISTING [`CteGraph`] union facts (union-typed edges +
//!   `body_leaf_table_refs`, cute-dbt#40) and the cute-dbt#131
//!   given↔leaf-ref binding — no new AST pass. Per-given runtime
//!   semantics verified against dbt-fusion
//!   `9977b6cbb1b761065536300037560d8e3c037011`
//!   (`render_unit_test` in
//!   `dbt-tasks-sa/src/renderable/renderable/unit_test.rs` builds one
//!   mock CTE per `given` entry — a relation with no given keeps
//!   reading its real table, which is why an unmocked *seed* input is
//!   honest UNKNOWN, never UNCOVERED).
//!
//! Domain purity: `std` + `serde` (+ `serde_json::Value` passthrough)
//! only — no I/O, no parser deps. Checks stay thin pattern-matchers over
//! already-parsed manifest + [`CteGraph`] facts (the `StateModifier`
//! precedent: plain functions until ≥2 rules force a seam).

use std::collections::{BTreeMap, BTreeSet};
// Infallible when writing into a String — the ledger generators use
// `let _ = write!(...)` per clippy::format_push_string.
use std::fmt::Write as _;

use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::domain::cte::{CteGraph, EdgeType};
use crate::domain::manifest::{Manifest, Node, NodeId};
use crate::domain::state::resolve_target_model;
use crate::domain::unit_test::{UnitTest, UnitTestGiven};
use crate::domain::unit_test_table::table_from_manifest_rows;

// ---------------------------------------------------------------------
// Spec vocabulary.
// ---------------------------------------------------------------------

/// Accuracy tier of a check — the credibility contract. Labeled in
/// output, never blended; gating (if ever) only on [`Tier::Total`]
/// (epic cute-dbt#168 design tenets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Deterministic over manifest facts — zero false positives by
    /// construction.
    Total,
    /// High-confidence pattern match; rare false positives possible.
    High,
    /// Heuristic advice; informational only.
    Advisory,
}

impl Tier {
    /// The ledger string form (`total` / `high` / `advisory`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::High => "high",
            Self::Advisory => "advisory",
        }
    }
}

/// Which testing instrument a check recommends — instrument-aware
/// routing, never unit-test-maximalist (epic cute-dbt#168 design tenets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Instrument {
    /// A dbt unit test (fixture-driven, `unit_tests:` block).
    UnitTest,
    /// A dbt data test (schema `tests:` / generic test).
    DataTest,
    /// Either instrument satisfies the check.
    Both,
}

impl Instrument {
    /// The ledger string form (`unit-test` / `data-test` / `both`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnitTest => "unit-test",
            Self::DataTest => "data-test",
            Self::Both => "both",
        }
    }
}

/// The spec metadata declared beside each detector in the
/// `heuristics!` block — the readable contract a user audits. The
/// `conditions` / `exclusions` strings are **prose mirrors** of the coded
/// predicate; the detector fn is the enforcement (the only residual
/// human-honesty surface — everything identifier-shaped is
/// impossible-by-construction).
#[derive(Debug)]
pub struct HeuristicSpec<Id: 'static> {
    /// The generated enum variant this spec belongs to.
    pub id: Id,
    /// Canonical dotted id string (e.g. `grain.unique-key-unbacked`).
    pub id_str: &'static str,
    /// Human-facing display name.
    pub name: &'static str,
    /// Check group (the dotted id's prefix, e.g. `grain`, `join`).
    pub group: &'static str,
    /// Accuracy tier (labeled in output, never blended).
    pub tier: Tier,
    /// Recommended testing instrument.
    pub instrument: Instrument,
    /// Checks this one supersedes — enum **variants**, so a dangling
    /// edge is a compile error. Shallow + acyclic by contract
    /// ([`supersedes_is_acyclic`]).
    pub supersedes: &'static [Id],
    /// Which extraction facts the detector consumes (the dependency map
    /// onto the evidence-model ladder).
    pub evidence: &'static [&'static str],
    /// Prose mirror of the trigger + satisfaction predicate.
    pub conditions: &'static [&'static str],
    /// Prose mirror of the shapes the check deliberately stays silent
    /// (or goes `UNKNOWN`) on. Every entry ships a paired negative test.
    pub exclusions: &'static [&'static str],
    /// The fix the report recommends when the verdict is `UNCOVERED`.
    pub recommendation: &'static str,
    /// Why the gap matters — embedded inline in the report (zero-egress).
    pub rationale: &'static str,
}

/// A check-id enum generated by the `heuristics!` macro.
///
/// Implemented only by macro expansion — the trait is the seam that lets
/// the engine pipeline ([`evaluate_all`] / [`resolve_supersedes`] /
/// [`filter_for_display`] / [`supersedes_is_acyclic`]) run unchanged over
/// the production [`HeuristicId`] registry *and* over small synthetic
/// test registries (which is how multi-check pipeline behaviour is tested
/// while the product registry holds a single walking-skeleton check).
pub trait CheckId: Copy + Eq + Ord + std::fmt::Debug + Sized + 'static {
    /// Every registered check, in declaration order.
    const ALL: &'static [Self];
    /// One spec per check, in the same declaration order as
    /// [`Self::ALL`].
    const SPECS: &'static [HeuristicSpec<Self>];

    /// The spec declared beside this check's detector.
    #[must_use]
    fn spec(self) -> &'static HeuristicSpec<Self>;

    /// The canonical dotted id string.
    #[must_use]
    fn as_str(self) -> &'static str {
        self.spec().id_str
    }

    /// Run this check's detector — the macro-generated **exhaustive,
    /// no-wildcard** id→detector dispatch.
    #[must_use]
    fn detect(self, ctx: &CheckContext<'_>) -> Vec<Finding<Self>>;
}

/// The evidence a detector pattern-matches over: the whole parsed
/// [`Manifest`] plus the one model node under evaluation, plus the
/// model's already-parsed [`CteGraph`] (the cute-dbt#40 single-parse
/// pass — the second evidence family on the extraction ladder). Borrowed
/// POD facts only — detectors never do I/O and never re-parse SQL.
#[derive(Debug, Clone, Copy)]
pub struct CheckContext<'a> {
    /// The full current manifest (test-node resolution, sibling lookups).
    pub manifest: &'a Manifest,
    /// The model node the engine is evaluating.
    pub model: &'a Node,
    /// The model's CTE graph, parsed once by the adapter from
    /// `compiled_code` (cute-dbt#172). `None` when the caller computed
    /// no graph — graph-fact checks then stay silent (no graph evidence
    /// is not evidence of absence; manifest-fact checks are unaffected).
    pub cte_graph: Option<&'a CteGraph>,
}

// ---------------------------------------------------------------------
// Verdict + Finding PODs.
// ---------------------------------------------------------------------

/// The verdict for one (construct, check) pair (cute-dbt#169 —
/// satisfaction detection, design sketch §5c).
///
/// `SUPPRESSED` is deliberately **not** a variant: suppression is a
/// display-layer filter applied after supersedes resolution; the engine
/// always evaluates and always emits one of these three.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Verdict {
    /// The satisfaction predicate found ≥1 satisfying test.
    Covered {
        /// The manifest node/test ids that satisfy the check, sorted for
        /// deterministic output. Attribution falls out of the predicate
        /// for free — retrofitting it later would touch every detector,
        /// so it is in the POD from day one.
        by: Vec<String>,
    },
    /// The construct trigger fired and no test satisfies the predicate —
    /// the recommendation fires.
    Uncovered,
    /// The predicate is not statically decidable for this construct
    /// (honest tier: surfaced in verbose views, never nagged as a gap).
    Unknown,
}

/// One concrete evidence instance that tripped a check — a name (and, for
/// future AST-zone checks, a span) the report can pin in-context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// What the value names (e.g. `unique_key`).
    pub label: String,
    /// The concrete instance (e.g. `customer_id, order_date`).
    pub value: String,
}

impl Evidence {
    /// Canonical constructor.
    #[must_use]
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// One emitted verdict for a (construct, check) pair on a target model —
/// the POD the render payload serializes (cute-dbt#169).
///
/// `tier` / `instrument` / `recommendation` are denormalized from the
/// check's [`HeuristicSpec`] by [`Finding::new`] so a detector cannot
/// mislabel them and the payload is renderable without reaching back
/// into Rust statics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(bound(serialize = "Id: CheckId"))]
pub struct Finding<Id: CheckId> {
    /// The check that produced this finding, serialized as its canonical
    /// dotted id string.
    #[serde(serialize_with = "serialize_check_id")]
    pub check: Id,
    /// The check's accuracy tier (from the spec).
    pub tier: Tier,
    /// The check's recommended instrument (from the spec).
    pub instrument: Instrument,
    /// Full node id of the target model.
    pub model_id: NodeId,
    /// Stable discriminator of the construct within the model (e.g.
    /// `config.unique_key`) — the supersedes-resolution join key together
    /// with [`Self::model_id`].
    pub construct: String,
    /// The per-(construct, check) verdict.
    pub verdict: Verdict,
    /// Concrete evidence instances that tripped the check. Omitted from
    /// JSON when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    /// The spec's recommendation copy — present **only** on an
    /// [`Verdict::Uncovered`] finding (a covered/unknown construct has
    /// nothing to recommend). Omitted from JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
}

/// Serialize a [`CheckId`] as its canonical dotted id string.
fn serialize_check_id<Id: CheckId, S: Serializer>(
    id: &Id,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(id.as_str())
}

impl<Id: CheckId> Finding<Id> {
    /// Build a finding for `check` on `(model_id, construct)`, filling
    /// `tier` / `instrument` / `recommendation` from the check's spec.
    #[must_use]
    pub fn new(
        check: Id,
        model_id: NodeId,
        construct: impl Into<String>,
        verdict: Verdict,
        evidence: Vec<Evidence>,
    ) -> Self {
        let spec = check.spec();
        let recommendation =
            matches!(verdict, Verdict::Uncovered).then(|| spec.recommendation.to_owned());
        Self {
            check,
            tier: spec.tier,
            instrument: spec.instrument,
            model_id,
            construct: construct.into(),
            verdict,
            evidence,
            recommendation,
        }
    }
}

// ---------------------------------------------------------------------
// The engine pipeline — FIXED order:
//   evaluate_all → resolve_supersedes → filter_for_display (downstream).
// ---------------------------------------------------------------------

/// Stage 1 — run **every** registered check against `ctx`, in
/// declaration order. Selection/suppression must never reach into this
/// stage: a disabled check still evaluates so it can still supersede
/// (epic cute-dbt#168, suppression-hierarchy invariant).
#[must_use]
pub fn evaluate_all<Id: CheckId>(ctx: &CheckContext<'_>) -> Vec<Finding<Id>> {
    Id::ALL.iter().flat_map(|id| id.detect(ctx)).collect()
}

/// Stage 2 — drop every finding superseded by another **fired** finding
/// on the same `(model_id, construct)`.
///
/// "Fired" means *emitted by stage 1*, before any resolution or display
/// filtering: a finding that is itself dropped here still silences the
/// checks it supersedes (shallow, one-level resolution — no chaining, no
/// numeric priorities; design sketch §3). Order of surviving findings is
/// preserved.
#[must_use]
pub fn resolve_supersedes<Id: CheckId>(findings: Vec<Finding<Id>>) -> Vec<Finding<Id>> {
    let keep: Vec<bool> = findings
        .iter()
        .map(|finding| {
            !findings.iter().any(|other| {
                other.check != finding.check
                    && other.model_id == finding.model_id
                    && other.construct == finding.construct
                    && other.check.spec().supersedes.contains(&finding.check)
            })
        })
        .collect();
    findings
        .into_iter()
        .zip(keep)
        .filter_map(|(finding, keep)| keep.then_some(finding))
        .collect()
}

/// Stage 3 (display layer, downstream of resolution) — remove findings
/// whose check the operator disabled/suppressed.
///
/// Runs strictly **after** [`resolve_supersedes`], so disabling a
/// superseding check removes its own findings but never resurrects the
/// findings it superseded. v0.1 has no user-facing selection config yet
/// (that is a separate epic slice); the seam exists now because the
/// pipeline order is the load-bearing contract.
#[must_use]
pub fn filter_for_display<Id: CheckId>(
    findings: Vec<Finding<Id>>,
    disabled: &[Id],
) -> Vec<Finding<Id>> {
    findings
        .into_iter()
        .filter(|finding| !disabled.contains(&finding.check))
        .collect()
}

/// `true` when the registry's `supersedes` graph has no cycle.
///
/// Cheap and total (the registry is a small static); pinned by a unit
/// test on the production registry so a cyclic edge set fails `cargo
/// test` even though it compiles.
#[must_use]
pub fn supersedes_is_acyclic<Id: CheckId>() -> bool {
    const UNVISITED: u8 = 0;
    const IN_STACK: u8 = 1;
    const DONE: u8 = 2;

    fn index_of<Id: CheckId>(id: Id) -> usize {
        Id::ALL
            .iter()
            .position(|candidate| *candidate == id)
            .expect("supersedes edges reference registered variants by construction")
    }

    fn visit<Id: CheckId>(id: Id, colors: &mut [u8]) -> bool {
        let index = index_of(id);
        match colors[index] {
            IN_STACK => return false,
            DONE => return true,
            _ => {}
        }
        colors[index] = IN_STACK;
        for &next in id.spec().supersedes {
            if !visit(next, colors) {
                return false;
            }
        }
        colors[index] = DONE;
        true
    }

    let mut colors = vec![UNVISITED; Id::ALL.len()];
    Id::ALL.iter().all(|&id| visit(id, &mut colors))
}

/// The run-loop entry: the fixed evaluate → resolve pipeline for one
/// model, over the production [`HeuristicId`] registry. Display
/// filtering ([`filter_for_display`]) is deliberately NOT applied here —
/// it is a downstream presentation step (and has no config surface yet).
///
/// `cte_graph` is the model's already-parsed CTE graph (the renderer's
/// single `parse_cte_graph` pass) — `None` when no graph was computed;
/// graph-fact checks (cute-dbt#172) then stay silent.
#[must_use]
pub fn model_findings(
    manifest: &Manifest,
    model: &Node,
    cte_graph: Option<&CteGraph>,
) -> Vec<Finding<HeuristicId>> {
    let ctx = CheckContext {
        manifest,
        model,
        cte_graph,
    };
    resolve_supersedes(evaluate_all::<HeuristicId>(&ctx))
}

// ---------------------------------------------------------------------
// The heuristics! macro — single source of truth per registry.
// ---------------------------------------------------------------------

/// Declare a check registry: spec metadata + detector fn **adjacently**,
/// one block per registry (production: [`HeuristicId`]).
///
/// Generates the unit-variant id enum, the `CheckId` impl (`ALL`,
/// `SPECS`, `spec`, and the exhaustive no-wildcard `detect` match). Every
/// field is macro-required, so a spec without a detector — or a detector
/// without spec metadata — fails to expand; `supersedes` entries are enum
/// **variant names**, so a dangling edge fails to compile.
macro_rules! heuristics {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident {
                    id: $id_str:literal,
                    name: $display:literal,
                    group: $group:literal,
                    tier: $tier:ident,
                    instrument: $instrument:ident,
                    supersedes: [$($supersedes:ident),* $(,)?],
                    evidence: [$($evidence:literal),* $(,)?],
                    conditions: [$($condition:literal),+ $(,)?],
                    exclusions: [$($exclusion:literal),* $(,)?],
                    recommendation: $recommendation:literal,
                    rationale: $rationale:literal,
                    detector: $detector:expr,
                }
            ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $crate::domain::checks::CheckId for $name {
            const ALL: &'static [Self] = &[ $( Self::$variant ),+ ];

            const SPECS: &'static [$crate::domain::checks::HeuristicSpec<Self>] = &[
                $(
                    $crate::domain::checks::HeuristicSpec {
                        id: Self::$variant,
                        id_str: $id_str,
                        name: $display,
                        group: $group,
                        tier: $crate::domain::checks::Tier::$tier,
                        instrument: $crate::domain::checks::Instrument::$instrument,
                        supersedes: &[ $( Self::$supersedes ),* ],
                        evidence: &[ $( $evidence ),* ],
                        conditions: &[ $( $condition ),+ ],
                        exclusions: &[ $( $exclusion ),* ],
                        recommendation: $recommendation,
                        rationale: $rationale,
                    },
                )+
            ];

            fn spec(self) -> &'static $crate::domain::checks::HeuristicSpec<Self> {
                // Unit variants in declaration order — the discriminant IS
                // the SPECS index (a unit test pins spec().id == self for
                // every variant).
                &Self::SPECS[self as usize]
            }

            fn detect(
                self,
                ctx: &$crate::domain::checks::CheckContext<'_>,
            ) -> Vec<$crate::domain::checks::Finding<Self>> {
                // Exhaustive, NO wildcard arm: adding a variant without a
                // detector (or removing a detector while its variant
                // remains) is a compile error.
                match self {
                    $( Self::$variant => ($detector)(ctx), )+
                }
            }
        }
    };
}

// ---------------------------------------------------------------------
// The production registry.
// ---------------------------------------------------------------------

heuristics! {
    /// The registered checks — one variant per check, declared in the
    /// central `heuristics!` block beside its spec + detector
    /// (cute-dbt#169). `heuristics/registry.toml` and the book check
    /// pages are generated from this block — edit HERE, then regenerate
    /// (`GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger`).
    pub enum HeuristicId {
        /// `grain.unique-key-unbacked` — declared unique key, no backing
        /// uniqueness data test.
        GrainUniqueKeyUnbacked {
            id: "grain.unique-key-unbacked",
            name: "Unique key without a uniqueness test",
            group: "grain",
            tier: Total,
            instrument: DataTest,
            supersedes: [],
            evidence: ["manifest.config.unique-key", "manifest.test-nodes"],
            conditions: [
                "the model declares config.unique_key (a column name or a list of columns)",
                "no enabled uniqueness data test (unique, or a composite unique_combination_of_columns) attached to the model has a column set that is a subset of the declared key",
            ],
            exclusions: [
                "a unique_key value that is not a literal column name / list of column names is reported UNKNOWN, never UNCOVERED (the declared grain is not statically recoverable)",
                "a uniqueness test whose column set is WIDER than the key does not satisfy the check (uniqueness of a superset does not imply uniqueness at the declared grain)",
            ],
            recommendation: "Add a uniqueness data test at the declared grain: `unique` on a single-column key, or `dbt_utils.unique_combination_of_columns` over the composite key columns.",
            rationale: "Incremental merge / delete+insert semantics silently depend on the declared unique_key actually being unique — a duplicate key corrupts the merge with no test to catch it. Declaring a grain without a test at that grain is an unverified load-bearing assumption.",
            detector: detect_grain_unique_key_unbacked,
        },
        /// `union.arm-coverage` — UNION arms left unexercised by the
        /// unit-test givens (cute-dbt#172, catalog class C3).
        UnionArmCoverage {
            id: "union.arm-coverage",
            name: "Unexercised UNION arm",
            group: "union",
            tier: High,
            instrument: UnitTest,
            supersedes: [],
            evidence: [
                "cte-graph.union-edges",
                "cte-graph.body-leaf-table-refs",
                "manifest.unit-test-givens",
            ],
            conditions: [
                "the model's body (or a CTE within it) UNIONs arms the CTE engine resolved to union-typed edges — each checked arm is a join-free reference to an earlier CTE (`UnionAll` / `UnionDistinct`)",
                "an arm counts as exercised when at least one unit-test given with one or more in-manifest rows binds — by `ref(...)` / `source(...)` leaf name, case-insensitive — to any external relation in the arm's upstream CTE closure",
                "a given bound to a relation shared by several arms exercises every arm whose closure reads it: its rows provably enter each arm's scan, while per-arm filter survival is deliberately out of scope (no predicate evaluation) — the HIGH-tier cue boundary, never an assertion of output-level coverage",
                "verdict order: any provably-unfed arm makes the construct UNCOVERED; otherwise any statically-unattributable arm makes it UNKNOWN; otherwise COVERED, attributing every test that feeds an arm",
            ],
            exclusions: [
                "arms that are not a join-free reference to an earlier CTE (join chains, derived tables, arms reading external tables directly, EXCEPT/INTERSECT arms) emit no union edge and are invisible to this check — never counted, never reported",
                "an arm whose upstream closure reads no resolvable external relation (constant SELECT, table functions) makes the construct UNKNOWN, never UNCOVERED",
                "an arm fed only by external-fixture or non-literal-sql givens (row counts not statically recoverable) makes the construct UNKNOWN, never UNCOVERED",
                "an arm whose only unbound feeding relation resolves to a seed is UNKNOWN, never UNCOVERED, when the model has unit tests (dbt lets seed inputs go ungiven and reads the real seed file)",
                "`this` givens (incremental prior state) never feed a union arm",
            ],
            recommendation: "Add (or fill) a given row for each unexercised UNION arm's input so every arm contributes at least one row, then extend `expect` with the row(s) that arm should emit. This finding's evidence carries the per-arm input and a given-row sketch.",
            rationale: "A UNION arm with no fixture rows contributes nothing to any unit test: its projection, casts, and filters run on zero rows, so a column mix-up or a dropped row in that arm ships silently. One given row per arm makes every branch's contribution visible in the expected output.",
            detector: detect_union_arm_coverage,
        },
    }
}

// ---------------------------------------------------------------------
// grain.unique-key-unbacked — the walking-skeleton detector.
// ---------------------------------------------------------------------

/// The construct discriminator for the unique-key grain check.
const UNIQUE_KEY_CONSTRUCT: &str = "config.unique_key";

/// Detector for `grain.unique-key-unbacked` (cute-dbt#169).
///
/// Trigger: a `model` node declaring `config.unique_key`
/// ([`crate::domain::manifest::NodeConfig::unique_key`]). Satisfaction:
/// any **enabled** uniqueness data test attached to the model whose
/// column set ⊆ the key columns (case-insensitive ASCII fold) — a test
/// proving uniqueness of a subset proves it at the declared grain, a
/// wider set does not. A composite `unique_combination_of_columns` is
/// kept **composite** (fusion's primary-key inference flattens it per
/// column — `dbt-parser/src/resolve/primary_key_inference.rs`,
/// `9977b6cbb1b761065536300037560d8e3c037011` — which would be unsound
/// here: pair-uniqueness does not imply per-column uniqueness).
///
/// Verdicts: satisfying tests found ⇒ [`Verdict::Covered`] attributing
/// their node ids (sorted); none ⇒ [`Verdict::Uncovered`]; a declared
/// key whose columns are not statically recoverable ⇒
/// [`Verdict::Unknown`]. No `unique_key` ⇒ no finding (trigger silent).
fn detect_grain_unique_key_unbacked(ctx: &CheckContext<'_>) -> Vec<Finding<HeuristicId>> {
    let check = HeuristicId::GrainUniqueKeyUnbacked;
    if ctx.model.resource_type() != "model" {
        return Vec::new();
    }
    let Some(unique_key) = ctx.model.config().unique_key() else {
        return Vec::new();
    };
    let columns = unique_key.columns().filter(|columns| !columns.is_empty());
    let Some(columns) = columns else {
        // Present but not a recoverable column list (or empty): honest
        // UNKNOWN — surface the raw value as evidence, never nag a gap.
        let raw = ctx
            .model
            .config()
            .config()
            .get("unique_key")
            .map_or_else(String::new, Value::to_string);
        return vec![Finding::new(
            check,
            ctx.model.id().clone(),
            UNIQUE_KEY_CONSTRUCT,
            Verdict::Unknown,
            vec![Evidence::new("unique_key", raw)],
        )];
    };
    let evidence = vec![Evidence::new("unique_key", columns.join(", "))];
    let key_set: BTreeSet<String> = columns
        .iter()
        .map(|column| column.to_ascii_lowercase())
        .collect();
    let mut by: Vec<String> = ctx
        .manifest
        .nodes()
        .iter()
        .filter(|(_, node)| covers_grain(node, ctx.model.id(), &key_set))
        .map(|(id, _)| id.as_str().to_owned())
        .collect();
    by.sort();
    let verdict = if by.is_empty() {
        Verdict::Uncovered
    } else {
        Verdict::Covered { by }
    };
    vec![Finding::new(
        check,
        ctx.model.id().clone(),
        UNIQUE_KEY_CONSTRUCT,
        verdict,
        evidence,
    )]
}

/// `true` when `node` is an enabled uniqueness data test attached to
/// `model_id` whose column set ⊆ `key_set` (already lowercased).
fn covers_grain(node: &Node, model_id: &NodeId, key_set: &BTreeSet<String>) -> bool {
    if node.resource_type() != "test" || node.attached_node() != Some(model_id) {
        return false;
    }
    if !test_is_enabled(node) {
        return false;
    }
    let Some(columns) = uniqueness_test_columns(node) else {
        return false;
    };
    !columns.is_empty()
        && columns
            .iter()
            .all(|column| key_set.contains(&column.to_ascii_lowercase()))
}

/// `config.enabled` on a test node, defaulting to enabled — mirrors
/// fusion's `get_enabled_with_default` (a disabled test asserts nothing).
fn test_is_enabled(node: &Node) -> bool {
    node.config()
        .config()
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// The column set a uniqueness test asserts, or `None` when `node` is
/// not a uniqueness test.
///
/// Extraction mirrors fusion's `extract_columns_from_metadata`
/// (`primary_key_inference.rs`, `9977b6cb…`): `unique` reads
/// `kwargs.column_name` (string; falling back to the node-level
/// `column_name` ingested in cute-dbt#166);
/// `unique_combination_of_columns` (any namespace — canonically
/// `dbt_utils`) reads the `kwargs.combination_of_columns` string array,
/// returned **whole** (kept composite — never flattened per column). A
/// combination array with non-string entries is not statically
/// recoverable ⇒ `None` (the test simply does not count as coverage).
fn uniqueness_test_columns(node: &Node) -> Option<Vec<String>> {
    let test_metadata = node.test_metadata()?;
    match test_metadata.name() {
        "unique" => {
            let column = test_metadata
                .kwargs()
                .get("column_name")
                .and_then(Value::as_str)
                .or_else(|| node.column_name())?;
            Some(vec![column.to_owned()])
        }
        "unique_combination_of_columns" => {
            let items = test_metadata
                .kwargs()
                .get("combination_of_columns")?
                .as_array()?;
            let columns: Vec<String> = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            (columns.len() == items.len() && !columns.is_empty()).then_some(columns)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------
// union.arm-coverage — UNION arms unexercised by unit-test givens
// (cute-dbt#172, catalog class C3).
// ---------------------------------------------------------------------

/// Whether a given's in-manifest rows are statically countable, and if
/// so whether any exist. Three-valued on purpose: the honest-tier
/// contract forbids guessing (`Unknown` never becomes a nagged gap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowsPresence {
    /// ≥1 row recovered from the manifest payload.
    NonEmpty,
    /// The payload provably carries zero rows (`rows: []`, header-only
    /// csv, …).
    Empty,
    /// Not statically countable: an external `fixture:` file (rows live
    /// on disk, not in the manifest — cute-dbt#126) or a non-literal
    /// `format: sql` SELECT.
    Unknown,
}

/// How one union arm relates to the unit-test fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArmCoverage {
    /// ≥1 non-empty given (in ≥1 test) feeds the arm's closure.
    Fed,
    /// Every test provably feeds the arm zero rows.
    Unfed,
    /// Not statically decidable (unbindable closure, unknown-count
    /// givens, or an ungiven seed input).
    Unknown,
}

/// Classify a given's `rows` payload via the shared fixture normalizer
/// ([`table_from_manifest_rows`] — the cute-dbt#66/#127/#137 single
/// source of truth for dict / csv-string / literal-sql row recovery).
fn given_rows_presence(given: &UnitTestGiven) -> RowsPresence {
    if given.fixture().is_some() {
        // External fixture file: the rows are NOT in the manifest and
        // the domain does no I/O — statically unknowable (cute-dbt#126).
        return RowsPresence::Unknown;
    }
    match table_from_manifest_rows(given.rows(), given.format()) {
        None => RowsPresence::Unknown,
        Some(table) if table.rows.is_empty() => RowsPresence::Empty,
        Some(_) => RowsPresence::NonEmpty,
    }
}

/// The lowercased leaf relation a given's `input` mocks: the **last
/// single-quoted argument** of a `ref(...)` / `source(...)` call —
/// `ref('stg_orders')` → `stg_orders`, `ref('pkg', 'stg_orders')` →
/// `stg_orders`, `source('raw', 'orders')` → `orders`. Mirrors the
/// renderer's given↔leaf-ref binding (cute-dbt#34/#131:
/// `render::parse_ref_name` + the source unwrap; duplicated here because
/// domain never imports adapters). `None` for `this` (incremental prior
/// state feeds the model itself, never an arm) and for any other shape.
fn given_input_leaf(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let is_call = ["ref", "source"].iter().any(|kw| {
        trimmed.get(..kw.len()).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case(kw) && trimmed[kw.len()..].trim_start().starts_with('(')
        })
    });
    if !is_call || !trimmed.ends_with(')') {
        return None;
    }
    let close = trimmed.rfind('\'')?;
    let open = trimmed[..close].rfind('\'')?;
    let name = trimmed[open + 1..close].trim();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// `true` when `leaf` (lowercased) is the leaf segment of a seed node's
/// id. dbt lets a seed input go ungiven (the test reads the real seed
/// file — verified against fusion `render_unit_test`, see module docs),
/// so an unbound seed relation never proves an arm unfed.
fn leaf_resolves_to_seed(manifest: &Manifest, leaf: &str) -> bool {
    manifest.nodes().values().any(|node| {
        node.resource_type() == "seed"
            && node
                .id()
                .as_str()
                .rsplit('.')
                .next()
                .is_some_and(|segment| segment.eq_ignore_ascii_case(leaf))
    })
}

/// The external relations feeding one union arm: walk the arm source's
/// upstream CTE closure over the graph's edges, union every node's
/// `body_leaf_table_refs`, and drop refs that are themselves CTE names
/// (internal plumbing, not model inputs). Pure reuse of the cute-dbt#40
/// single-parse facts — no SQL is re-parsed. Engine refs are already
/// lowercased.
fn arm_external_refs(graph: &CteGraph, arm: usize) -> BTreeSet<String> {
    let cte_names: BTreeSet<String> = graph
        .nodes()
        .iter()
        .map(|node| node.name().to_ascii_lowercase())
        .collect();
    let mut closure = BTreeSet::from([arm]);
    let mut frontier = vec![arm];
    while let Some(node) = frontier.pop() {
        for edge in graph.edges() {
            if edge.to() == node && closure.insert(edge.from()) {
                frontier.push(edge.from());
            }
        }
    }
    closure
        .into_iter()
        .filter_map(|index| graph.nodes().get(index))
        .flat_map(|node| node.body_leaf_table_refs().iter())
        .filter(|leaf| !cte_names.contains(*leaf))
        .cloned()
        .collect()
}

/// One unit test's contribution to one arm (refs = the arm's external
/// closure): `Fed` when a bound given provably carries rows; `Unknown`
/// when a bound given's count is unrecoverable OR an unbound ref is a
/// seed (real seed data flows); `Unfed` when every path provably
/// delivers zero rows.
fn arm_coverage_for_test(
    manifest: &Manifest,
    unit_test: &UnitTest,
    refs: &BTreeSet<String>,
) -> ArmCoverage {
    let mut unknown = false;
    let mut any_bound = BTreeSet::new();
    for given in unit_test.given() {
        let Some(leaf) = given_input_leaf(given.input()) else {
            continue;
        };
        if !refs.contains(&leaf) {
            continue;
        }
        any_bound.insert(leaf);
        match given_rows_presence(given) {
            RowsPresence::NonEmpty => return ArmCoverage::Fed,
            RowsPresence::Unknown => unknown = true,
            RowsPresence::Empty => {}
        }
    }
    let unbound_seed = refs
        .iter()
        .filter(|leaf| !any_bound.contains(*leaf))
        .any(|leaf| leaf_resolves_to_seed(manifest, leaf));
    if unknown || unbound_seed {
        ArmCoverage::Unknown
    } else {
        ArmCoverage::Unfed
    }
}

/// Render the `ref-list` half of an arm's evidence value.
fn refs_display(refs: &BTreeSet<String>) -> String {
    refs.iter()
        .map(|leaf| format!("ref('{leaf}')"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A copy-pasteable given-row YAML sketch for an unfed arm (the catalog
/// C3 recommendation payload): one `- input:` block per feeding
/// relation, with a column sketch from the input model's declared
/// `columns` when the manifest carries one.
fn suggested_given_sketch(manifest: &Manifest, refs: &BTreeSet<String>) -> String {
    let mut out = String::new();
    for leaf in refs {
        let columns = resolve_target_model(manifest, &NodeId::new(leaf))
            .map(|node| node.columns().keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let row = if columns.is_empty() {
            "{...}".to_owned()
        } else {
            let cells: Vec<String> = columns.iter().map(|c| format!("{c}: ...")).collect();
            format!("{{{}}}", cells.join(", "))
        };
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = write!(out, "- input: ref('{leaf}')\n  rows:\n    - {row}");
    }
    out
}

/// Detector for `union.arm-coverage` (cute-dbt#172, catalog class C3).
///
/// Trigger: a consumer node in the model's [`CteGraph`] with incoming
/// union-typed edges (`UnionAll` / `UnionDistinct`) — one finding per
/// consumer, discriminated as `union[<consumer>]`. The checked arms are
/// exactly the union-edge sources (join-free references to earlier CTEs
/// — the only arm shape the engine marks; everything else is the
/// declared visibility exclusion).
///
/// Satisfaction (the cute-dbt#172 Discovery settlement): an arm is
/// **exercised** when ≥1 given with ≥1 in-manifest row binds to any
/// external relation in the arm's upstream closure. A given whose
/// relation is shared by several arms exercises **all** of them — rows
/// provably reach each arm's scan; per-arm filter survival is
/// statically undecidable and deliberately out of scope (tier HIGH: a
/// cue, never an assertion). UNCOVERED therefore requires a **provably
/// unfed** arm; anything not statically attributable (unbindable
/// closure, external-fixture / non-literal-sql givens, ungiven seed
/// inputs) degrades to honest UNKNOWN.
fn detect_union_arm_coverage(ctx: &CheckContext<'_>) -> Vec<Finding<HeuristicId>> {
    if ctx.model.resource_type() != "model" {
        return Vec::new();
    }
    let Some(graph) = ctx.cte_graph else {
        return Vec::new();
    };
    let mut arms_by_consumer: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for edge in graph.edges() {
        if matches!(
            edge.edge_type(),
            EdgeType::UnionAll | EdgeType::UnionDistinct
        ) {
            arms_by_consumer
                .entry(edge.to())
                .or_default()
                .insert(edge.from());
        }
    }
    if arms_by_consumer.is_empty() {
        return Vec::new();
    }
    let mut tests: Vec<(&String, &UnitTest)> = ctx
        .manifest
        .unit_tests()
        .iter()
        .filter(|(_, ut)| {
            resolve_target_model(ctx.manifest, ut.model())
                .is_some_and(|model| model.id() == ctx.model.id())
        })
        .collect();
    tests.sort_by(|a, b| a.0.cmp(b.0));
    arms_by_consumer
        .into_iter()
        .map(|(consumer, arms)| union_finding(ctx, graph, &tests, consumer, &arms))
        .collect()
}

/// Build the per-consumer finding for [`detect_union_arm_coverage`].
fn union_finding(
    ctx: &CheckContext<'_>,
    graph: &CteGraph,
    tests: &[(&String, &UnitTest)],
    consumer: usize,
    arms: &BTreeSet<usize>,
) -> Finding<HeuristicId> {
    let node_name = |index: usize| {
        graph
            .nodes()
            .get(index)
            .map_or_else(String::new, |node| node.name().to_owned())
    };
    let consumer_name = node_name(consumer);
    let arm_names: Vec<String> = arms.iter().map(|&arm| node_name(arm)).collect();
    let mut evidence = vec![Evidence::new(
        "union",
        format!(
            "{consumer_name} ({} arm{}: {})",
            arms.len(),
            if arms.len() == 1 { "" } else { "s" },
            arm_names.join(", "),
        ),
    )];
    let mut unfed: Vec<(String, BTreeSet<String>)> = Vec::new();
    let mut unknown_evidence: Vec<Evidence> = Vec::new();
    let mut covered_by: BTreeSet<String> = BTreeSet::new();
    for &arm in arms {
        let arm_name = node_name(arm);
        let refs = arm_external_refs(graph, arm);
        if refs.is_empty() {
            unknown_evidence.push(Evidence::new(
                "unattributable arm",
                format!("{arm_name} — no resolvable upstream relation"),
            ));
            continue;
        }
        let mut fed_by: Vec<&str> = Vec::new();
        let mut unknown_in_a_test = false;
        for (id, unit_test) in tests {
            match arm_coverage_for_test(ctx.manifest, unit_test, &refs) {
                ArmCoverage::Fed => fed_by.push(id.as_str()),
                ArmCoverage::Unknown => unknown_in_a_test = true,
                ArmCoverage::Unfed => {}
            }
        }
        if !fed_by.is_empty() {
            covered_by.extend(fed_by.iter().map(|id| (*id).to_owned()));
        } else if unknown_in_a_test {
            unknown_evidence.push(Evidence::new(
                "unattributable arm",
                format!(
                    "{arm_name} — fed only by givens whose rows are not statically countable (reads {})",
                    refs_display(&refs),
                ),
            ));
        } else {
            unfed.push((arm_name, refs));
        }
    }
    for (arm_name, refs) in &unfed {
        evidence.push(Evidence::new(
            "unexercised arm",
            format!("{arm_name} — no given row reaches {}", refs_display(refs)),
        ));
        evidence.push(Evidence::new(
            "suggested given",
            suggested_given_sketch(ctx.manifest, refs),
        ));
    }
    evidence.extend(unknown_evidence.iter().cloned());
    let verdict = if !unfed.is_empty() {
        Verdict::Uncovered
    } else if !unknown_evidence.is_empty() {
        Verdict::Unknown
    } else {
        Verdict::Covered {
            by: covered_by.into_iter().collect(),
        }
    };
    Finding::new(
        HeuristicId::UnionArmCoverage,
        ctx.model.id().clone(),
        format!("union[{consumer_name}]"),
        verdict,
        evidence,
    )
}

// ---------------------------------------------------------------------
// Ledger generation — heuristics/registry.toml + book check pages,
// generated from SPECS, byte-gated by tests/heuristics_ledger.rs.
// ---------------------------------------------------------------------

/// The shared "do not edit" banner line for every generated artifact.
const GENERATED_BANNER: &str = "GENERATED \u{2014} do not edit. \
Source of truth: the `heuristics!` block in src/domain/checks.rs. \
Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger";

/// Render the full `heuristics/registry.toml` ledger from a registry's
/// `SPECS` — deterministic (declaration order), with the GENERATED
/// header. Humans author in the macro block; this file exists for the
/// book build and human reading (design sketch §5b).
#[must_use]
pub fn registry_toml<Id: CheckId>() -> String {
    let mut out = format!("# {GENERATED_BANNER}\n");
    for spec in Id::SPECS {
        out.push_str("\n[[heuristic]]\n");
        push_toml_string(&mut out, "id", spec.id_str);
        push_toml_string(&mut out, "name", spec.name);
        push_toml_string(&mut out, "group", spec.group);
        push_toml_string(&mut out, "tier", spec.tier.as_str());
        push_toml_string(&mut out, "instrument", spec.instrument.as_str());
        if !spec.supersedes.is_empty() {
            let ids: Vec<&str> = spec.supersedes.iter().map(|id| id.as_str()).collect();
            push_toml_array(&mut out, "supersedes", &ids);
        }
        push_toml_array(&mut out, "evidence", spec.evidence);
        push_toml_array(&mut out, "conditions", spec.conditions);
        push_toml_array(&mut out, "exclusions", spec.exclusions);
        push_toml_string(&mut out, "recommendation", spec.recommendation);
        push_toml_string(&mut out, "rationale", spec.rationale);
    }
    out
}

/// Render one generated book check page (`book/src/checks/<id>.md`).
#[must_use]
pub fn check_page_markdown<Id: CheckId>(id: Id) -> String {
    let spec = id.spec();
    let mut out = format!(
        "<!-- {GENERATED_BANNER} -->\n\n# {id_str}\n\n**{name}**\n\n\
         | | |\n|---|---|\n| Group | `{group}` |\n| Tier | `{tier}` |\n\
         | Instrument | `{instrument}` |\n",
        id_str = spec.id_str,
        name = spec.name,
        group = spec.group,
        tier = spec.tier.as_str(),
        instrument = spec.instrument.as_str(),
    );
    if !spec.supersedes.is_empty() {
        let ids: Vec<String> = spec
            .supersedes
            .iter()
            .map(|sup| format!("[`{0}`](./{0}.md)", sup.as_str()))
            .collect();
        let _ = writeln!(out, "| Supersedes | {} |", ids.join(", "));
    }
    push_markdown_list(&mut out, "Conditions", spec.conditions);
    push_markdown_list(&mut out, "Exclusions", spec.exclusions);
    let _ = write!(
        out,
        "\n## Recommendation\n\n{}\n\n## Rationale\n\n{}\n",
        spec.recommendation, spec.rationale
    );
    out
}

/// Render the generated checks index page (`book/src/checks/index.md`).
#[must_use]
pub fn checks_index_markdown<Id: CheckId>() -> String {
    let mut out = format!(
        "<!-- {GENERATED_BANNER} -->\n\n# Checks\n\n\
         The coverage-intelligence check registry. Each check pairs a \
         construct trigger with a satisfaction predicate and reports a \
         per-construct verdict: covered, uncovered, or unknown.\n\n\
         | Check | Name | Tier | Instrument |\n|---|---|---|---|\n",
    );
    for spec in Id::SPECS {
        let _ = writeln!(
            out,
            "| [`{id}`](./{id}.md) | {name} | `{tier}` | `{instrument}` |",
            id = spec.id_str,
            name = spec.name,
            tier = spec.tier.as_str(),
            instrument = spec.instrument.as_str(),
        );
    }
    out
}

/// Append `key = "escaped value"`.
fn push_toml_string(out: &mut String, key: &str, value: &str) {
    let _ = writeln!(out, "{key} = \"{}\"", toml_escape(value));
}

/// Append a multi-line TOML string array (empty ⇒ `key = []`).
fn push_toml_array(out: &mut String, key: &str, values: &[&str]) {
    if values.is_empty() {
        let _ = writeln!(out, "{key} = []");
        return;
    }
    let _ = writeln!(out, "{key} = [");
    for value in values {
        let _ = writeln!(out, "  \"{}\",", toml_escape(value));
    }
    out.push_str("]\n");
}

/// Append a `## <title>` markdown bullet list (omitted when empty).
fn push_markdown_list(out: &mut String, title: &str, items: &[&str]) {
    if items.is_empty() {
        return;
    }
    let _ = write!(out, "\n## {title}\n\n");
    for item in items {
        let _ = writeln!(out, "- {item}");
    }
}

/// Escape a string for a TOML basic (double-quoted) string: backslash,
/// double quote, and control characters (RFC-compliant `\uXXXX` for the
/// rare control case; the spec prose never carries one).
fn toml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, NodeConfig, TestMetadata,
    };
    use std::collections::{BTreeMap, HashMap};

    // ===== test scaffolding ==========================================

    fn node_id(id: &str) -> NodeId {
        NodeId::new(id)
    }

    /// A `model` node with an arbitrary flat config dict.
    fn model_with_config(full_id: &str, config: &[(&str, Value)]) -> Node {
        let map: BTreeMap<String, Value> = config
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", "x"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(map, false),
            None,
            BTreeMap::new(),
        )
    }

    /// A generic-test node attached to `attached`, with the given
    /// `test_metadata` and an optional flat config.
    fn test_node(
        full_id: &str,
        attached: &str,
        column_name: Option<&str>,
        metadata: TestMetadata,
        config: &[(&str, Value)],
    ) -> Node {
        let map: BTreeMap<String, Value> = config
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        Node::new(
            NodeId::new(full_id),
            "test",
            Checksum::new("none", ""),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(map, false),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            column_name.map(str::to_owned),
            Some(NodeId::new(attached)),
            Some(metadata),
        )
    }

    fn unique_metadata(column: &str) -> TestMetadata {
        TestMetadata::new("unique", None, serde_json::json!({ "column_name": column }))
    }

    fn combo_metadata(columns: &[&str]) -> TestMetadata {
        TestMetadata::new(
            "unique_combination_of_columns",
            Some("dbt_utils".to_owned()),
            serde_json::json!({ "combination_of_columns": columns }),
        )
    }

    fn manifest_of(nodes: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    /// Run the production pipeline on `model` within `manifest`
    /// (no CTE graph — the manifest-fact checks' path).
    fn run(manifest: &Manifest, model_id: &str) -> Vec<Finding<HeuristicId>> {
        run_with_graph(manifest, model_id, None)
    }

    /// Run the production pipeline on `model` with an optional graph.
    fn run_with_graph(
        manifest: &Manifest,
        model_id: &str,
        graph: Option<&CteGraph>,
    ) -> Vec<Finding<HeuristicId>> {
        let model = manifest
            .node(&NodeId::new(model_id))
            .expect("model node exists");
        model_findings(manifest, model, graph)
    }

    // ===== registry generation invariants ============================

    #[test]
    fn production_registry_has_one_spec_per_variant_in_order() {
        assert_eq!(HeuristicId::ALL.len(), HeuristicId::SPECS.len());
        for (variant, spec) in HeuristicId::ALL.iter().zip(HeuristicId::SPECS) {
            assert_eq!(*variant, spec.id, "ALL and SPECS share declaration order");
            assert_eq!(variant.spec().id, *variant, "spec() resolves to own spec");
        }
    }

    #[test]
    fn production_spec_fields_are_non_empty() {
        for spec in HeuristicId::SPECS {
            assert!(!spec.id_str.is_empty());
            assert!(!spec.name.is_empty());
            assert!(!spec.group.is_empty());
            assert!(
                spec.id_str.starts_with(&format!("{}.", spec.group)),
                "dotted id {} carries its group prefix {}",
                spec.id_str,
                spec.group
            );
            assert!(!spec.evidence.is_empty());
            assert!(!spec.conditions.is_empty());
            assert!(!spec.recommendation.is_empty());
            assert!(!spec.rationale.is_empty());
        }
    }

    #[test]
    fn production_supersedes_graph_is_acyclic() {
        assert!(supersedes_is_acyclic::<HeuristicId>());
    }

    #[test]
    fn as_str_is_the_dotted_id() {
        assert_eq!(
            HeuristicId::GrainUniqueKeyUnbacked.as_str(),
            "grain.unique-key-unbacked"
        );
    }

    // ===== synthetic multi-check registry for pipeline behaviour =====
    //
    // The production registry holds ONE walking-skeleton check, so the
    // multi-check pipeline properties (supersedes resolution, the
    // suppression-does-not-resurrect invariant, cycle detection) are
    // exercised against small synthetic registries generated by the SAME
    // heuristics! macro — proving the macro's guarantees hold for any
    // registry, not just today's.

    /// Test detectors key off marker entries in the model's config dict
    /// so each scenario controls exactly which checks fire.
    fn marker_detector<Id: CheckId>(
        marker: &'static str,
        check: Id,
        construct: &'static str,
    ) -> impl Fn(&CheckContext<'_>) -> Vec<Finding<Id>> {
        move |ctx: &CheckContext<'_>| {
            if ctx.model.config().config().contains_key(marker) {
                vec![Finding::new(
                    check,
                    ctx.model.id().clone(),
                    construct,
                    Verdict::Uncovered,
                    Vec::new(),
                )]
            } else {
                Vec::new()
            }
        }
    }

    heuristics! {
        /// Synthetic registry: Specific supersedes General; Unrelated
        /// stands apart. All three fire on the same construct when their
        /// marker is present.
        enum PipelineTestId {
            /// The general check the specific one silences.
            General {
                id: "join.general",
                name: "General join check",
                group: "join",
                tier: High,
                instrument: UnitTest,
                supersedes: [],
                evidence: ["cte-graph.join-edges"],
                conditions: ["a general join shape"],
                exclusions: [],
                recommendation: "general fix",
                rationale: "general rationale",
                detector: marker_detector("fire_general", PipelineTestId::General, "join#1"),
            },
            /// The specific check that supersedes the general one.
            Specific {
                id: "join.specific",
                name: "Specific join check",
                group: "join",
                tier: High,
                instrument: UnitTest,
                supersedes: [General],
                evidence: ["cte-graph.join-edges", "ast.where-predicates"],
                conditions: ["a more specific join shape"],
                exclusions: [],
                recommendation: "specific fix",
                rationale: "specific rationale",
                detector: marker_detector("fire_specific", PipelineTestId::Specific, "join#1"),
            },
            /// A check unrelated to the supersedes pair.
            Unrelated {
                id: "case.unrelated",
                name: "Unrelated check",
                group: "case",
                tier: Advisory,
                instrument: Both,
                supersedes: [],
                evidence: ["ast.case-arms"],
                conditions: ["an unrelated shape"],
                exclusions: [],
                recommendation: "unrelated fix",
                rationale: "unrelated rationale",
                detector: marker_detector("fire_unrelated", PipelineTestId::Unrelated, "case#1"),
            },
        }
    }

    fn pipeline_model(markers: &[&str]) -> Node {
        let config: Vec<(&str, Value)> = markers.iter().map(|m| (*m, Value::Bool(true))).collect();
        model_with_config("model.shop.pipeline", &config)
    }

    fn pipeline_run(markers: &[&str]) -> Vec<Finding<PipelineTestId>> {
        let model = pipeline_model(markers);
        let manifest = manifest_of(vec![model]);
        let model = manifest
            .node(&node_id("model.shop.pipeline"))
            .expect("model exists");
        let ctx = CheckContext {
            manifest: &manifest,
            model,
            cte_graph: None,
        };
        resolve_supersedes(evaluate_all::<PipelineTestId>(&ctx))
    }

    fn checks_of<Id: CheckId>(findings: &[Finding<Id>]) -> Vec<Id> {
        findings.iter().map(|f| f.check).collect()
    }

    #[test]
    fn evaluate_all_runs_every_registered_check_in_declaration_order() {
        let model = pipeline_model(&["fire_general", "fire_specific", "fire_unrelated"]);
        let manifest = manifest_of(vec![model]);
        let model = manifest
            .node(&node_id("model.shop.pipeline"))
            .expect("model exists");
        let ctx = CheckContext {
            manifest: &manifest,
            model,
            cte_graph: None,
        };
        let findings = evaluate_all::<PipelineTestId>(&ctx);
        assert_eq!(
            checks_of(&findings),
            vec![
                PipelineTestId::General,
                PipelineTestId::Specific,
                PipelineTestId::Unrelated
            ],
        );
    }

    #[test]
    fn resolve_drops_the_superseded_finding_on_the_shared_construct() {
        let findings = pipeline_run(&["fire_general", "fire_specific"]);
        assert_eq!(checks_of(&findings), vec![PipelineTestId::Specific]);
    }

    #[test]
    fn resolve_keeps_the_general_finding_when_the_specific_did_not_fire() {
        let findings = pipeline_run(&["fire_general"]);
        assert_eq!(checks_of(&findings), vec![PipelineTestId::General]);
    }

    #[test]
    fn resolve_never_touches_findings_on_other_constructs() {
        let findings = pipeline_run(&["fire_general", "fire_specific", "fire_unrelated"]);
        assert_eq!(
            checks_of(&findings),
            vec![PipelineTestId::Specific, PipelineTestId::Unrelated],
        );
    }

    #[test]
    fn resolve_is_scoped_per_model_not_global() {
        // The same (construct) string on two DIFFERENT models must not
        // cross-supersede: build findings directly to control model ids.
        let general_a = Finding::new(
            PipelineTestId::General,
            node_id("model.shop.a"),
            "join#1",
            Verdict::Uncovered,
            Vec::new(),
        );
        let specific_b = Finding::new(
            PipelineTestId::Specific,
            node_id("model.shop.b"),
            "join#1",
            Verdict::Uncovered,
            Vec::new(),
        );
        let resolved = resolve_supersedes(vec![general_a.clone(), specific_b.clone()]);
        assert_eq!(resolved, vec![general_a, specific_b]);
    }

    /// THE required engine test (cute-dbt#169 AC): suppressing/disabling
    /// the SUPERSEDING check must not resurrect the superseded finding —
    /// guaranteed by the fixed pipeline order (evaluate ALL → resolve →
    /// display filter), because disabled checks still evaluate and still
    /// participate in resolution.
    #[test]
    fn disabling_the_superseding_check_does_not_resurrect_the_superseded_finding() {
        let resolved = pipeline_run(&["fire_general", "fire_specific"]);
        let displayed = filter_for_display(resolved, &[PipelineTestId::Specific]);
        assert!(
            displayed.is_empty(),
            "disabling Specific must remove it WITHOUT resurrecting General; got {displayed:?}"
        );
    }

    #[test]
    fn display_filter_removes_only_the_disabled_checks_findings() {
        let resolved = pipeline_run(&["fire_general", "fire_unrelated"]);
        let displayed = filter_for_display(resolved, &[PipelineTestId::Unrelated]);
        assert_eq!(checks_of(&displayed), vec![PipelineTestId::General]);
    }

    // Property-style invariants over the resolution stage: for every
    // subset of firing checks, (1) the resolved set is a subset of the
    // evaluated set, (2) a finding is dropped IFF a distinct fired
    // finding on the same (model, construct) supersedes it, and (3) a
    // check with no incoming supersedes edge always survives.
    #[test]
    fn resolve_supersedes_invariants_hold_for_every_firing_subset() {
        let markers = ["fire_general", "fire_specific", "fire_unrelated"];
        for mask in 0u8..8 {
            let firing: Vec<&str> = markers
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, m)| *m)
                .collect();
            let model = pipeline_model(&firing);
            let manifest = manifest_of(vec![model]);
            let model = manifest
                .node(&node_id("model.shop.pipeline"))
                .expect("model exists");
            let ctx = CheckContext {
                manifest: &manifest,
                model,
                cte_graph: None,
            };
            let evaluated = evaluate_all::<PipelineTestId>(&ctx);
            let resolved = resolve_supersedes(evaluated.clone());
            // (1) subset.
            assert!(
                resolved.iter().all(|f| evaluated.contains(f)),
                "mask {mask}: resolved ⊆ evaluated"
            );
            // (2) exact drop condition.
            for finding in &evaluated {
                let superseded = evaluated.iter().any(|other| {
                    other.check != finding.check
                        && other.model_id == finding.model_id
                        && other.construct == finding.construct
                        && other.check.spec().supersedes.contains(&finding.check)
                });
                assert_eq!(
                    !superseded,
                    resolved.contains(finding),
                    "mask {mask}: drop iff superseded-by-fired ({:?})",
                    finding.check
                );
            }
            // (3) Unrelated has no incoming edge — always survives.
            if firing.contains(&"fire_unrelated") {
                assert!(checks_of(&resolved).contains(&PipelineTestId::Unrelated));
            }
        }
    }

    heuristics! {
        /// A deliberately CYCLIC registry — compiles (the variants
        /// exist), but the acyclicity gate must reject it.
        enum CyclicTestId {
            /// A supersedes B.
            CycleA {
                id: "cycle.a",
                name: "Cycle A",
                group: "cycle",
                tier: Advisory,
                instrument: Both,
                supersedes: [CycleB],
                evidence: ["none"],
                conditions: ["never fires"],
                exclusions: [],
                recommendation: "n/a",
                rationale: "n/a",
                detector: |_ctx: &CheckContext<'_>| Vec::new(),
            },
            /// B supersedes A — the cycle.
            CycleB {
                id: "cycle.b",
                name: "Cycle B",
                group: "cycle",
                tier: Advisory,
                instrument: Both,
                supersedes: [CycleA],
                evidence: ["none"],
                conditions: ["never fires"],
                exclusions: [],
                recommendation: "n/a",
                rationale: "n/a",
                detector: |_ctx: &CheckContext<'_>| Vec::new(),
            },
        }
    }

    #[test]
    fn acyclicity_gate_detects_a_cycle() {
        assert!(supersedes_is_acyclic::<PipelineTestId>());
        assert!(!supersedes_is_acyclic::<CyclicTestId>());
    }

    // ===== Finding / Verdict PODs ====================================

    #[test]
    fn finding_new_denormalizes_spec_fields() {
        let finding = Finding::new(
            HeuristicId::GrainUniqueKeyUnbacked,
            node_id("model.shop.orders"),
            UNIQUE_KEY_CONSTRUCT,
            Verdict::Uncovered,
            vec![Evidence::new("unique_key", "order_id")],
        );
        assert_eq!(finding.tier, Tier::Total);
        assert_eq!(finding.instrument, Instrument::DataTest);
        assert_eq!(
            finding.recommendation.as_deref(),
            Some(HeuristicId::GrainUniqueKeyUnbacked.spec().recommendation),
        );
    }

    #[test]
    fn finding_new_omits_the_recommendation_unless_uncovered() {
        for verdict in [
            Verdict::Covered {
                by: vec!["test.shop.unique_orders_order_id".to_owned()],
            },
            Verdict::Unknown,
        ] {
            let finding = Finding::new(
                HeuristicId::GrainUniqueKeyUnbacked,
                node_id("model.shop.orders"),
                UNIQUE_KEY_CONSTRUCT,
                verdict,
                Vec::new(),
            );
            assert!(finding.recommendation.is_none());
        }
    }

    #[test]
    fn finding_serializes_check_as_dotted_id_and_tagged_verdict() {
        let finding = Finding::new(
            HeuristicId::GrainUniqueKeyUnbacked,
            node_id("model.shop.orders"),
            UNIQUE_KEY_CONSTRUCT,
            Verdict::Covered {
                by: vec!["test.shop.unique_orders_order_id".to_owned()],
            },
            vec![Evidence::new("unique_key", "order_id")],
        );
        let json = serde_json::to_value(&finding).expect("finding serializes");
        assert_eq!(json["check"], "grain.unique-key-unbacked");
        assert_eq!(json["tier"], "total");
        assert_eq!(json["instrument"], "data-test");
        assert_eq!(json["model_id"], "model.shop.orders");
        assert_eq!(json["construct"], "config.unique_key");
        assert_eq!(json["verdict"]["status"], "covered");
        assert_eq!(json["verdict"]["by"][0], "test.shop.unique_orders_order_id");
        assert_eq!(json["evidence"][0]["label"], "unique_key");
        // Covered ⇒ no recommendation key at all.
        assert!(json.get("recommendation").is_none());
    }

    #[test]
    fn uncovered_and_unknown_verdicts_serialize_as_status_only() {
        assert_eq!(
            serde_json::to_value(Verdict::Uncovered).expect("serializes"),
            serde_json::json!({ "status": "uncovered" }),
        );
        assert_eq!(
            serde_json::to_value(Verdict::Unknown).expect("serializes"),
            serde_json::json!({ "status": "unknown" }),
        );
    }

    // ===== grain.unique-key-unbacked detector ========================

    const ORDERS: &str = "model.shop.orders";

    fn orders_with_key(key: Value) -> Node {
        model_with_config(
            ORDERS,
            &[
                ("materialized", Value::from("incremental")),
                ("unique_key", key),
            ],
        )
    }

    fn single_finding(findings: Vec<Finding<HeuristicId>>) -> Finding<HeuristicId> {
        assert_eq!(findings.len(), 1, "exactly one grain finding: {findings:?}");
        findings.into_iter().next().expect("one finding")
    }

    #[test]
    fn no_unique_key_means_no_finding() {
        let manifest = manifest_of(vec![model_with_config(
            ORDERS,
            &[("materialized", Value::from("table"))],
        )]);
        assert!(run(&manifest, ORDERS).is_empty());
    }

    #[test]
    fn explicit_null_unique_key_means_no_finding() {
        // fusion null-fills unset Option config fields (cute-dbt#145).
        let manifest = manifest_of(vec![orders_with_key(Value::Null)]);
        assert!(run(&manifest, ORDERS).is_empty());
    }

    #[test]
    fn unbacked_single_key_is_uncovered_with_evidence_and_recommendation() {
        let manifest = manifest_of(vec![orders_with_key(Value::from("order_id"))]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert_eq!(finding.construct, UNIQUE_KEY_CONSTRUCT);
        assert_eq!(
            finding.evidence,
            vec![Evidence::new("unique_key", "order_id")]
        );
        assert!(finding.recommendation.is_some());
    }

    #[test]
    fn enabled_unique_test_on_the_key_column_covers_with_attribution() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            test_node(
                "test.shop.unique_orders_order_id",
                ORDERS,
                Some("order_id"),
                unique_metadata("order_id"),
                &[("enabled", Value::Bool(true))],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec!["test.shop.unique_orders_order_id".to_owned()],
            },
        );
        assert!(finding.recommendation.is_none());
    }

    #[test]
    fn unique_test_on_a_subset_of_a_composite_key_covers() {
        // Uniqueness of one key column implies uniqueness at the wider
        // composite grain (⊆ direction).
        let manifest = manifest_of(vec![
            orders_with_key(serde_json::json!(["customer_id", "order_date"])),
            test_node(
                "test.shop.unique_orders_customer_id",
                ORDERS,
                Some("customer_id"),
                unique_metadata("customer_id"),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn combination_test_on_exactly_the_composite_key_covers() {
        let manifest = manifest_of(vec![
            orders_with_key(serde_json::json!(["customer_id", "order_date"])),
            test_node(
                "test.shop.combo_orders",
                ORDERS,
                None,
                combo_metadata(&["order_date", "customer_id"]),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec!["test.shop.combo_orders".to_owned()],
            },
        );
    }

    #[test]
    fn combination_test_wider_than_the_key_does_not_cover() {
        // THE anti-flattening case: fusion's PK inference flattens a
        // combination test into per-column uniqueness claims; copying
        // that here would let a {order_id, order_date} combo "cover" a
        // single-column order_id key. Pair-uniqueness does NOT imply
        // per-column uniqueness — the composite set must stay whole and
        // the ⊆ test must fail.
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            test_node(
                "test.shop.combo_orders_wide",
                ORDERS,
                None,
                combo_metadata(&["order_id", "order_date"]),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn disabled_uniqueness_test_does_not_cover() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            test_node(
                "test.shop.unique_orders_order_id",
                ORDERS,
                Some("order_id"),
                unique_metadata("order_id"),
                &[("enabled", Value::Bool(false))],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn unique_test_on_a_non_key_column_does_not_cover() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            test_node(
                "test.shop.unique_orders_status",
                ORDERS,
                Some("status"),
                unique_metadata("status"),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn non_uniqueness_tests_never_cover() {
        // not_null on the key column proves presence, not grain.
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            test_node(
                "test.shop.not_null_orders_order_id",
                ORDERS,
                Some("order_id"),
                TestMetadata::new(
                    "not_null",
                    None,
                    serde_json::json!({ "column_name": "order_id" }),
                ),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn a_test_attached_to_another_model_does_not_cover() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            test_node(
                "test.shop.unique_other_order_id",
                "model.shop.other",
                Some("order_id"),
                unique_metadata("order_id"),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn column_match_is_ascii_case_insensitive() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("Order_ID")),
            test_node(
                "test.shop.unique_orders_order_id",
                ORDERS,
                Some("order_id"),
                unique_metadata("order_id"),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn attribution_lists_every_satisfying_test_sorted() {
        let manifest = manifest_of(vec![
            orders_with_key(serde_json::json!(["customer_id", "order_date"])),
            test_node(
                "test.shop.b_combo",
                ORDERS,
                None,
                combo_metadata(&["customer_id", "order_date"]),
                &[],
            ),
            test_node(
                "test.shop.a_unique_customer_id",
                ORDERS,
                Some("customer_id"),
                unique_metadata("customer_id"),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec![
                    "test.shop.a_unique_customer_id".to_owned(),
                    "test.shop.b_combo".to_owned(),
                ],
            },
        );
    }

    #[test]
    fn unique_kwargs_fallback_to_node_column_name() {
        // Belt-and-braces: an engine omitting kwargs.column_name still
        // resolves via the node-level column_name (cute-dbt#166 field).
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            test_node(
                "test.shop.unique_orders_order_id",
                ORDERS,
                Some("order_id"),
                TestMetadata::new("unique", None, Value::Null),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn unrecognized_unique_key_is_unknown_never_uncovered() {
        // Paired negative test for the declared exclusion: a
        // non-literal key shape is honest UNKNOWN (and carries no
        // recommendation), never a nagged gap.
        for key in [Value::from(42), serde_json::json!(["a", 7])] {
            let manifest = manifest_of(vec![orders_with_key(key)]);
            let finding = single_finding(run(&manifest, ORDERS));
            assert_eq!(finding.verdict, Verdict::Unknown);
            assert!(finding.recommendation.is_none());
            assert_eq!(finding.evidence.len(), 1, "raw value surfaced as evidence");
        }
    }

    #[test]
    fn empty_array_unique_key_is_unknown() {
        let manifest = manifest_of(vec![orders_with_key(serde_json::json!([]))]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Unknown);
    }

    #[test]
    fn non_model_nodes_never_trigger() {
        // A snapshot also carries unique_key on the wire; the check is
        // scoped to model nodes (the run loop only feeds models anyway —
        // this pins the detector's own guard).
        let mut config = BTreeMap::new();
        config.insert("unique_key".to_owned(), Value::from("patient_id"));
        let snapshot = Node::new(
            NodeId::new("snapshot.shop.snp_patients"),
            "snapshot",
            Checksum::new("sha256", "x"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_of(vec![snapshot]);
        let node = manifest
            .node(&node_id("snapshot.shop.snp_patients"))
            .expect("node exists");
        assert!(model_findings(&manifest, node, None).is_empty());
    }

    #[test]
    fn combination_kwargs_with_non_string_entries_do_not_cover() {
        let manifest = manifest_of(vec![
            orders_with_key(serde_json::json!(["customer_id"])),
            test_node(
                "test.shop.combo_bad",
                ORDERS,
                None,
                TestMetadata::new(
                    "unique_combination_of_columns",
                    Some("dbt_utils".to_owned()),
                    serde_json::json!({ "combination_of_columns": ["customer_id", 5] }),
                ),
                &[],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    // ===== union.arm-coverage detector ===============================

    use crate::domain::cte::{CteEdge, CteNode};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect, UnitTestGiven};

    const PAYMENTS: &str = "model.shop.fct_payments";

    /// A CTE node carrying engine-computed leaf refs (already lowercase,
    /// the engine contract).
    fn cte(name: &str, refs: &[&str]) -> CteNode {
        CteNode::new(name, None, None, None)
            .with_shape_facts(false, refs.iter().map(|r| (*r).to_owned()).collect())
    }

    /// The catalog C3 worked-example graph: `charges` + `refunds` import
    /// CTEs union-ALLed by the terminal select.
    fn charges_refunds_graph() -> CteGraph {
        CteGraph::new(
            vec![
                cte("charges", &["stg_charges"]),
                cte("refunds", &["stg_refunds"]),
                cte("(final select)", &["charges", "refunds"]),
            ],
            vec![
                CteEdge::new(0, 2, EdgeType::UnionAll),
                CteEdge::new(1, 2, EdgeType::UnionAll),
            ],
        )
    }

    fn payments_model() -> Node {
        // No unique_key — the grain check stays silent, so every
        // finding asserted below is the union check's.
        model_with_config(PAYMENTS, &[("materialized", Value::from("table"))])
    }

    fn given(input: &str, rows: Value) -> UnitTestGiven {
        UnitTestGiven::new(input, rows, Some("dict".to_owned()), None)
    }

    fn unit_test_on_payments(givens: Vec<UnitTestGiven>) -> UnitTest {
        UnitTest::new(
            "test_fct_payments",
            NodeId::new("fct_payments"),
            givens,
            UnitTestExpect::new(serde_json::json!([{ "payment_id": 1 }]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    fn manifest_with_unit_tests(nodes: Vec<Node>, tests: Vec<(&str, UnitTest)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            tests
                .into_iter()
                .map(|(id, t)| (id.to_owned(), t))
                .collect(),
            HashMap::new(),
        )
    }

    #[test]
    fn union_without_a_graph_emits_no_finding() {
        // No graph evidence is not evidence of absence — silent.
        let manifest = manifest_of(vec![payments_model()]);
        assert!(run_with_graph(&manifest, PAYMENTS, None).is_empty());
    }

    #[test]
    fn union_graph_without_union_edges_is_silent() {
        let graph = CteGraph::new(
            vec![cte("a", &["stg_charges"]), cte("(final select)", &["a"])],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        );
        let manifest = manifest_of(vec![payments_model()]);
        assert!(run_with_graph(&manifest, PAYMENTS, Some(&graph)).is_empty());
    }

    #[test]
    fn union_with_zero_unit_tests_is_uncovered_listing_every_arm() {
        let graph = charges_refunds_graph();
        let manifest = manifest_of(vec![payments_model()]);
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(finding.check, HeuristicId::UnionArmCoverage);
        assert_eq!(finding.tier, Tier::High);
        assert_eq!(finding.instrument, Instrument::UnitTest);
        assert_eq!(finding.construct, "union[(final select)]");
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert!(finding.recommendation.is_some());
        let unexercised: Vec<&str> = finding
            .evidence
            .iter()
            .filter(|e| e.label == "unexercised arm")
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(unexercised.len(), 2, "both arms listed: {unexercised:?}");
        assert!(unexercised[0].contains("charges"));
        assert!(unexercised[1].contains("refunds"));
    }

    #[test]
    fn union_with_every_arm_fed_is_covered_with_attribution() {
        let graph = charges_refunds_graph();
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                    given("ref('stg_refunds')", serde_json::json!([{ "amount": 40 }])),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec!["unit_test.shop.fct_payments.test_fct_payments".to_owned()],
            },
        );
        assert!(finding.recommendation.is_none());
    }

    #[test]
    fn union_with_an_empty_given_arm_is_uncovered_with_a_concrete_sketch() {
        // The catalog C3 worked example: the refunds given is mocked
        // EMPTY — the arm provably contributes zero rows to every test.
        // The recommendation evidence carries a copy-pasteable given-row
        // sketch with the input model's declared columns.
        let graph = charges_refunds_graph();
        let mut columns = BTreeMap::new();
        columns.insert("payment_id".to_owned(), None);
        columns.insert("amount".to_owned(), None);
        let stg_refunds = Node::new(
            NodeId::new("model.shop.stg_refunds"),
            "model",
            Checksum::new("sha256", "y"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(BTreeMap::new(), false),
            None,
            columns,
        );
        let manifest = manifest_with_unit_tests(
            vec![payments_model(), stg_refunds],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                    given("ref('stg_refunds')", serde_json::json!([])),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        let unexercised: Vec<&str> = finding
            .evidence
            .iter()
            .filter(|e| e.label == "unexercised arm")
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(
            unexercised,
            vec!["refunds — no given row reaches ref('stg_refunds')"]
        );
        let sketch = finding
            .evidence
            .iter()
            .find(|e| e.label == "suggested given")
            .expect("sketch evidence present");
        assert_eq!(
            sketch.value,
            "- input: ref('stg_refunds')\n  rows:\n    - {amount: ..., payment_id: ...}",
        );
    }

    #[test]
    fn union_arm_fed_transitively_through_the_cte_chain_is_covered() {
        // The arm CTE reads an intermediate CTE; the given binds to the
        // EXTERNAL relation at the bottom of the closure.
        let graph = CteGraph::new(
            vec![
                cte("base", &["stg_charges"]),
                cte("charges", &["base"]),
                cte("refunds", &["stg_refunds"]),
                cte("(final select)", &["charges", "refunds"]),
            ],
            vec![
                CteEdge::new(0, 1, EdgeType::From),
                CteEdge::new(1, 3, EdgeType::UnionAll),
                CteEdge::new(2, 3, EdgeType::UnionAll),
            ],
        );
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                    given("ref('stg_refunds')", serde_json::json!([{ "amount": 40 }])),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn union_shared_ref_given_exercises_every_arm_it_reaches() {
        // THE cute-dbt#172 Discovery settlement, encoded: two arms read
        // the SAME external relation (filter-split arms). A non-empty
        // given for it provably reaches BOTH arms' scans, so both count
        // as exercised at the input level — per-arm filter survival is
        // out of scope (no predicate evaluation; tier HIGH, cue not
        // assertion). Conservative direction: never a false UNCOVERED.
        let graph = CteGraph::new(
            vec![
                cte("completed", &["stg_payments"]),
                cte("other", &["stg_payments"]),
                cte("(final select)", &["completed", "other"]),
            ],
            vec![
                CteEdge::new(0, 2, EdgeType::UnionAll),
                CteEdge::new(1, 2, EdgeType::UnionAll),
            ],
        );
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![given(
                    "ref('stg_payments')",
                    serde_json::json!([{ "status": "completed" }]),
                )]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert!(
            matches!(finding.verdict, Verdict::Covered { .. }),
            "shared-ref arms are both input-fed, never flagged: {:?}",
            finding.verdict,
        );
    }

    #[test]
    fn union_constant_arm_is_unknown_never_uncovered() {
        // Paired negative test for the declared exclusion: an arm with
        // no resolvable upstream relation (sentinel/constant SELECT)
        // cannot be bound to any given — honest UNKNOWN, with no
        // recommendation, never a nagged gap.
        let graph = CteGraph::new(
            vec![
                cte("unknown_member", &[]),
                cte("sequenced", &["stg_payers"]),
                cte("(final select)", &["unknown_member", "sequenced"]),
            ],
            vec![
                CteEdge::new(0, 2, EdgeType::UnionAll),
                CteEdge::new(1, 2, EdgeType::UnionAll),
            ],
        );
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![given(
                    "ref('stg_payers')",
                    serde_json::json!([{ "payer_id": 1 }]),
                )]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Unknown);
        assert!(finding.recommendation.is_none());
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "unattributable arm" && e.value.contains("unknown_member")),
        );
    }

    #[test]
    fn union_external_fixture_given_is_unknown_never_uncovered() {
        // Paired negative test for the declared exclusion: an external
        // `fixture:` given's rows live on disk (cute-dbt#126), so its
        // row count is statically unknowable — the arm degrades to
        // UNKNOWN, never UNCOVERED.
        let graph = charges_refunds_graph();
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                    UnitTestGiven::new(
                        "ref('stg_refunds')",
                        Value::Null,
                        Some("csv".to_owned()),
                        Some("refunds_fixture".to_owned()),
                    ),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Unknown);
        assert!(finding.recommendation.is_none());
    }

    #[test]
    fn union_unbound_seed_input_is_unknown_when_tests_exist() {
        // Paired negative test for the declared exclusion: dbt lets a
        // seed input go ungiven (the test reads the real seed file —
        // fusion `render_unit_test` mocks only `given` entries), so an
        // unbound seed relation never proves the arm unfed.
        let graph = CteGraph::new(
            vec![
                cte("charges", &["stg_charges"]),
                cte("manual_adjustments", &["raw_adjustments"]),
                cte("(final select)", &["charges", "manual_adjustments"]),
            ],
            vec![
                CteEdge::new(0, 2, EdgeType::UnionAll),
                CteEdge::new(1, 2, EdgeType::UnionAll),
            ],
        );
        let seed = Node::new(
            NodeId::new("seed.shop.raw_adjustments"),
            "seed",
            Checksum::new("sha256", "z"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(BTreeMap::new(), false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_with_unit_tests(
            vec![payments_model(), seed],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![given(
                    "ref('stg_charges')",
                    serde_json::json!([{ "amount": 100 }]),
                )]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(
            finding.verdict,
            Verdict::Unknown,
            "ungiven seed input must not nag a gap",
        );
    }

    #[test]
    fn union_this_given_never_feeds_an_arm() {
        // Paired negative test for the declared exclusion: a non-empty
        // `this` given (incremental prior state) feeds the model itself,
        // never a union arm — the unbound arm stays provably unfed.
        let graph = charges_refunds_graph();
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                    given("this", serde_json::json!([{ "payment_id": 1 }])),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "unexercised arm" && e.value.contains("refunds")),
        );
    }

    #[test]
    fn union_distinct_arms_are_checked_too() {
        let graph = CteGraph::new(
            vec![
                cte("charges", &["stg_charges"]),
                cte("refunds", &["stg_refunds"]),
                cte("(final select)", &["charges", "refunds"]),
            ],
            vec![
                CteEdge::new(0, 2, EdgeType::UnionDistinct),
                CteEdge::new(1, 2, EdgeType::UnionDistinct),
            ],
        );
        let manifest = manifest_of(vec![payments_model()]);
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn union_sql_format_given_counts_when_literal_only() {
        // A literal-row `format: sql` given produces a statically known
        // row count (cute-dbt#137 parse) — it feeds the arm. A
        // non-literal SELECT does not parse to rows — honest UNKNOWN.
        let graph = charges_refunds_graph();
        let literal = UnitTestGiven::new(
            "ref('stg_refunds')",
            Value::from("select 1 as refund_id"),
            Some("sql".to_owned()),
            None,
        );
        let opaque = UnitTestGiven::new(
            "ref('stg_refunds')",
            Value::from("select * from somewhere"),
            Some("sql".to_owned()),
            None,
        );
        for (g, expected) in [
            (literal, None::<Verdict>), // Covered — asserted via matches! below
            (opaque, Some(Verdict::Unknown)),
        ] {
            let manifest = manifest_with_unit_tests(
                vec![payments_model()],
                vec![(
                    "unit_test.shop.fct_payments.test_fct_payments",
                    unit_test_on_payments(vec![
                        given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                        g,
                    ]),
                )],
            );
            let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
            match expected {
                Some(v) => assert_eq!(finding.verdict, v),
                None => assert!(matches!(finding.verdict, Verdict::Covered { .. })),
            }
        }
    }

    #[test]
    fn union_header_only_csv_given_is_provably_empty() {
        // A fusion csv-string given with a header and no data rows
        // carries zero rows — the arm is provably unfed.
        let graph = charges_refunds_graph();
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                    UnitTestGiven::new(
                        "ref('stg_refunds')",
                        Value::from("refund_id,amount\n"),
                        Some("csv".to_owned()),
                        None,
                    ),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn union_given_binding_is_ascii_case_insensitive() {
        let graph = charges_refunds_graph();
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("REF('STG_CHARGES')", serde_json::json!([{ "amount": 100 }])),
                    given("Ref('Stg_Refunds')", serde_json::json!([{ "amount": 40 }])),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn union_source_given_binds_by_last_quoted_argument() {
        let graph = CteGraph::new(
            vec![
                cte("charges", &["stg_charges"]),
                cte("raw_refunds", &["refunds"]),
                cte("(final select)", &["charges", "raw_refunds"]),
            ],
            vec![
                CteEdge::new(0, 2, EdgeType::UnionAll),
                CteEdge::new(1, 2, EdgeType::UnionAll),
            ],
        );
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![(
                "unit_test.shop.fct_payments.test_fct_payments",
                unit_test_on_payments(vec![
                    given("ref('stg_charges')", serde_json::json!([{ "amount": 100 }])),
                    given(
                        "source('billing', 'refunds')",
                        serde_json::json!([{ "amount": 40 }]),
                    ),
                ]),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn union_multiple_consumers_emit_one_finding_each() {
        // Two distinct union sites in one model (the dogfood
        // order_metrics shape: a UNION ALL consumer and a UNION
        // DISTINCT consumer) — one finding per consumer, discriminated
        // by construct, in declaration order.
        let graph = CteGraph::new(
            vec![
                cte("charges", &["stg_charges"]),
                cte("refunds", &["stg_refunds"]),
                cte("all_rows", &["charges", "refunds"]),
                cte("status_dim", &["charges", "refunds"]),
                cte("(final select)", &["all_rows", "status_dim"]),
            ],
            vec![
                CteEdge::new(0, 2, EdgeType::UnionAll),
                CteEdge::new(1, 2, EdgeType::UnionAll),
                CteEdge::new(0, 3, EdgeType::UnionDistinct),
                CteEdge::new(1, 3, EdgeType::UnionDistinct),
                CteEdge::new(2, 4, EdgeType::From),
            ],
        );
        let manifest = manifest_of(vec![payments_model()]);
        let findings = run_with_graph(&manifest, PAYMENTS, Some(&graph));
        assert_eq!(findings.len(), 2, "one finding per union consumer");
        assert_eq!(findings[0].construct, "union[all_rows]");
        assert_eq!(findings[1].construct, "union[status_dim]");
    }

    #[test]
    fn union_check_skips_non_model_nodes() {
        let graph = charges_refunds_graph();
        let snapshot = Node::new(
            NodeId::new("snapshot.shop.snp_payments"),
            "snapshot",
            Checksum::new("sha256", "x"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(BTreeMap::new(), false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_of(vec![snapshot]);
        let node = manifest
            .node(&node_id("snapshot.shop.snp_payments"))
            .expect("node exists");
        assert!(model_findings(&manifest, node, Some(&graph)).is_empty());
    }

    #[test]
    fn union_attribution_lists_every_feeding_test_sorted() {
        // Two tests each feed one arm — coverage is per-arm any-test,
        // and BOTH tests are attributed, sorted by id.
        let graph = charges_refunds_graph();
        let charges_only = unit_test_on_payments(vec![given(
            "ref('stg_charges')",
            serde_json::json!([{ "amount": 100 }]),
        )]);
        let refunds_only = unit_test_on_payments(vec![given(
            "ref('stg_refunds')",
            serde_json::json!([{ "amount": 40 }]),
        )]);
        let manifest = manifest_with_unit_tests(
            vec![payments_model()],
            vec![
                ("unit_test.shop.fct_payments.b_refunds", refunds_only),
                ("unit_test.shop.fct_payments.a_charges", charges_only),
            ],
        );
        let finding = single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph)));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec![
                    "unit_test.shop.fct_payments.a_charges".to_owned(),
                    "unit_test.shop.fct_payments.b_refunds".to_owned(),
                ],
            },
        );
    }

    // ===== ledger generation =========================================

    #[test]
    fn registry_toml_is_deterministic_and_carries_the_generated_header() {
        let toml = registry_toml::<HeuristicId>();
        assert_eq!(toml, registry_toml::<HeuristicId>());
        assert!(toml.starts_with("# GENERATED \u{2014} do not edit."));
        assert!(toml.contains("[[heuristic]]"));
        assert!(toml.contains("id = \"grain.unique-key-unbacked\""));
        assert!(toml.contains("tier = \"total\""));
        assert!(toml.contains("instrument = \"data-test\""));
    }

    #[test]
    fn registry_toml_parses_as_toml_and_mirrors_specs() {
        // The dev-dep `toml` crate is already a runtime dep; round-trip
        // the generated ledger to prove it is structurally valid TOML
        // with one [[heuristic]] entry per spec.
        let parsed: toml::Value =
            toml::from_str(&registry_toml::<HeuristicId>()).expect("ledger parses as TOML");
        let entries = parsed["heuristic"].as_array().expect("array of tables");
        assert_eq!(entries.len(), HeuristicId::SPECS.len());
        assert_eq!(entries[0]["id"].as_str(), Some("grain.unique-key-unbacked"));
        assert_eq!(
            entries[0]["conditions"]
                .as_array()
                .expect("conditions array")
                .len(),
            HeuristicId::GrainUniqueKeyUnbacked.spec().conditions.len(),
        );
    }

    #[test]
    fn registry_toml_emits_supersedes_only_when_present() {
        let toml = registry_toml::<PipelineTestId>();
        // Specific carries the edge; General/Unrelated omit the key.
        assert!(toml.contains("supersedes = [\n  \"join.general\",\n]"));
        let general_block = toml
            .split("[[heuristic]]")
            .find(|block| block.contains("id = \"join.general\""))
            .expect("general block present");
        assert!(!general_block.contains("supersedes"));
    }

    #[test]
    fn check_page_markdown_carries_every_spec_section() {
        let page = check_page_markdown(HeuristicId::GrainUniqueKeyUnbacked);
        assert!(page.starts_with("<!-- GENERATED \u{2014} do not edit."));
        assert!(page.contains("# grain.unique-key-unbacked"));
        assert!(page.contains("## Conditions"));
        assert!(page.contains("## Exclusions"));
        assert!(page.contains("## Recommendation"));
        assert!(page.contains("## Rationale"));
        assert!(page.contains("| Tier | `total` |"));
    }

    #[test]
    fn check_page_markdown_links_supersedes_edges() {
        let page = check_page_markdown(PipelineTestId::Specific);
        assert!(page.contains("| Supersedes | [`join.general`](./join.general.md) |"));
    }

    #[test]
    fn checks_index_lists_every_check() {
        let index = checks_index_markdown::<HeuristicId>();
        assert!(index.starts_with("<!-- GENERATED \u{2014} do not edit."));
        for spec in HeuristicId::SPECS {
            assert!(
                index.contains(&format!("[`{0}`](./{0}.md)", spec.id_str)),
                "index links {}",
                spec.id_str
            );
        }
    }

    #[test]
    fn toml_escape_handles_quotes_backslashes_and_control_chars() {
        assert_eq!(
            toml_escape(r#"a "quoted" \ path"#),
            r#"a \"quoted\" \\ path"#
        );
        assert_eq!(toml_escape("line\nbreak\ttab"), "line\\nbreak\\ttab");
        assert_eq!(toml_escape("bell\u{7}"), "bell\\u0007");
    }
}
