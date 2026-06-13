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
//! The `heuristics!` block below is the registry (the committed
//! `heuristics/registry.toml` ledger is generated from it). Wire-shape
//! verification provenance for the manifest-fact checks:
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
//! - `incremental.branch-coverage` (HIGH, cute-dbt#164 —
//!   coverage-intelligence rule #1) — an incremental model's unit tests
//!   exercise only one side of the `is_incremental()` fork. Consumes the
//!   cute-dbt#145 ingestion (`config.materialized` +
//!   `overrides.macros.is_incremental` are already on the domain types).
//!   Override semantics verified against dbt-fusion
//!   `9977b6cbb1b761065536300037560d8e3c037011` (`bind_override_macros`
//!   in `dbt-tasks-sa/src/renderable/renderable/unit_test.rs` stubs the
//!   overridden macro; without the stub the unit test compiles the
//!   full-build branch — dbt's documented default). The microbatch
//!   exclusion's wire shape (`incremental_strategy = "microbatch"`,
//!   `DbtIncrementalStrategy::Microbatch`, serde `snake_case`) is pinned
//!   to `dbt-schemas/src/schemas/common.rs` at the same SHA.
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

use crate::domain::cte::{
    CteGraph, EdgeType, JoinKeyPair, LeftJoinFact, SubqueryFact, SubqueryKind,
};
use crate::domain::governance::{ConstraintSupport, backing_test_for, constraint_support};
use crate::domain::grain::test_is_enabled;
use crate::domain::manifest::{
    Constraint, ConstraintKind, Manifest, Node, NodeConfig, NodeId, TestMetadata, TestSeverity,
};
use crate::domain::state::{resolve_target_model, resolve_tested_model};
use crate::domain::unit_test::{UnitTest, UnitTestGiven};
use crate::domain::unit_test_table::{CellValue, FixtureTable, table_from_manifest_rows};

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

/// One attributed-but-degraded backing test riding beside a
/// [`Verdict::Covered`] finding's attribution (cute-dbt#259).
///
/// The test satisfies the check's predicate — it is real backing and it
/// attributes — but its own config weakens the guarantee below the
/// default error-severity full-table contract: `severity: warn` (a
/// failing run warns instead of failing), a `where` row filter (only a
/// row subset is checked), or a `limit` cap (failure reporting is
/// truncated). Deliberately **not** a fourth verdict: the three-valued
/// covered/uncovered/unknown vocabulary is the epic cute-dbt#168 trust
/// contract, and the cue rides in-row beside the attribution (the #262
/// copy principles: in-row honesty, enumerated causes, never a silent
/// downgrade, never a percentage).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DegradedBacking {
    /// The attributed test node id — always one of the covered
    /// verdict's `by` entries (the subset invariant is pinned in tests).
    pub by: String,
    /// The enumerated degradation causes, human-readable copy composed
    /// in the domain (Rust computes, JS only renders). Never empty on
    /// an emitted entry.
    pub causes: Vec<String>,
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
    /// Attributed tests whose backing is degraded (cute-dbt#259) —
    /// per-test enumerated causes, set only by detectors whose
    /// satisfying tests can carry weakening config (today:
    /// `grain.unique-key-unbacked`). Non-empty only beside a
    /// [`Verdict::Covered`], and every entry's `by` is one of the
    /// verdict's attributed ids. Omitted from JSON when empty so every
    /// pre-#259 payload (and the committed goldens whose backing is
    /// full-strength) stays byte-stable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<DegradedBacking>,
    /// An operator acknowledgement (cute-dbt#171), set by the
    /// display-layer policy stage (`apply_check_policy`) — never by a
    /// detector or by supersedes resolution. Omitted from JSON when
    /// `None` so pre-#171 payloads (and the committed goldens) stay
    /// byte-stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppressed: Option<Suppression>,
}

/// Where a suppression came from (cute-dbt#171) — a `[[checks.suppress]]`
/// config entry or an inline `-- cute-dbt: ignore(...)` SQL pragma.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionSource {
    /// A `[[checks.suppress]]` entry in the `--config` TOML.
    Config,
    /// An inline `-- cute-dbt: ignore(check-id, "reason")` pragma in the
    /// model's raw SQL.
    Pragma,
}

/// An operator acknowledgement attached to a finding (cute-dbt#171).
///
/// Display-layer ONLY: a suppressed finding was still evaluated and
/// still participated in supersedes resolution — the mark is applied by
/// `apply_check_policy` (the grown [`filter_for_display`] stage) strictly
/// after [`resolve_supersedes`]. The reason rides into the payload so the
/// report can render the acknowledgement without browser-local state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suppression {
    /// Config entry vs inline pragma.
    pub source: SuppressionSource,
    /// The acknowledgement reason — required (`Some`) for config
    /// entries, optional for pragmas. Omitted from JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
            degraded: Vec::new(),
            suppressed: None,
        }
    }

    /// Attach degraded-backing cues (cute-dbt#259) — detector-side only,
    /// beside a [`Verdict::Covered`] attribution.
    #[must_use]
    pub fn with_degraded(mut self, degraded: Vec<DegradedBacking>) -> Self {
        self.degraded = degraded;
        self
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
                "a covering test whose own config weakens the guarantee — severity: warn, a where row filter, or a limit cap — still attributes, marked as DEGRADED backing with every cause enumerated on the finding (in-row honesty: a cue beside the attribution, never a fourth verdict, never a percentage)",
                "when no enabled generic uniqueness test covers the key but an enabled singular (SQL-file) test references the model through depends_on, the verdict degrades to UNKNOWN — a singular test may assert the declared grain, but its SQL is not statically classifiable (never a false Uncovered nag on singular-test shops)",
                "a uniqueness test on the declared grain that exists but is disabled — config.enabled: false on a nodes-map test, or a generic-test entry in the manifest disabled map — never counts as coverage and surfaces as `exists but disabled` evidence, distinct from absent",
            ],
            exclusions: [
                "a unique_key value that is not a literal column name / list of column names is reported UNKNOWN, never UNCOVERED (the declared grain is not statically recoverable)",
                "a uniqueness test whose column set is WIDER than the key does not satisfy the check (uniqueness of a superset does not imply uniqueness at the declared grain)",
                "a disabled SINGULAR test (and every non-generic-test disabled-map entry) carries no statically recoverable model linkage — both engines empty depends_on and omit attached_node on disabled nodes — so it is never attributed and never surfaced here",
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
        /// `join.left-null-propagation` — LEFT JOIN right-side columns
        /// reach the output and no given exercises the no-match path
        /// (cute-dbt#173, catalog class C4).
        JoinLeftNullPropagation {
            id: "join.left-null-propagation",
            name: "LEFT JOIN null propagation untested",
            group: "join",
            tier: High,
            instrument: Both,
            supersedes: [],
            evidence: [
                "cte-graph.left-join-facts",
                "cte-graph.body-leaf-table-refs",
                "manifest.unit-test-givens",
            ],
            conditions: [
                "the model LEFT-JOINs a relation and right-side columns provably reach the containing SELECT's projection: a direct `<right>.<column>` item, a `<right>.*` qualified wildcard, or a bare `*`",
                "satisfaction: some unit test's literal givens carry a left-side row whose ON equi-key has no match among the right-side given rows — cells compare on the value-normalized equality key, and a left row whose key cell is NULL or absent never matches (SQL join semantics), so it exercises the no-match path",
                "given binding follows the union.arm-coverage premise: only `given` entries carry statically-visible rows — an ungiven non-seed input is an empty mock; join sides bind to givens directly by external leaf name or through an upstream closure of simple-FROM CTEs reading exactly one external relation",
                "instrument routing (catalog C4/C10): when the SELECT containing the LEFT JOIN dedups its output with DISTINCT — the dedup-after-fan-out signal — the data-test recommendation wins: prove the right key's grain with a uniqueness data test at the source instead of adding fixtures",
                "verdict order: any test exercising an unmatched left row makes the construct COVERED with attribution; otherwise any statically-unattributable binding or given makes it UNKNOWN; otherwise UNCOVERED",
            ],
            exclusions: [
                "right-side columns reaching the output only through expressions (COALESCE, CASE, function calls) are not attributed — the construct stays silent unless a direct right-qualified item or wildcard projects (conservative: never a false fire)",
                "non-equi or non-column ON predicates, USING / NATURAL constraints, and unqualified key columns leave the join key statically unrecoverable — verdict UNKNOWN, never UNCOVERED",
                "a derived-table join side emits no fact; a CTE side whose upstream closure is not a single-external chain of simple-FROM CTEs is UNKNOWN, never UNCOVERED",
                "external `fixture:` files and non-literal `format: sql` givens make a test statically uncountable — UNKNOWN, never UNCOVERED",
                "an ungiven seed-side input reads real seed data — UNKNOWN, never UNCOVERED",
            ],
            recommendation: "Add a no-match given: one left-side row whose join key is absent from the right-side given rows, then extend `expect` with that row carrying NULL right-side columns (or the intended fallback). This finding's evidence carries a copy-pasteable given sketch.",
            rationale: "A LEFT JOIN whose right-side columns reach the output propagates NULLs on every unmatched left row — the most common real dbt unit-test catch. When every left given row has a right match, the no-match path runs on zero rows in every test, so an unhandled NULL (or a wrong fallback) ships silently.",
            detector: detect_join_left_null_propagation,
        },
        /// `join.anti-join` — LEFT JOIN + `WHERE <right key> IS NULL`;
        /// the more specific shape SUPERSEDES left-null-propagation and
        /// inverts the recommendation (cute-dbt#173, catalog C4
        /// refinement: rules must recognize the pattern, not force
        /// suppression). Since cute-dbt#196 the correlated NOT EXISTS
        /// and single-column NOT IN forms detect too, fed by the
        /// sibling cte-graph.subquery-facts evidence family.
        JoinAntiJoin {
            id: "join.anti-join",
            name: "Anti-join exclusion untested",
            group: "join",
            tier: High,
            instrument: UnitTest,
            supersedes: [JoinLeftNullPropagation],
            evidence: [
                "cte-graph.left-join-facts",
                "cte-graph.subquery-facts",
                "cte-graph.body-leaf-table-refs",
                "manifest.unit-test-givens",
            ],
            conditions: [
                "the model LEFT-JOINs a relation and filters `WHERE <right>.<key> IS NULL` in a top-level AND conjunct, where `<key>` is one of the join's ON equi-key right columns — the anti-join idiom: the join deliberately keeps the UNMATCHED left rows",
                "OR the model filters `WHERE NOT EXISTS (SELECT … FROM <inner> WHERE …)` in a top-level AND conjunct, the inner being a single plain named relation whose WHERE carries a correlated reference to the outer query — the resolvable outer↔inner equi-conjuncts are the anti-join keys (cute-dbt#196)",
                "OR the model filters `WHERE <col> NOT IN (SELECT <col> FROM <inner>)` in a top-level AND conjunct, the inner being a single plain named relation projecting exactly one column — the membership pair (outer column ↔ inner projected column) is the anti-join key (cute-dbt#196). SQL honesty note: a NULL in the inner column makes NOT IN yield NO rows at all; detection still treats the construct as the anti-join idiom (that is how it is authored), and the matched-row fixture this check recommends is exactly what surfaces the NULL trap",
                "the recommendation INVERTS join.left-null-propagation's: the anti-join's risk is the matched class leaking through, so the missing fixture is a left row that DOES match a right row, with `expect` proving it is excluded",
                "satisfaction (all arms): some unit test's literal givens carry a left/outer row whose key matches an inner/right given row (both cells non-NULL, equal on the value-normalized key)",
                "supersedes join.left-null-propagation on the same construct: NULL right-side columns are the anti-join's working mechanism, not an untested gap (the subquery constructs are never enumerated by left-null-propagation at all)",
                "given binding follows the union.arm-coverage premise: only `given` entries carry statically-visible rows — an ungiven non-seed input is an empty mock; join sides bind directly by external leaf name or through a single-external simple-FROM closure",
            ],
            exclusions: [
                "negated subqueries anywhere but a top-level AND conjunct of a SELECT's WHERE (OR branches, HAVING, JOIN ON, projections) are NOT detected — different semantics or position, silent, never misclassified",
                "non-negated EXISTS / IN subqueries (semi-join and membership inclusion — future evidence consumers) and scalar subqueries are NOT detected",
                "a NOT EXISTS / NOT IN whose inner is a derived table, joins or reads several relations, or carries its own WITH clause is NOT detected; an uncorrelated NOT EXISTS (zero outer references) is not a keyed anti-join — silent",
                "expression (non-column) correlations and projections are not key material: a correlated NOT EXISTS with no resolvable equi pair, and a NOT IN whose outer column does not resolve to a single relation, degrade to UNKNOWN, never UNCOVERED",
                "an IS NULL on a non-key right column is a data filter, not the anti-join idiom — join.left-null-propagation governs that construct",
                "an IS NULL inside an OR branch has different semantics and is never treated as the anti-join filter",
                "unrecoverable join keys, unresolvable side bindings, external `fixture:` files, non-literal `format: sql` givens, and ungiven seed inputs degrade to UNKNOWN, never UNCOVERED",
            ],
            recommendation: "Add a matching given pair: one left row whose join key IS present in the right-side given rows, then assert in `expect` that the matched row is excluded from the output. This finding's evidence carries a copy-pasteable given sketch.",
            rationale: "An anti-join's output is defined by what it excludes. Every existing given that only carries unmatched rows proves the keep path, never the exclusion: if the ON key drifts or the IS NULL column changes, matched rows leak into the output and no test catches it.",
            detector: detect_join_anti_join,
        },
        /// `incremental.branch-coverage` — the `is_incremental()`
        /// true/false branch rollup on incremental models (cute-dbt#164,
        /// coverage-intelligence rule #1).
        IncrementalBranchCoverage {
            id: "incremental.branch-coverage",
            name: "Unexercised is_incremental() branch",
            group: "incremental",
            tier: High,
            instrument: UnitTest,
            supersedes: [],
            evidence: [
                "manifest.config.materialized",
                "manifest.unit-test-overrides",
            ],
            conditions: [
                "the model is materialized incremental (config.materialized = \"incremental\") — its body forks on is_incremental(), and a dbt unit test compiles exactly one side of that fork",
                "a unit test with overrides.macros.is_incremental = true exercises the incremental branch; an explicit false override OR no override at all exercises the initial full-build branch (dbt compiles is_incremental() as false in unit tests by default)",
                "branch coverage rolls up per model as none / false-only / true-only / both; only BOTH satisfies the construct, attributing every unit test on the model (each test compiles one side of the fork)",
                "the HIGH-tier cue boundary (the union.arm-coverage precedent): a test on a branch proves that branch's compiled SQL runs under fixtures — whether the branch's filter/merge semantics are meaningfully asserted, or whether the body's Jinja even calls is_incremental(), is not statically decidable from the manifest, so the recommendation is a cue, never an assertion of a bug",
            ],
            exclusions: [
                "models whose materialization is absent, non-string, or anything other than incremental (view, table, ephemeral, custom) emit no finding — when the fork provably cannot exist or cannot be statically known, the check is silent, never misclassifies",
                "microbatch-strategy models (incremental_strategy = \"microbatch\", or any non-null event_time config) are OUT of rule #1: dbt replays them through event-time batch windows, not the is_incremental() fork, so true/false override coverage does not describe their semantics — never classified",
                "a non-boolean is_incremental override collapses to the no-override default at ingestion (cute-dbt#145 tolerant truthiness) and counts toward the full-build branch — the conservative side of the cue",
            ],
            recommendation: "Add a unit test for each missing is_incremental() branch: one with `overrides: macros: is_incremental: true` (mock the prior model state with a `given: - input: this` entry) to exercise the incremental branch, and one without the override for the initial full build (dbt compiles is_incremental() as false by default). This finding's evidence carries a copy-pasteable unit-test sketch per missing branch.",
            rationale: "An incremental model is two programs in one body: the initial full build, and the incremental run that filters on the high-water mark and merges by key. Each unit test compiles only one of them, so a suite living entirely on one branch ships the other untested — exactly where incremental models silently drop, duplicate, or re-process rows.",
            detector: detect_incremental_branch_coverage,
        },
        /// `enforcement.constraint-unbacked` — a DECLARED primary-key /
        /// unique constraint that the warehouse does not enforce
        /// (metadata-only on the adapter) AND has no backing data test
        /// (cute-dbt#260 Slice 3, governance-gated). The enforcement-reality
        /// surface: a parallel of `grain.unique-key-unbacked`, keyed on
        /// declared `constraints` rather than `config.unique_key`.
        EnforcementConstraintUnbacked {
            id: "enforcement.constraint-unbacked",
            name: "Declared constraint without a backing test",
            group: "enforcement",
            tier: Total,
            instrument: DataTest,
            supersedes: [],
            evidence: [
                "manifest.constraints",
                "manifest.metadata.adapter-type",
                "manifest.test-nodes",
            ],
            conditions: [
                "the model declares a primary_key or unique constraint (model-level constraints[] or a column-level constraint) whose enforcement on the manifest's adapter is metadata-only (NotEnforced) — the warehouse accepts the declaration but does not enforce uniqueness at write time",
                "no enabled generic uniqueness data test (unique, attached to the model) backs the constrained column — the constraint→test edge is INFERRED by column + test-name match, since the manifest never links a constraint to its test",
                "the verdict is UNCOVERED only when the constraint is declared, metadata-only, AND has no inferred backing test; a backing test (even one cute-dbt could miss) keeps it silent",
            ],
            exclusions: [
                "a constraint the adapter ENFORCES at write time (e.g. not_null / foreign_key on Postgres/DuckDB) is never a gap — the warehouse guarantees it",
                "a constraint kind with no column-level generic-test backing (check / custom / foreign_key) is out of this inference and never reported here",
                "the inferred edge can MISS a renamed test or a singular/custom test asserting the same uniqueness — the copy says \"backing test\" (an authoring-discipline cue), never that the warehouse lacks an index; columns are authored-YAML-only, so this is never a warehouse-truth claim",
                "the whole `enforcement` group is gated behind the governance experiment — off by default, it never fires on a non-governance report",
            ],
            recommendation: "Add a uniqueness data test on the declared-but-unenforced constraint column (`unique` for a single column), so the grain the contract DECLARES is actually verified by a test on every run. The warehouse will not enforce it for you on this adapter.",
            rationale: "A primary-key / unique constraint that the warehouse treats as metadata-only is a DECLARED guarantee with nothing checking it: duplicate rows load silently, and any downstream join or incremental merge that trusts the declared grain corrupts. A backing data test is the only thing that actually verifies the declared uniqueness on this adapter.",
            detector: detect_enforcement_constraint_unbacked,
        },
    }
}

// ---------------------------------------------------------------------
// grain.unique-key-unbacked — the walking-skeleton detector.
// ---------------------------------------------------------------------

/// The construct discriminator for the unique-key grain check.
const UNIQUE_KEY_CONSTRUCT: &str = "config.unique_key";

/// Detector for `grain.unique-key-unbacked` (cute-dbt#169; truthfulness
/// hardening cute-dbt#259).
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
/// their node ids (sorted), with per-test [`DegradedBacking`] cues when
/// a covering test is warn-severity / where-filtered / limit-capped
/// (cute-dbt#259 — attribution stays, the weakening is enumerated
/// in-row); none, but ≥1 enabled singular test referencing the model
/// via `depends_on` ⇒ [`Verdict::Unknown`] (a singular test may assert
/// the grain; its SQL is not statically classifiable — never a false
/// Uncovered nag); none at all ⇒ [`Verdict::Uncovered`]; a declared
/// key whose columns are not statically recoverable ⇒
/// [`Verdict::Unknown`]. No `unique_key` ⇒ no finding (trigger silent).
///
/// A declared-but-disabled uniqueness test on the grain (a nodes-map
/// test with `config.enabled: false`, or a generic-test entry in the
/// manifest `disabled` map) never counts as coverage and surfaces as
/// `exists but disabled` evidence — distinct from absent (cute-dbt#259).
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
    let mut evidence = vec![Evidence::new("unique_key", columns.join(", "))];
    let key_set: BTreeSet<String> = columns
        .iter()
        .map(|column| column.to_ascii_lowercase())
        .collect();
    // Borrowed scan: ids stay `&str` through the sort and only become
    // owned at the POD ownership boundary (`by` / `DegradedBacking.by`).
    let mut covering: Vec<(&str, &Node)> = ctx
        .manifest
        .nodes()
        .iter()
        .filter(|(_, node)| covers_grain(node, ctx.model.id(), &key_set))
        .map(|(id, node)| (id.as_str(), node))
        .collect();
    covering.sort_by_key(|(id, _)| *id);
    // cute-dbt#259 — per-test degradation cues beside the attribution:
    // a warn-severity / where-filtered / limit-capped test still backs
    // the grain, but never silently as full-strength coverage.
    let degraded: Vec<DegradedBacking> = covering
        .iter()
        .filter_map(|(id, node)| {
            let causes = backing_degradations(node.config());
            (!causes.is_empty()).then(|| DegradedBacking {
                by: (*id).to_owned(),
                causes,
            })
        })
        .collect();
    let by: Vec<String> = covering.into_iter().map(|(id, _)| id.to_owned()).collect();
    evidence.extend(disabled_grain_evidence(
        ctx.manifest,
        ctx.model.id(),
        &key_set,
    ));
    let verdict = if by.is_empty() {
        grain_fallback_verdict(ctx.manifest, ctx.model.id(), &mut evidence)
    } else {
        Verdict::Covered { by }
    };
    vec![
        Finding::new(
            check,
            ctx.model.id().clone(),
            UNIQUE_KEY_CONSTRUCT,
            verdict,
            evidence,
        )
        .with_degraded(degraded),
    ]
}

/// The construct id for an enforcement-reality finding (cute-dbt#260
/// Slice 3) — the declared constraint column it annotates.
fn enforcement_construct(column: &str, kind: ConstraintKind) -> String {
    let kind_label = match kind {
        ConstraintKind::PrimaryKey => "primary_key",
        ConstraintKind::Unique => "unique",
        _ => "constraint",
    };
    format!("constraint.{kind_label}[{column}]")
}

/// `enforcement.constraint-unbacked` (cute-dbt#260 Slice 3) — a DECLARED
/// PK/unique constraint that the adapter does not enforce (metadata-only)
/// and no inferred backing test covers. Governance-gated: the
/// `enforcement` group is filtered out of a non-governance report's
/// policy, so this never fires unless the experiment is on.
fn detect_enforcement_constraint_unbacked(ctx: &CheckContext<'_>) -> Vec<Finding<HeuristicId>> {
    if ctx.model.resource_type() != "model" {
        return Vec::new();
    }
    let Some(adapter) = ctx.manifest.metadata().adapter_type() else {
        // No adapter type ⇒ the enforcement matrix can't be applied. Stay
        // silent (never a false claim about a warehouse we can't name).
        return Vec::new();
    };
    declared_unique_constraints(ctx.model)
        .into_iter()
        .filter_map(|(column, kind)| enforcement_finding(ctx, adapter, &column, kind))
        .collect()
}

/// The declared primary-key / unique constraint columns on a model
/// (cute-dbt#260 Slice 3) — model-level `constraints[]` of PK/unique kind
/// (one entry per constrained column) plus column-level PK/unique
/// constraints. Deduplicated by `(column, kind)`, deterministic order.
fn declared_unique_constraints(model: &Node) -> Vec<(String, ConstraintKind)> {
    let mut seen: BTreeSet<(String, &'static str)> = BTreeSet::new();
    model_level_unique_columns(model)
        .into_iter()
        .chain(column_level_unique_columns(model))
        .filter(|(column, kind)| seen.insert((column.clone(), kind_tag(*kind))))
        .collect()
}

/// Model-level `constraints[]` of PK/unique kind, flattened to
/// `(column, kind)` (one per constrained column).
fn model_level_unique_columns(model: &Node) -> Vec<(String, ConstraintKind)> {
    model
        .constraints()
        .iter()
        .filter_map(|c| unique_constraint_kind(c).map(|kind| (c, kind)))
        .flat_map(|(c, kind)| c.columns().iter().map(move |col| (col.clone(), kind)))
        .collect()
}

/// Column-level PK/unique constraints, as `(column, kind)`.
fn column_level_unique_columns(model: &Node) -> Vec<(String, ConstraintKind)> {
    model
        .column_facts()
        .iter()
        .flat_map(|(column, facts)| {
            facts
                .constraints()
                .iter()
                .filter_map(move |c| unique_constraint_kind(c).map(|kind| (column.clone(), kind)))
        })
        .collect()
}

/// The PK/unique kind of a constraint, or `None` for any other kind
/// (only PK + unique are in the column-uniqueness inference).
fn unique_constraint_kind(constraint: &Constraint) -> Option<ConstraintKind> {
    match constraint.kind() {
        kind @ (ConstraintKind::PrimaryKey | ConstraintKind::Unique) => Some(kind),
        _ => None,
    }
}

/// A stable string tag for a PK/unique kind (the dedup key half).
fn kind_tag(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::PrimaryKey => "primary_key",
        _ => "unique",
    }
}

/// Emit the enforcement finding for one declared constraint column, or
/// `None` when the warehouse enforces it (no gap) or a backing test
/// covers it (the inference found one). The copy says "declared" +
/// "backing test" — authoring discipline, never a warehouse-truth claim.
fn enforcement_finding(
    ctx: &CheckContext<'_>,
    adapter: &str,
    column: &str,
    kind: ConstraintKind,
) -> Option<Finding<HeuristicId>> {
    // Only metadata-only (NotEnforced) declarations are a gap — an
    // enforced one is guaranteed by the warehouse; a NotSupported one was
    // never accepted.
    if constraint_support(adapter, kind) != ConstraintSupport::NotEnforced {
        return None;
    }
    let backed = backing_test_for(ctx.manifest, ctx.model.id(), column, kind);
    let kind_label = kind_tag(kind);
    let evidence = vec![
        Evidence::new("constraint", format!("{kind_label} on {column}")),
        Evidence::new("adapter", format!("{adapter} (metadata-only)")),
        Evidence::new("backing-test", if backed { "present" } else { "missing" }),
    ];
    let verdict = if backed {
        Verdict::Covered {
            by: vec![format!("{kind_label}[{column}]")],
        }
    } else {
        Verdict::Uncovered
    };
    Some(Finding::new(
        HeuristicId::EnforcementConstraintUnbacked,
        ctx.model.id().clone(),
        enforcement_construct(column, kind),
        verdict,
        evidence,
    ))
}

/// The no-generic-backing arm of the grain verdict (cute-dbt#259):
/// UNCOVERED only when nothing else could plausibly assert the grain.
/// An enabled singular (SQL-file) test referencing the model via
/// `depends_on` — the only wire linkage singular tests carry
/// (cute-dbt#258, live-probed on both engines) — degrades the verdict
/// to honest UNKNOWN with the singular tests enumerated in evidence,
/// stating what WAS checked (the #262 copy principles).
fn grain_fallback_verdict(
    manifest: &Manifest,
    model_id: &NodeId,
    evidence: &mut Vec<Evidence>,
) -> Verdict {
    let singular = singular_tests_on(manifest, model_id);
    if singular.is_empty() {
        return Verdict::Uncovered;
    }
    evidence.push(Evidence::new(
        "generic backing",
        "no enabled generic uniqueness data test (unique / unique_combination_of_columns) covers a column subset of the declared key",
    ));
    for id in &singular {
        evidence.push(Evidence::new(
            "singular test",
            format!(
                "{id} — an enabled singular (SQL-file) test references this model via depends_on; whether its SQL asserts the declared grain is not statically decidable",
            ),
        ));
    }
    Verdict::Unknown
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
    covers_key(&columns, key_set)
}

/// `true` when a uniqueness test's `columns` ⊆ `key_set` (already
/// lowercased) — non-empty subset direction only (cute-dbt#169).
fn covers_key(columns: &[String], key_set: &BTreeSet<String>) -> bool {
    !columns.is_empty()
        && columns
            .iter()
            .all(|column| key_set.contains(&column.to_ascii_lowercase()))
}

/// The enumerated degradation causes a covering data test's own config
/// carries (cute-dbt#259) — empty for the default full-strength
/// contract (error severity, unfiltered, uncapped). Copy is composed
/// here so the wording lives in one testable place (Rust computes, JS
/// only renders).
fn backing_degradations(config: &NodeConfig) -> Vec<String> {
    let mut causes = Vec::new();
    match config.severity() {
        Some(TestSeverity::Warn) => causes.push(
            "severity: warn — a failing run warns instead of failing, so this backing is advisory, not gating"
                .to_owned(),
        ),
        Some(TestSeverity::Unrecognized) => {
            let raw = config
                .config()
                .get("severity")
                .map_or_else(String::new, Value::to_string);
            causes.push(format!(
                "severity: {raw} — not a recognized severity, so the gating behavior is unknown",
            ));
        }
        Some(TestSeverity::Error) | None => {}
    }
    if let Some(filter) = config
        .where_filter()
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
    {
        causes.push(format!(
            "where-filtered — only rows matching `{filter}` are checked; the grain is unverified outside the filter",
        ));
    }
    if let Some(limit) = config.limit() {
        causes.push(format!(
            "limit: {limit} — failing-row reporting caps at {limit} rows; the test still trips on the first failure, but the failure surface is truncated",
        ));
    }
    causes
}

/// `exists but disabled` evidence rows (cute-dbt#259): a disabled
/// uniqueness test on the declared grain asserts nothing and never
/// counts as coverage, but its existence is a different fact than NO
/// test — the author wrote one and switched it off. Scans BOTH
/// disabled surfaces: nodes-map test nodes carrying `config.enabled:
/// false` (synthetic manifests keep disabled tests inline) and the
/// manifest `disabled` map (where both real engines put them —
/// cute-dbt#258). Deterministic: sorted by id, deduplicated.
fn disabled_grain_evidence(
    manifest: &Manifest,
    model_id: &NodeId,
    key_set: &BTreeSet<String>,
) -> Vec<Evidence> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for (id, node) in manifest.nodes() {
        if node.resource_type() == "test"
            && node.attached_node() == Some(model_id)
            && !test_is_enabled(node)
            && let Some(columns) = uniqueness_test_columns(node)
            && covers_key(&columns, key_set)
        {
            rows.push((id.as_str().to_owned(), columns.join(", ")));
        }
    }
    for (id, entries) in manifest.disabled() {
        for entry in entries {
            if entry.resource_type() == "test"
                && entry.attached_node() == Some(model_id)
                && let Some(test_metadata) = entry.test_metadata()
                && let Some(columns) = uniqueness_columns(test_metadata, entry.column_name())
                && covers_key(&columns, key_set)
            {
                rows.push((id.clone(), columns.join(", ")));
            }
        }
    }
    rows.sort();
    rows.dedup();
    rows.into_iter()
        .map(|(id, columns)| {
            Evidence::new(
                "exists but disabled",
                format!(
                    "{id} — a uniqueness test on ({columns}) is declared but disabled (config.enabled: false); it asserts nothing and never counts as coverage",
                ),
            )
        })
        .collect()
}

/// The enabled singular (SQL-file) tests referencing `model_id`
/// through `depends_on.nodes`, sorted by id (cute-dbt#259). Singular
/// tests carry no `attached_node` / `test_metadata` on either engine
/// (cute-dbt#258) — `depends_on` is the only statically recoverable
/// linkage. Borrowed from the manifest: the sole consumer
/// ([`grain_fallback_verdict`]) only formats the ids into evidence
/// copy, so owning them here would allocate just to discard.
fn singular_tests_on<'a>(manifest: &'a Manifest, model_id: &NodeId) -> Vec<&'a str> {
    let mut ids: Vec<&str> = manifest
        .nodes()
        .iter()
        .filter(|(_, node)| {
            node.is_singular_test()
                && test_is_enabled(node)
                && node.depends_on().nodes().contains(model_id)
        })
        .map(|(id, _)| id.as_str())
        .collect();
    ids.sort_unstable();
    ids
}

/// The column set a uniqueness test asserts, or `None` when `node` is
/// not a uniqueness test ([`uniqueness_columns`] over the node's
/// linkage fields).
fn uniqueness_test_columns(node: &Node) -> Option<Vec<String>> {
    uniqueness_columns(node.test_metadata()?, node.column_name())
}

/// The column set a uniqueness test's `test_metadata` asserts, or
/// `None` when it is not a uniqueness test. Shared by the nodes-map
/// recognizer ([`uniqueness_test_columns`]) and the disabled-map
/// recognizer (cute-dbt#259 —
/// [`crate::domain::manifest::DisabledEntry`] keeps the same
/// `test_metadata` / `column_name` linkage pair on disabled GENERIC
/// tests).
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
fn uniqueness_columns(
    test_metadata: &TestMetadata,
    node_column_name: Option<&str>,
) -> Option<Vec<String>> {
    match test_metadata.name() {
        "unique" => {
            let column = test_metadata
                .kwargs()
                .get("column_name")
                .and_then(Value::as_str)
                .or(node_column_name)?;
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
/// string-literal argument** of a `ref(...)` / `source(...)` call —
/// `ref('stg_orders')` → `stg_orders`, `ref('pkg', 'stg_orders')` →
/// `stg_orders`, `source('raw', 'orders')` → `orders`. Either quote
/// style is accepted under the matching-quote rule (dbt given inputs
/// are Python/Jinja string literals — dbt accepts both quote
/// characters and both engines ship the authored string verbatim on
/// the manifest wire, cute-dbt#245): the open and close quote must be
/// the **same** character; a mixed pair or an unbalanced quote fails
/// open to `None` — the domain twin of the renderer's
/// `strip_matching_quotes` contract (cute-dbt#249). Mirrors the
/// renderer's given↔leaf-ref binding (cute-dbt#34/#131:
/// `render::parse_ref_name` + the source unwrap; duplicated here because
/// domain never imports adapters). `None` for `this` (incremental prior
/// state feeds the model itself, never an arm) and for any other shape.
fn given_input_leaf(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let keyword = trimmed[..trimmed.find('(')?].trim_end();
    let is_call = keyword.eq_ignore_ascii_case("ref") || keyword.eq_ignore_ascii_case("source");
    if !is_call || !trimmed.ends_with(')') {
        return None;
    }
    // Right-to-left scan: the LAST quote character of either style
    // closes the last string-literal argument (tolerating trailing
    // non-string kwargs like `ref('orders', v=2)`); its opener must be
    // the SAME character — mixed/unbalanced pairs fall through to None.
    let close = trimmed.rfind(['\'', '"'])?;
    let quote = char::from(trimmed.as_bytes()[close]);
    let open = trimmed[..close].rfind(quote)?;
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
            resolve_tested_model(ctx.manifest, ut).is_some_and(|model| model.id() == ctx.model.id())
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
        match classify_union_arm(ctx, tests, &arm_name, refs) {
            UnionArm::Covered(ids) => covered_by.extend(ids),
            UnionArm::Unknown(evidence) => unknown_evidence.push(evidence),
            UnionArm::Unfed(name, refs) => unfed.push((name, refs)),
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

/// How one UNION arm classifies in [`union_finding`].
enum UnionArm {
    /// At least one test feeds the arm — the ids that cover it.
    Covered(Vec<String>),
    /// The arm is unattributable: no resolvable upstream relation, or fed
    /// only by givens whose rows are not statically countable. Carries the
    /// evidence line.
    Unknown(Evidence),
    /// No given row reaches the arm — its name + the external refs it reads.
    Unfed(String, BTreeSet<String>),
}

/// Classify one UNION arm against the model's unit tests: an arm with no
/// resolvable upstream relation is [`UnionArm::Unknown`]; otherwise it is
/// [`UnionArm::Covered`] when any test feeds it, [`UnionArm::Unknown`]
/// when only not-statically-countable givens reach it, else
/// [`UnionArm::Unfed`].
fn classify_union_arm(
    ctx: &CheckContext<'_>,
    tests: &[(&String, &UnitTest)],
    arm_name: &str,
    refs: BTreeSet<String>,
) -> UnionArm {
    if refs.is_empty() {
        return UnionArm::Unknown(Evidence::new(
            "unattributable arm",
            format!("{arm_name} — no resolvable upstream relation"),
        ));
    }
    let mut fed_by: Vec<String> = Vec::new();
    let mut unknown_in_a_test = false;
    for (id, unit_test) in tests {
        match arm_coverage_for_test(ctx.manifest, unit_test, &refs) {
            ArmCoverage::Fed => fed_by.push((*id).clone()),
            ArmCoverage::Unknown => unknown_in_a_test = true,
            ArmCoverage::Unfed => {}
        }
    }
    if !fed_by.is_empty() {
        UnionArm::Covered(fed_by)
    } else if unknown_in_a_test {
        UnionArm::Unknown(Evidence::new(
            "unattributable arm",
            format!(
                "{arm_name} — fed only by givens whose rows are not statically countable (reads {})",
                refs_display(&refs),
            ),
        ))
    } else {
        UnionArm::Unfed(arm_name.to_owned(), refs)
    }
}

// ---------------------------------------------------------------------
// join.left-null-propagation + join.anti-join — the supersedes pair
// (cute-dbt#173, catalog class C4 + the agenda §4b anti-join refinement).
// ---------------------------------------------------------------------

/// One LEFT JOIN site: the **shared** construct discriminator both join
/// checks emit findings on. Identical by construction (both detectors
/// consume this one enumeration), so `(model_id, construct)` is the
/// supersedes-resolution join key.
struct LeftJoinSite<'a> {
    construct: String,
    fact: &'a LeftJoinFact,
}

/// Enumerate a graph's LEFT JOIN sites in source order, deduplicating
/// repeated `(consumer, right_leaf)` pairs with a `#<ordinal>` suffix.
fn left_join_sites(graph: &CteGraph) -> Vec<LeftJoinSite<'_>> {
    let mut seen: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    graph
        .left_join_facts()
        .iter()
        .map(|fact| {
            let ordinal = seen
                .entry((fact.consumer(), fact.right_leaf()))
                .and_modify(|n| *n += 1)
                .or_insert(1);
            let construct = if *ordinal == 1 {
                format!("left_join[{}:{}]", fact.consumer(), fact.right_leaf())
            } else {
                format!(
                    "left_join[{}:{}#{}]",
                    fact.consumer(),
                    fact.right_leaf(),
                    ordinal
                )
            };
            LeftJoinSite { construct, fact }
        })
        .collect()
}

/// The ON equi-key right columns that also appear under a top-level
/// `WHERE … IS NULL` conjunct — non-empty exactly when the fact is the
/// anti-join idiom (`LEFT JOIN … WHERE <right key> IS NULL`).
fn anti_join_null_keys(fact: &LeftJoinFact) -> Vec<&str> {
    fact.equi_keys()
        .iter()
        .map(crate::domain::cte::JoinKeyPair::right_column)
        .filter(|column| {
            fact.where_is_null_columns()
                .iter()
                .any(|null_column| null_column == column)
        })
        .collect()
}

/// Resolve a join side's leaf to the external relation a unit-test
/// given mocks: the leaf itself when it is not a CTE; otherwise the
/// single external relation read by the CTE's upstream closure,
/// required to be a chain of simple-FROM CTEs (the dominant
/// `select … from <one relation>` import/refine shape — the column
/// names the ON clause keys on conventionally survive such a chain).
/// `None` is the honest UNKNOWN degrade.
fn resolve_side_external(graph: &CteGraph, leaf: &str) -> Option<String> {
    let lower = leaf.to_ascii_lowercase();
    let cte_names: BTreeSet<String> = graph
        .nodes()
        .iter()
        .map(|node| node.name().to_ascii_lowercase())
        .collect();
    let Some(start) = graph
        .nodes()
        .iter()
        .position(|node| node.name().eq_ignore_ascii_case(&lower))
    else {
        // Not a CTE — an external relation, directly mockable.
        return Some(lower);
    };
    let closure = upstream_closure(graph, start);
    single_external_in_closure(graph, &closure, &cte_names)
}

/// The set of node indices reachable upstream of `start` (inclusive) by
/// walking edges backward (`edge.to() == node ⇒ visit edge.from()`).
fn upstream_closure(graph: &CteGraph, start: usize) -> BTreeSet<usize> {
    let mut closure = BTreeSet::from([start]);
    let mut frontier = vec![start];
    while let Some(node) = frontier.pop() {
        for edge in graph.edges() {
            if edge.to() == node && closure.insert(edge.from()) {
                frontier.push(edge.from());
            }
        }
    }
    closure
}

/// The single external (non-CTE) relation read across every node in
/// `closure`, or `None` (the honest UNKNOWN degrade) when any node is not
/// a simple-FROM shape, or the closure reads zero or more-than-one
/// external relation.
fn single_external_in_closure(
    graph: &CteGraph,
    closure: &BTreeSet<usize>,
    cte_names: &BTreeSet<String>,
) -> Option<String> {
    let mut externals: BTreeSet<String> = BTreeSet::new();
    for &index in closure {
        let node = graph.nodes().get(index)?;
        if !node.is_simple_from_shape() {
            return None;
        }
        for leaf_ref in node.body_leaf_table_refs() {
            if !cte_names.contains(leaf_ref) {
                externals.insert(leaf_ref.clone());
            }
        }
    }
    if externals.len() == 1 {
        externals.into_iter().next()
    } else {
        None
    }
}

/// The shared key-verdict inputs of one join-shaped construct — an
/// internal view constructed from both [`LeftJoinFact`] and
/// [`SubqueryFact`] (cute-dbt#196). [`bind_keys`] /
/// [`key_match_verdict`] consume only (consumer, right-side leaf, equi
/// keys) plus the LEFT-JOIN-only DISTINCT dedup signal, so both fact
/// families inherit the same supersedes + covered/uncovered/UNKNOWN
/// machinery — a view, never a POD change.
struct KeyedJoinView<'a> {
    consumer: &'a str,
    right_leaf: &'a str,
    equi_keys: &'a [JoinKeyPair],
    /// The dedup-after-fan-out instrument-routing signal — carried by
    /// LEFT JOIN facts only; always `false` for subquery facts (a
    /// negated subquery never fans out).
    select_is_distinct: bool,
}

impl<'a> From<&'a LeftJoinFact> for KeyedJoinView<'a> {
    fn from(fact: &'a LeftJoinFact) -> Self {
        Self {
            consumer: fact.consumer(),
            right_leaf: fact.right_leaf(),
            equi_keys: fact.equi_keys(),
            select_is_distinct: fact.select_is_distinct(),
        }
    }
}

impl<'a> From<&'a SubqueryFact> for KeyedJoinView<'a> {
    fn from(fact: &'a SubqueryFact) -> Self {
        Self {
            consumer: fact.consumer(),
            right_leaf: fact.inner_leaf(),
            equi_keys: fact.equi_keys(),
            select_is_distinct: false,
        }
    }
}

/// A LEFT JOIN site's equi keys bound to the external relations its
/// unit-test givens mock — the statically-recoverable key-match inputs.
struct BoundKeys {
    left_external: String,
    right_external: String,
    /// `(left_column, right_column)` pairs, lowercased.
    pairs: Vec<(String, String)>,
}

/// Bind a fact's equi keys, or `None` when not statically recoverable:
/// no equi pairs, pairs spanning several left relations, or a side that
/// does not resolve to a single mockable external relation.
fn bind_keys(graph: &CteGraph, view: &KeyedJoinView<'_>) -> Option<BoundKeys> {
    let mut left_leaves: BTreeSet<&str> = BTreeSet::new();
    for pair in view.equi_keys {
        left_leaves.insert(pair.left_leaf()?);
    }
    if left_leaves.len() != 1 {
        return None;
    }
    let left_external = resolve_side_external(graph, left_leaves.iter().next()?)?;
    let right_external = resolve_side_external(graph, view.right_leaf)?;
    Some(BoundKeys {
        left_external,
        right_external,
        pairs: view
            .equi_keys
            .iter()
            .map(|pair| {
                (
                    pair.left_column().to_owned(),
                    pair.right_column().to_owned(),
                )
            })
            .collect(),
    })
}

/// One side's statically-known given rows within one unit test.
enum SideRows {
    /// A literal table — possibly empty. An ungiven non-seed input is
    /// the empty mock (the union.arm-coverage premise: only `given`
    /// entries carry statically-visible rows).
    Table(FixtureTable),
    /// Not statically countable: an external `fixture:` file, a
    /// non-literal `format: sql` given, or an ungiven seed input.
    Unknown,
}

/// Resolve the given mocking `external` within `unit_test` to its
/// literal table.
fn side_rows(manifest: &Manifest, unit_test: &UnitTest, external: &str) -> SideRows {
    for given in unit_test.given() {
        let Some(leaf) = given_input_leaf(given.input()) else {
            continue;
        };
        if leaf != external {
            continue;
        }
        if given.fixture().is_some() {
            return SideRows::Unknown;
        }
        return match table_from_manifest_rows(given.rows(), given.format()) {
            Some(table) => SideRows::Table(table),
            None => SideRows::Unknown,
        };
    }
    if leaf_resolves_to_seed(manifest, external) {
        SideRows::Unknown
    } else {
        SideRows::Table(FixtureTable::default())
    }
}

/// The key cells of every row in `table` for `columns`
/// (case-insensitive header lookup), or `None` when a **non-empty**
/// table lacks one of the key columns — a likely misbinding, degraded
/// honestly instead of judged.
fn key_rows(table: &FixtureTable, columns: &[String]) -> Option<Vec<Vec<CellValue>>> {
    if table.rows.is_empty() {
        return Some(Vec::new());
    }
    let indices: Vec<usize> = columns
        .iter()
        .map(|column| {
            table
                .columns
                .iter()
                .position(|header| header.eq_ignore_ascii_case(column))
        })
        .collect::<Option<Vec<usize>>>()?;
    Some(
        table
            .rows
            .iter()
            .map(|row| {
                indices
                    .iter()
                    .map(|&index| {
                        row.cells
                            .get(index)
                            .map_or(CellValue::Absent, |cell| cell.key.clone())
                    })
                    .collect()
            })
            .collect(),
    )
}

/// `true` when a left key tuple matches a right key tuple: every
/// component non-NULL/non-absent on **both** sides and equal on the
/// value-normalized key (SQL semantics: NULL never equals anything).
fn keys_match(left: &[CellValue], right: &[CellValue]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(l, r)| {
            !matches!(l, CellValue::Null | CellValue::Absent)
                && !matches!(r, CellValue::Null | CellValue::Absent)
                && l == r
        })
}

/// What one unit test statically proves about a bound LEFT JOIN's key
/// matching.
struct KeyFacts {
    /// ≥1 left given row has NO matching right row — the no-match path
    /// carries rows.
    has_unmatched_left: bool,
    /// ≥1 left given row HAS a matching right row — the matched path
    /// carries rows.
    has_matched_left: bool,
}

/// Per-test key facts, or `None` when the test is not statically
/// attributable for this join (the UNKNOWN degrade).
fn test_key_facts(
    manifest: &Manifest,
    unit_test: &UnitTest,
    bound: &BoundKeys,
) -> Option<KeyFacts> {
    let SideRows::Table(left) = side_rows(manifest, unit_test, &bound.left_external) else {
        return None;
    };
    let SideRows::Table(right) = side_rows(manifest, unit_test, &bound.right_external) else {
        return None;
    };
    let left_columns: Vec<String> = bound.pairs.iter().map(|(l, _)| l.clone()).collect();
    let right_columns: Vec<String> = bound.pairs.iter().map(|(_, r)| r.clone()).collect();
    let left_rows = key_rows(&left, &left_columns)?;
    let right_rows = key_rows(&right, &right_columns)?;
    let mut facts = KeyFacts {
        has_unmatched_left: false,
        has_matched_left: false,
    };
    for left_row in &left_rows {
        if right_rows
            .iter()
            .any(|right_row| keys_match(left_row, right_row))
        {
            facts.has_matched_left = true;
        } else {
            facts.has_unmatched_left = true;
        }
    }
    Some(facts)
}

/// The unit tests targeting `ctx.model`, sorted by id (deterministic
/// attribution).
fn tests_on_model<'a>(ctx: &'a CheckContext<'_>) -> Vec<(&'a String, &'a UnitTest)> {
    let mut tests: Vec<(&String, &UnitTest)> = ctx
        .manifest
        .unit_tests()
        .iter()
        .filter(|(_, unit_test)| {
            resolve_tested_model(ctx.manifest, unit_test)
                .is_some_and(|model| model.id() == ctx.model.id())
        })
        .collect();
    tests.sort_by(|a, b| a.0.cmp(b.0));
    tests
}

/// The always-present "left join" evidence naming the construct.
fn left_join_evidence(fact: &LeftJoinFact) -> Evidence {
    let on = if fact.equi_keys().is_empty() {
        "ON <not statically recoverable>".to_owned()
    } else {
        let pairs: Vec<String> = fact
            .equi_keys()
            .iter()
            .map(|pair| {
                format!(
                    "{}.{} = {}.{}",
                    pair.left_leaf().unwrap_or("?"),
                    pair.left_column(),
                    fact.right_leaf(),
                    pair.right_column(),
                )
            })
            .collect();
        format!("ON {}", pairs.join(" AND "))
    };
    Evidence::new(
        "left join",
        format!(
            "{} — LEFT JOIN {} {}",
            fact.consumer(),
            fact.right_leaf(),
            on
        ),
    )
}

/// The copy-pasteable no-match given sketch (catalog C4 worked
/// example): a left row whose key is absent from the right given.
fn no_match_given_sketch(bound: &BoundKeys) -> String {
    let left_row: Vec<String> = bound
        .pairs
        .iter()
        .map(|(left, _)| format!("{left}: 404"))
        .collect();
    let right_row: Vec<String> = bound
        .pairs
        .iter()
        .map(|(_, right)| format!("{right}: 1"))
        .collect();
    format!(
        "- input: ref('{left}')\n  rows:\n    - {{{left_cells}}}   # 404 has no match below — the no-match path\n- input: ref('{right}')\n  rows:\n    - {{{right_cells}}}\n# expect: the no-match row with NULL {right} columns (or the intended fallback)",
        left = bound.left_external,
        right = bound.right_external,
        left_cells = left_row.join(", "),
        right_cells = right_row.join(", "),
    )
}

/// The INVERTED anti-join sketch: a left row that DOES match, proving
/// the matched class is excluded.
fn matching_given_sketch(bound: &BoundKeys) -> String {
    let left_row: Vec<String> = bound
        .pairs
        .iter()
        .map(|(left, _)| format!("{left}: 1"))
        .collect();
    let right_row: Vec<String> = bound
        .pairs
        .iter()
        .map(|(_, right)| format!("{right}: 1"))
        .collect();
    format!(
        "- input: ref('{left}')\n  rows:\n    - {{{left_cells}}}   # matches the right row below\n- input: ref('{right}')\n  rows:\n    - {{{right_cells}}}\n# expect: rows WITHOUT the matched left row — the anti-join must exclude it",
        left = bound.left_external,
        right = bound.right_external,
        left_cells = left_row.join(", "),
        right_cells = right_row.join(", "),
    )
}

/// The routed data-test sketch for the dedup-after-fan-out case
/// (catalog C4/C10): prove the right key's grain at the source instead
/// of adding fixtures.
fn grain_data_test_sketch(bound: &BoundKeys) -> String {
    let header = format!(
        "# DISTINCT dedups this LEFT JOIN's output — test '{}' key grain at the source\n# instead of another fixture (catalog C4/C10 routing)\n",
        bound.right_external,
    );
    if bound.pairs.len() == 1 {
        format!(
            "{header}columns:\n  - name: {key}\n    data_tests:\n      - unique",
            key = bound.pairs[0].1,
        )
    } else {
        let columns: Vec<String> = bound.pairs.iter().map(|(_, right)| right.clone()).collect();
        format!(
            "{header}data_tests:\n  - dbt_utils.unique_combination_of_columns:\n      combination_of_columns: [{}]",
            columns.join(", "),
        )
    }
}

/// Which key-match direction satisfies a join check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyDirection {
    /// `join.left-null-propagation` — an UNMATCHED left row exercises
    /// the no-match path.
    NoMatch,
    /// `join.anti-join` — a MATCHED left row proves the exclusion.
    Match,
}

/// Shared verdict computation for the join-shaped checks (LEFT JOIN
/// sites and, since cute-dbt#196, negated-subquery sites): bind the
/// keys, score every unit test in `direction`, and aggregate per the
/// honest tier order (covered → unknown → uncovered).
fn key_match_verdict(
    ctx: &CheckContext<'_>,
    graph: &CteGraph,
    view: &KeyedJoinView<'_>,
    direction: KeyDirection,
    evidence: &mut Vec<Evidence>,
) -> Verdict {
    let Some(bound) = bind_keys(graph, view) else {
        evidence.push(Evidence::new(
            "unattributable join",
            format!(
                "{} — join key or side binding not statically recoverable",
                view.consumer,
            ),
        ));
        return Verdict::Unknown;
    };
    let mut covered_by: Vec<String> = Vec::new();
    let mut unknown = false;
    for (id, unit_test) in tests_on_model(ctx) {
        match test_key_facts(ctx.manifest, unit_test, &bound) {
            Some(facts) => {
                let exercised = match direction {
                    KeyDirection::NoMatch => facts.has_unmatched_left,
                    KeyDirection::Match => facts.has_matched_left,
                };
                if exercised {
                    covered_by.push(id.clone());
                }
            }
            None => unknown = true,
        }
    }
    if !covered_by.is_empty() {
        covered_by.sort();
        return Verdict::Covered { by: covered_by };
    }
    if unknown {
        evidence.push(Evidence::new(
            "unattributable given",
            format!(
                "{} — a unit test's rows are not statically countable (external fixture, non-literal sql, or ungiven seed input)",
                view.consumer,
            ),
        ));
        return Verdict::Unknown;
    }
    let sketch = match direction {
        KeyDirection::NoMatch if view.select_is_distinct => {
            evidence.push(Evidence::new(
                "instrument routing",
                format!(
                    "{} dedups the join output with DISTINCT — the data-test recommendation wins over a unit-test fixture (dedup after a fan-out join)",
                    view.consumer,
                ),
            ));
            Evidence::new("suggested data test", grain_data_test_sketch(&bound))
        }
        KeyDirection::NoMatch => Evidence::new("suggested given", no_match_given_sketch(&bound)),
        KeyDirection::Match => Evidence::new("suggested given", matching_given_sketch(&bound)),
    };
    evidence.push(sketch);
    Verdict::Uncovered
}

/// Detector for `join.left-null-propagation` (cute-dbt#173, catalog
/// class C4).
///
/// Trigger: a LEFT JOIN site whose right-side columns provably reach
/// the containing SELECT's projection. Fires on anti-join constructs
/// too — by design: [`resolve_supersedes`] silences it there (the
/// founder's "rules must recognize the pattern, not force suppression"
/// case), so a detector-level skip would mask the supersedes contract.
fn detect_join_left_null_propagation(ctx: &CheckContext<'_>) -> Vec<Finding<HeuristicId>> {
    if ctx.model.resource_type() != "model" {
        return Vec::new();
    }
    let Some(graph) = ctx.cte_graph else {
        return Vec::new();
    };
    left_join_sites(graph)
        .into_iter()
        .filter(|site| site.fact.projects_right_columns())
        .map(|site| {
            let mut evidence = vec![left_join_evidence(site.fact)];
            let verdict = key_match_verdict(
                ctx,
                graph,
                &site.fact.into(),
                KeyDirection::NoMatch,
                &mut evidence,
            );
            Finding::new(
                HeuristicId::JoinLeftNullPropagation,
                ctx.model.id().clone(),
                site.construct,
                verdict,
                evidence,
            )
        })
        .collect()
}

/// One negated-subquery site (cute-dbt#196): the construct
/// discriminator the anti-join check emits subquery-arm findings on.
/// `join.left-null-propagation` never enumerates these sites (it
/// consumes `left_join_facts` only), so supersedes resolution is
/// trivially correct per construct.
struct SubquerySite<'a> {
    construct: String,
    fact: &'a SubqueryFact,
}

/// Enumerate a graph's negated-subquery sites in source order,
/// deduplicating repeated `(kind, consumer, inner_leaf)` triples with a
/// `#<ordinal>` suffix (the [`left_join_sites`] discipline).
fn subquery_sites(graph: &CteGraph) -> Vec<SubquerySite<'_>> {
    let mut seen: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
    graph
        .subquery_facts()
        .iter()
        .map(|fact| {
            let kind = match fact.kind() {
                SubqueryKind::NotExists => "not_exists",
                SubqueryKind::NotIn => "not_in",
            };
            let ordinal = seen
                .entry((kind, fact.consumer(), fact.inner_leaf()))
                .and_modify(|n| *n += 1)
                .or_insert(1);
            let construct = if *ordinal == 1 {
                format!("{kind}[{}:{}]", fact.consumer(), fact.inner_leaf())
            } else {
                format!(
                    "{kind}[{}:{}#{}]",
                    fact.consumer(),
                    fact.inner_leaf(),
                    ordinal
                )
            };
            SubquerySite { construct, fact }
        })
        .collect()
}

/// The form-specific "anti-join" evidence naming a subquery construct
/// (cute-dbt#196): the rendered shape of the `NOT EXISTS` correlation
/// or the `NOT IN` membership pair, with the unrecoverable-key arm
/// spelled out honestly.
fn subquery_evidence(fact: &SubqueryFact) -> Evidence {
    match fact.kind() {
        SubqueryKind::NotExists => {
            let correlation = if fact.equi_keys().is_empty() {
                "<correlation keys not statically recoverable>".to_owned()
            } else {
                let pairs: Vec<String> = fact
                    .equi_keys()
                    .iter()
                    .map(|pair| {
                        format!(
                            "{}.{} = {}.{}",
                            fact.inner_leaf(),
                            pair.right_column(),
                            pair.left_leaf().unwrap_or("?"),
                            pair.left_column(),
                        )
                    })
                    .collect();
                pairs.join(" AND ")
            };
            Evidence::new(
                "anti-join (NOT EXISTS)",
                format!(
                    "{} — WHERE NOT EXISTS (SELECT … FROM {} WHERE {})",
                    fact.consumer(),
                    fact.inner_leaf(),
                    correlation,
                ),
            )
        }
        SubqueryKind::NotIn => {
            let membership = fact.equi_keys().first().map_or_else(
                || {
                    format!(
                        "<membership column not statically resolvable> NOT IN (SELECT … FROM {})",
                        fact.inner_leaf(),
                    )
                },
                |pair| {
                    format!(
                        "{}.{} NOT IN (SELECT {} FROM {})",
                        pair.left_leaf().unwrap_or("?"),
                        pair.left_column(),
                        pair.right_column(),
                        fact.inner_leaf(),
                    )
                },
            );
            Evidence::new(
                "anti-join (NOT IN)",
                format!("{} — WHERE {membership}", fact.consumer()),
            )
        }
    }
}

/// Detector for `join.anti-join` (cute-dbt#173, the agenda §4b
/// refinement; subquery arms added by cute-dbt#196).
///
/// Triggers, one finding per site:
/// - a LEFT JOIN site filtering `WHERE <right key> IS NULL` in a
///   top-level AND conjunct, the IS NULL column being one of the ON
///   equi-key right columns;
/// - a correlated `NOT EXISTS` / single-column `NOT IN` subquery site
///   (cute-dbt#196 — the lifted v1 exclusions), whose extracted
///   outer↔inner key pairs play the ON equi-key role.
///
/// All arms emit the INVERTED recommendation (a given row that DOES
/// match, proving the matched class is excluded) through the same
/// [`key_match_verdict`] machinery. The LEFT JOIN arm supersedes
/// `join.left-null-propagation` on the same construct; the subquery
/// constructs are never enumerated by left-null-propagation at all.
fn detect_join_anti_join(ctx: &CheckContext<'_>) -> Vec<Finding<HeuristicId>> {
    if ctx.model.resource_type() != "model" {
        return Vec::new();
    }
    let Some(graph) = ctx.cte_graph else {
        return Vec::new();
    };
    let mut findings: Vec<Finding<HeuristicId>> = left_join_sites(graph)
        .into_iter()
        .filter(|site| !anti_join_null_keys(site.fact).is_empty())
        .map(|site| {
            let mut evidence = vec![left_join_evidence(site.fact)];
            let null_keys = anti_join_null_keys(site.fact).join(", ");
            evidence.push(Evidence::new(
                "anti-join filter",
                format!("WHERE {}.{} IS NULL", site.fact.right_leaf(), null_keys),
            ));
            let verdict = key_match_verdict(
                ctx,
                graph,
                &site.fact.into(),
                KeyDirection::Match,
                &mut evidence,
            );
            Finding::new(
                HeuristicId::JoinAntiJoin,
                ctx.model.id().clone(),
                site.construct,
                verdict,
                evidence,
            )
        })
        .collect();
    findings.extend(subquery_sites(graph).into_iter().map(|site| {
        let mut evidence = vec![subquery_evidence(site.fact)];
        let verdict = key_match_verdict(
            ctx,
            graph,
            &site.fact.into(),
            KeyDirection::Match,
            &mut evidence,
        );
        Finding::new(
            HeuristicId::JoinAntiJoin,
            ctx.model.id().clone(),
            site.construct,
            verdict,
            evidence,
        )
    }));
    findings
}

// ---------------------------------------------------------------------
// incremental.branch-coverage — the is_incremental() true/false rollup
// (cute-dbt#164, coverage-intelligence rule #1).
// ---------------------------------------------------------------------

/// The construct discriminator for the incremental branch check — the
/// model-level `is_incremental()` fork declared by `config.materialized`.
const INCREMENTAL_BRANCH_CONSTRUCT: &str = "config.materialized";

/// A model's unit-test rollup over the two `is_incremental()` branches
/// (cute-dbt#164): which sides of the fork its tests compile.
///
/// A test with `overrides.macros.is_incremental = true` exercises the
/// incremental branch; an explicit `false` override — or NO override,
/// dbt's unit-test default — exercises the initial full-build branch.
/// (fusion stubs only *overridden* macros: `bind_override_macros` in
/// `dbt-tasks-sa/src/renderable/renderable/unit_test.rs`, `9977b6cb…`;
/// without the stub the unit test compiles the full-build branch.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchCoverage {
    /// The model has no unit tests at all.
    None,
    /// Only the full-build (false) branch is exercised.
    FalseOnly,
    /// Only the incremental (true) branch is exercised.
    TrueOnly,
    /// Both branches are exercised — the satisfied state.
    Both,
}

/// Classify a model's unit-test `is_incremental` override modes into the
/// four-state branch rollup. Pure and total: `Some(true)` counts toward
/// the incremental branch; `Some(false)` **and** `None` (no override)
/// count toward the full-build branch; no modes at all is
/// [`BranchCoverage::None`].
fn classify_branch_coverage<I>(modes: I) -> BranchCoverage
where
    I: IntoIterator<Item = Option<bool>>,
{
    let mut any_true = false;
    let mut any_false = false;
    for mode in modes {
        match mode {
            Some(true) => any_true = true,
            Some(false) | None => any_false = true,
        }
    }
    match (any_true, any_false) {
        (true, true) => BranchCoverage::Both,
        (true, false) => BranchCoverage::TrueOnly,
        (false, true) => BranchCoverage::FalseOnly,
        (false, false) => BranchCoverage::None,
    }
}

/// `true` when the model's resolved config marks dbt's microbatch
/// incremental strategy: `incremental_strategy = "microbatch"`
/// (`DbtIncrementalStrategy::Microbatch`, serde `snake_case` —
/// `dbt-schemas/src/schemas/common.rs`, dbt-fusion `9977b6cb…`) or any
/// non-null `event_time` config (the microbatch window column). The
/// declared cute-dbt#164 exclusion: microbatch models replay event-time
/// batch windows, not the `is_incremental()` fork, so the check stays
/// silent on them — never classified, never misclassified.
fn is_microbatch(config: &NodeConfig) -> bool {
    let microbatch_strategy = config
        .config()
        .get("incremental_strategy")
        .and_then(Value::as_str)
        .is_some_and(|strategy| strategy.eq_ignore_ascii_case("microbatch"));
    let event_time = config
        .config()
        .get("event_time")
        .is_some_and(|value| !value.is_null());
    microbatch_strategy || event_time
}

/// The copy-pasteable unit-test sketch for the missing incremental
/// (true) branch (the cute-dbt#164 recommendation payload).
fn incremental_run_sketch(model_bare: &str) -> String {
    format!(
        "- name: test_{model_bare}_incremental_run\n  model: {model_bare}\n  overrides:\n    macros:\n      is_incremental: true\n  given:\n    - input: this\n      rows:\n        - {{...}}   # the prior model state the incremental run reads\n    # plus the model's normal input givens\n  # expect: only the rows the incremental run emits (the delta), not the merged table",
    )
}

/// The copy-pasteable unit-test sketch for the missing full-build
/// (false) branch.
fn full_build_sketch(model_bare: &str) -> String {
    format!(
        "- name: test_{model_bare}_initial_build\n  model: {model_bare}\n  # no overrides block \u{2014} dbt compiles is_incremental() as false by default\n  given:\n    - input: ref('...')\n      rows:\n        - {{...}}\n  # expect: the full first-build output",
    )
}

/// Detector for `incremental.branch-coverage` (cute-dbt#164 —
/// coverage-intelligence rule #1).
///
/// Trigger: a `model` node with `config.materialized = "incremental"`,
/// excluding microbatch-strategy models ([`is_microbatch`] — the
/// declared rule-#1 exclusion). Satisfaction: the model's unit tests
/// exercise BOTH `is_incremental()` branches
/// ([`classify_branch_coverage`]).
///
/// Verdicts: BOTH ⇒ [`Verdict::Covered`] attributing every unit test on
/// the model (each one compiles one side of the fork); none /
/// false-only / true-only ⇒ [`Verdict::Uncovered`] with the missing
/// branch(es) named in evidence and a copy-pasteable unit-test sketch
/// per missing branch. There is deliberately no UNKNOWN arm: the inputs
/// (a string `materialized`, boolean overrides) are statically total —
/// every not-statically-known shape (absent/non-string materialization,
/// microbatch) emits NOTHING. Miss direction is silence, never
/// misclassification.
fn detect_incremental_branch_coverage(ctx: &CheckContext<'_>) -> Vec<Finding<HeuristicId>> {
    let check = HeuristicId::IncrementalBranchCoverage;
    if ctx.model.resource_type() != "model" {
        return Vec::new();
    }
    if ctx.model.config().materialized() != Some("incremental") {
        return Vec::new();
    }
    if is_microbatch(ctx.model.config()) {
        return Vec::new();
    }
    let tests = tests_on_model(ctx);
    let modes: Vec<Option<bool>> = tests
        .iter()
        .map(|(_, unit_test)| unit_test.is_incremental_mode())
        .collect();
    let coverage = classify_branch_coverage(modes.iter().copied());
    let true_count = modes.iter().filter(|mode| **mode == Some(true)).count();
    let false_count = tests.len() - true_count;
    let materialized = ctx
        .model
        .config()
        .config()
        .get("incremental_strategy")
        .and_then(Value::as_str)
        .map_or_else(
            || "incremental".to_owned(),
            |strategy| format!("incremental (strategy: {strategy})"),
        );
    let mut evidence = vec![Evidence::new("materialized", materialized)];
    // cute-dbt#256 — the authored bare name (ingested wire `name`, leaf
    // fallback): a versioned model's id leaf is the `.vN` suffix, and a
    // sketch saying `model: v2` would not be valid authoring YAML.
    let model_bare = ctx.model.bare_name().to_owned();
    // tests_on_model sorts by unit-test id, so `by` is already
    // deterministic.
    let by: Vec<String> = tests.iter().map(|(id, _)| (*id).clone()).collect();
    let verdict = branch_rollup_verdict(
        coverage,
        true_count,
        false_count,
        &model_bare,
        by,
        &mut evidence,
    );
    vec![Finding::new(
        check,
        ctx.model.id().clone(),
        INCREMENTAL_BRANCH_CONSTRUCT,
        verdict,
        evidence,
    )]
}

/// Turn the [`BranchCoverage`] rollup into the finding's verdict,
/// appending the per-state evidence: the rollup line, the missing
/// branch(es), and one `suggested given` unit-test sketch per missing
/// branch (lifted into copyable sketches by the renderer). `by` is the
/// sorted unit-test ids attributed on [`BranchCoverage::Both`].
fn branch_rollup_verdict(
    coverage: BranchCoverage,
    true_count: usize,
    false_count: usize,
    model_bare: &str,
    by: Vec<String>,
    evidence: &mut Vec<Evidence>,
) -> Verdict {
    match coverage {
        BranchCoverage::Both => {
            evidence.push(Evidence::new(
                "branch coverage",
                format!(
                    "both — {true_count} test(s) exercise the incremental (true) branch, {false_count} the initial full-build (false) branch",
                ),
            ));
            Verdict::Covered { by }
        }
        BranchCoverage::TrueOnly => {
            evidence.push(Evidence::new(
                "branch coverage",
                format!(
                    "true-only — {true_count} test(s) override is_incremental to true; none exercise the initial full-build (false) branch",
                ),
            ));
            evidence.push(Evidence::new(
                "missing branch",
                "the initial full-build branch (is_incremental() = false) runs in no unit test",
            ));
            evidence.push(Evidence::new(
                "suggested given",
                full_build_sketch(model_bare),
            ));
            Verdict::Uncovered
        }
        BranchCoverage::FalseOnly => {
            evidence.push(Evidence::new(
                "branch coverage",
                format!(
                    "false-only — {false_count} test(s) exercise the initial full-build branch (no override compiles is_incremental() as false); none override is_incremental to true",
                ),
            ));
            evidence.push(Evidence::new(
                "missing branch",
                "the incremental branch (is_incremental() = true) runs in no unit test",
            ));
            evidence.push(Evidence::new(
                "suggested given",
                incremental_run_sketch(model_bare),
            ));
            Verdict::Uncovered
        }
        BranchCoverage::None => {
            evidence.push(Evidence::new(
                "branch coverage",
                "none — the model has no unit tests; neither is_incremental() branch is exercised",
            ));
            evidence.push(Evidence::new(
                "missing branch",
                "both branches: neither the incremental (true) nor the initial full-build (false) side runs in any unit test",
            ));
            evidence.push(Evidence::new(
                "suggested given",
                incremental_run_sketch(model_bare),
            ));
            evidence.push(Evidence::new(
                "suggested given",
                full_build_sketch(model_bare),
            ));
            Verdict::Uncovered
        }
    }
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
        // `table`, not `incremental`: the grain check is
        // materialization-agnostic, and a table model keeps the
        // cute-dbt#164 incremental.branch-coverage check silent so these
        // tests stay single-concern (`single_finding`).
        model_with_config(
            ORDERS,
            &[("materialized", Value::from("table")), ("unique_key", key)],
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

    // ===== cute-dbt#259 — degraded backing (severity / where / limit) =====

    /// A covering unique test on `order_id` with the given flat config.
    fn unique_order_id_test(config: &[(&str, Value)]) -> Node {
        test_node(
            "test.shop.unique_orders_order_id",
            ORDERS,
            Some("order_id"),
            unique_metadata("order_id"),
            config,
        )
    }

    #[test]
    fn warn_severity_backing_attributes_as_degraded() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[("severity", Value::from("warn"))]),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec!["test.shop.unique_orders_order_id".to_owned()],
            },
            "a warn-severity test still attributes — degraded, never dropped",
        );
        assert_eq!(finding.degraded.len(), 1);
        assert_eq!(finding.degraded[0].by, "test.shop.unique_orders_order_id");
        assert_eq!(finding.degraded[0].causes.len(), 1);
        assert!(
            finding.degraded[0].causes[0].starts_with("severity: warn"),
            "the cause names the weakening config: {:?}",
            finding.degraded[0].causes,
        );
        assert!(finding.recommendation.is_none());
    }

    #[test]
    fn where_and_limit_causes_are_enumerated_together() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[
                ("where", Value::from("order_date >= '2024-01-01'")),
                ("limit", Value::from(100)),
            ]),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
        assert_eq!(finding.degraded.len(), 1);
        let causes = &finding.degraded[0].causes;
        assert_eq!(causes.len(), 2, "every cause enumerated: {causes:?}");
        assert!(
            causes[0].contains("order_date >= '2024-01-01'"),
            "the where cue quotes the filter: {causes:?}",
        );
        assert!(
            causes[1].starts_with("limit: 100"),
            "the limit cue names the cap: {causes:?}",
        );
    }

    #[test]
    fn unrecognized_severity_is_cued_with_the_raw_value() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[("severity", Value::from("loud"))]),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.degraded.len(), 1);
        assert!(
            finding.degraded[0].causes[0].contains("\"loud\"")
                && finding.degraded[0].causes[0].contains("not a recognized severity"),
            "the raw value is surfaced, never guessed at: {:?}",
            finding.degraded[0].causes,
        );
    }

    #[test]
    fn full_strength_backing_carries_no_degradation() {
        // Explicit error severity, null where/limit (the fusion
        // null-fill shape) — the default contract, no cues.
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[
                ("severity", Value::from("ERROR")),
                ("where", Value::Null),
                ("limit", Value::Null),
            ]),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
        assert!(finding.degraded.is_empty());
        // serde: the key is omitted entirely (payload byte-stability).
        let json = serde_json::to_value(&finding).expect("finding serializes");
        assert!(json.get("degraded").is_none());
    }

    #[test]
    fn mixed_clean_and_degraded_backing_cues_only_the_degraded_test() {
        let manifest = manifest_of(vec![
            orders_with_key(serde_json::json!(["customer_id", "order_date"])),
            test_node(
                "test.shop.a_unique_customer_id",
                ORDERS,
                Some("customer_id"),
                unique_metadata("customer_id"),
                &[],
            ),
            test_node(
                "test.shop.b_combo",
                ORDERS,
                None,
                combo_metadata(&["customer_id", "order_date"]),
                &[("severity", Value::from("warn"))],
            ),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        let Verdict::Covered { by } = &finding.verdict else {
            panic!("expected Covered, got {:?}", finding.verdict);
        };
        assert_eq!(by.len(), 2, "both tests attribute: {by:?}");
        assert_eq!(finding.degraded.len(), 1, "only the warn test is cued");
        assert_eq!(finding.degraded[0].by, "test.shop.b_combo");
        // The subset invariant: every degraded id is an attributed id.
        assert!(
            finding.degraded.iter().all(|d| by.contains(&d.by)),
            "degraded ⊆ by",
        );
    }

    #[test]
    fn degraded_serializes_per_test_with_causes() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[("severity", Value::from("warn"))]),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        let json = serde_json::to_value(&finding).expect("finding serializes");
        assert_eq!(
            json["degraded"][0]["by"],
            "test.shop.unique_orders_order_id"
        );
        assert!(
            json["degraded"][0]["causes"][0]
                .as_str()
                .is_some_and(|c| c.starts_with("severity: warn")),
            "causes serialize as the composed copy: {json}",
        );
    }

    // ===== cute-dbt#259 — exists but disabled, distinct from absent =====

    #[test]
    fn disabled_nodes_map_test_surfaces_exists_but_disabled() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[("enabled", Value::Bool(false))]),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Uncovered,
            "never counts as coverage"
        );
        let disabled: Vec<&Evidence> = finding
            .evidence
            .iter()
            .filter(|e| e.label == "exists but disabled")
            .collect();
        assert_eq!(disabled.len(), 1, "{:?}", finding.evidence);
        assert!(
            disabled[0]
                .value
                .contains("test.shop.unique_orders_order_id")
                && disabled[0].value.contains("(order_id)"),
            "the cue names the test and its columns: {}",
            disabled[0].value,
        );
        assert!(
            finding.degraded.is_empty(),
            "disabled tests never attribute"
        );
    }

    #[test]
    fn disabled_map_uniqueness_test_surfaces_exists_but_disabled() {
        use crate::domain::manifest::DisabledEntry;
        let entry = DisabledEntry::new("test").with_attachment(
            Some("order_id".to_owned()),
            Some(node_id(ORDERS)),
            Some(unique_metadata("order_id")),
        );
        let manifest = manifest_of(vec![orders_with_key(Value::from("order_id"))]).with_disabled(
            BTreeMap::from([(
                "test.shop.unique_orders_order_id.0123456789".to_owned(),
                vec![entry],
            )]),
        );
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "exists but disabled"
                    && e.value
                        .contains("test.shop.unique_orders_order_id.0123456789")),
            "the disabled-map entry surfaces by id: {:?}",
            finding.evidence,
        );
    }

    #[test]
    fn irrelevant_disabled_entries_never_surface() {
        use crate::domain::manifest::DisabledEntry;
        // A disabled accepted_values test (not a uniqueness signature), a
        // disabled uniqueness test on ANOTHER model, a disabled test with
        // a non-subset column, and a linkage-free disabled SINGULAR test
        // — none of them concern this grain.
        let other_check = DisabledEntry::new("test").with_attachment(
            Some("status".to_owned()),
            Some(node_id(ORDERS)),
            Some(TestMetadata::new(
                "accepted_values",
                None,
                serde_json::json!({ "column_name": "status" }),
            )),
        );
        let other_model = DisabledEntry::new("test").with_attachment(
            Some("order_id".to_owned()),
            Some(node_id("model.shop.other")),
            Some(unique_metadata("order_id")),
        );
        let wider = DisabledEntry::new("test").with_attachment(
            Some("status".to_owned()),
            Some(node_id(ORDERS)),
            Some(unique_metadata("status")),
        );
        let singular = DisabledEntry::new("test");
        let manifest = manifest_of(vec![orders_with_key(Value::from("order_id"))]).with_disabled(
            BTreeMap::from([
                ("test.shop.av".to_owned(), vec![other_check]),
                ("test.shop.um".to_owned(), vec![other_model]),
                ("test.shop.uw".to_owned(), vec![wider]),
                ("test.shop.assert_x".to_owned(), vec![singular]),
            ]),
        );
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert!(
            finding
                .evidence
                .iter()
                .all(|e| e.label != "exists but disabled"),
            "no irrelevant disabled entry surfaces: {:?}",
            finding.evidence,
        );
    }

    #[test]
    fn exists_but_disabled_rides_a_covered_finding_too() {
        // An enabled covering test AND a disabled twin: covered with
        // attribution, and the disabled fact still surfaces (in-row
        // honesty — the author should know the off switch exists).
        use crate::domain::manifest::DisabledEntry;
        let entry = DisabledEntry::new("test").with_attachment(
            Some("order_id".to_owned()),
            Some(node_id(ORDERS)),
            Some(unique_metadata("order_id")),
        );
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[]),
        ])
        .with_disabled(BTreeMap::from([(
            "test.shop.unique_orders_order_id_v2.aa00".to_owned(),
            vec![entry],
        )]));
        let finding = single_finding(run(&manifest, ORDERS));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "exists but disabled"),
            "{:?}",
            finding.evidence,
        );
    }

    // ===== cute-dbt#259 — singular tests via depends_on linkage =====

    /// An enabled singular (SQL-file) test node depending on `target`
    /// — no `test_metadata`, no `attached_node` (the cute-dbt#258
    /// linkage truth).
    fn singular_test(full_id: &str, target: &str) -> Node {
        Node::new(
            NodeId::new(full_id),
            "test",
            Checksum::new("sha256", "s"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(Vec::new(), vec![NodeId::new(target)]),
            None,
            NodeConfig::new(BTreeMap::new(), false),
            None,
            BTreeMap::new(),
        )
    }

    #[test]
    fn singular_test_degrades_an_unbacked_key_to_unknown() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            singular_test("test.shop.assert_orders_consistent", ORDERS),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Unknown,
            "a singular test may assert the grain — never a false Uncovered nag",
        );
        assert!(finding.recommendation.is_none());
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "generic backing"),
            "the evidence states what WAS checked: {:?}",
            finding.evidence,
        );
        assert!(
            finding.evidence.iter().any(|e| e.label == "singular test"
                && e.value.contains("test.shop.assert_orders_consistent")),
            "the singular tests are enumerated: {:?}",
            finding.evidence,
        );
    }

    #[test]
    fn singular_tests_never_attribute_when_generic_backing_exists() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            unique_order_id_test(&[]),
            singular_test("test.shop.assert_orders_consistent", ORDERS),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec!["test.shop.unique_orders_order_id".to_owned()],
            },
            "a singular test is never claimed as grain coverage",
        );
        assert!(
            finding.evidence.iter().all(|e| e.label != "singular test"),
            "covered findings stay quiet about singular tests: {:?}",
            finding.evidence,
        );
    }

    #[test]
    fn disabled_or_unrelated_singular_tests_keep_uncovered() {
        let mut disabled = singular_test("test.shop.assert_orders_consistent", ORDERS);
        disabled = Node::new(
            disabled.id().clone(),
            "test",
            Checksum::new("sha256", "s"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(Vec::new(), vec![node_id(ORDERS)]),
            None,
            NodeConfig::new(
                BTreeMap::from([("enabled".to_owned(), Value::Bool(false))]),
                false,
            ),
            None,
            BTreeMap::new(),
        );
        let elsewhere = singular_test("test.shop.assert_other_consistent", "model.shop.other");
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            disabled,
            elsewhere,
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        assert_eq!(
            finding.verdict,
            Verdict::Uncovered,
            "a disabled singular test asserts nothing; an unrelated one is not linkage",
        );
    }

    #[test]
    fn singular_evidence_is_sorted_by_id() {
        let manifest = manifest_of(vec![
            orders_with_key(Value::from("order_id")),
            singular_test("test.shop.b_assert", ORDERS),
            singular_test("test.shop.a_assert", ORDERS),
        ]);
        let finding = single_finding(run(&manifest, ORDERS));
        let singular: Vec<&str> = finding
            .evidence
            .iter()
            .filter(|e| e.label == "singular test")
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(singular.len(), 2);
        assert!(singular[0].starts_with("test.shop.a_assert"));
        assert!(singular[1].starts_with("test.shop.b_assert"));
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

    // ----- given_input_leaf: quoted-leaf extraction (cute-dbt#249) ---

    #[test]
    fn given_input_leaf_extracts_single_quoted_arguments() {
        // The pre-#249 pins: single-quoted ref()/source() stay accepted.
        assert_eq!(
            given_input_leaf("ref('stg_orders')").as_deref(),
            Some("stg_orders")
        );
        assert_eq!(
            given_input_leaf("ref('pkg', 'stg_orders')").as_deref(),
            Some("stg_orders")
        );
        assert_eq!(
            given_input_leaf("source('raw', 'orders')").as_deref(),
            Some("orders")
        );
        // A trailing non-string kwarg: the LAST string literal wins (the
        // versioned-ref tolerance the right-to-left scan has always
        // carried — `ref('orders', v=2)` binds the model, not the kwarg).
        assert_eq!(
            given_input_leaf("ref('orders', v=2)").as_deref(),
            Some("orders")
        );
        // Case-insensitive keyword + lowercased leaf.
        assert_eq!(
            given_input_leaf("REF('Stg_Orders')").as_deref(),
            Some("stg_orders")
        );
    }

    #[test]
    fn given_input_leaf_extracts_double_quoted_arguments() {
        // cute-dbt#249: dbt accepts either Python/Jinja quote style in a
        // given's `input:` and both engines ship the authored string
        // verbatim on the manifest wire (cute-dbt#245) — a double-quoted
        // given must extract identically to its single-quoted twin.
        assert_eq!(
            given_input_leaf(r#"ref("stg_orders")"#).as_deref(),
            Some("stg_orders")
        );
        assert_eq!(
            given_input_leaf(r#"ref("pkg", "stg_orders")"#).as_deref(),
            Some("stg_orders")
        );
        assert_eq!(
            given_input_leaf(r#"source("raw", "orders")"#).as_deref(),
            Some("orders")
        );
        // Per-argument quote style: each argument is its own string
        // literal, so mixing styles ACROSS arguments is engine-valid
        // (the cute-dbt#245 contract render's parse_source_ref pins).
        assert_eq!(
            given_input_leaf(r#"source("raw", 'orders')"#).as_deref(),
            Some("orders")
        );
        assert_eq!(
            given_input_leaf(r#"source('raw', "orders")"#).as_deref(),
            Some("orders")
        );
        assert_eq!(
            given_input_leaf(r#"ref("orders", v=2)"#).as_deref(),
            Some("orders")
        );
        assert_eq!(
            given_input_leaf(r#"REF("Stg_Orders")"#).as_deref(),
            Some("stg_orders")
        );
    }

    #[test]
    fn given_input_leaf_rejects_mixed_or_unbalanced_quotes_and_non_calls() {
        // The strip_matching_quotes contract (cute-dbt#245 / PR #248):
        // open and close must be the SAME character; a mixed pair or an
        // unbalanced quote fails open to None — never a half-stripped
        // garbage leaf.
        assert_eq!(given_input_leaf(r#"ref("stg_orders')"#), None);
        assert_eq!(given_input_leaf(r#"ref('stg_orders")"#), None);
        assert_eq!(given_input_leaf(r#"ref("stg_orders)"#), None);
        assert_eq!(given_input_leaf("ref('stg_orders)"), None);
        assert_eq!(given_input_leaf("ref(stg_orders)"), None);
        assert_eq!(given_input_leaf(r#"ref("")"#), None);
        assert_eq!(given_input_leaf("ref('')"), None);
        // `this` (incremental prior state) and non-call shapes stay None.
        assert_eq!(given_input_leaf("this"), None);
        assert_eq!(given_input_leaf("raw_orders"), None);
    }

    #[test]
    fn union_double_quoted_given_classifies_like_its_single_quoted_twin() {
        // cute-dbt#249 AC: a double-quoted given input classifies
        // arm-coverage IDENTICALLY to its single-quoted twin. Before the
        // fix, `ref("x")` lost its leaf binding entirely and a fed arm
        // degraded to a false UNCOVERED nag.
        let graph = charges_refunds_graph();
        let verdict_for = |charges_input: &str, refunds_input: &str| {
            let manifest = manifest_with_unit_tests(
                vec![payments_model()],
                vec![(
                    "unit_test.shop.fct_payments.test_fct_payments",
                    unit_test_on_payments(vec![
                        given(charges_input, serde_json::json!([{ "amount": 100 }])),
                        given(refunds_input, serde_json::json!([{ "amount": 40 }])),
                    ]),
                )],
            );
            single_finding(run_with_graph(&manifest, PAYMENTS, Some(&graph))).verdict
        };
        let single_quoted = verdict_for("ref('stg_charges')", "ref('stg_refunds')");
        let double_quoted = verdict_for(r#"ref("stg_charges")"#, r#"ref("stg_refunds")"#);
        assert!(
            matches!(single_quoted, Verdict::Covered { .. }),
            "the single-quoted twin is the established covered case",
        );
        assert_eq!(
            double_quoted, single_quoted,
            "quote style must not change the arm-coverage verdict",
        );
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

    // ===== join.left-null-propagation + join.anti-join (cute-dbt#173) =====

    use crate::domain::check_config::{
        CheckPolicy, ChecksConfig, SuppressRule, apply_check_policy, resolve_check_policy,
    };
    use crate::domain::cte::JoinKeyPair;

    const EMAILS: &str = "model.shop.fct_order_emails";

    fn emails_model() -> Node {
        // No unique_key (grain silent), no union edges in any test graph
        // (union silent) — every finding asserted below is the join
        // pair's.
        model_with_config(EMAILS, &[("materialized", Value::from("table"))])
    }

    /// A CTE node classified simple-FROM (the import/refine shape the
    /// closure binding requires), carrying engine-computed leaf refs.
    fn simple_cte(name: &str, refs: &[&str]) -> CteNode {
        CteNode::new(name, None, None, None)
            .with_shape_facts(true, refs.iter().map(|r| (*r).to_owned()).collect())
    }

    fn key_pair(left_leaf: &str, left: &str, right: &str) -> JoinKeyPair {
        JoinKeyPair::new(Some(left_leaf.to_owned()), left, right)
    }

    /// A terminal-body LEFT JOIN fact (the WITH-less canonical shape).
    fn join_fact(
        right: &str,
        pairs: Vec<JoinKeyPair>,
        nulls: &[&str],
        projects: bool,
        distinct: bool,
    ) -> LeftJoinFact {
        LeftJoinFact::new(
            "(final select)",
            right,
            pairs,
            nulls.iter().map(|s| (*s).to_owned()).collect(),
            projects,
            distinct,
        )
    }

    /// The catalog C4 worked-example fact: `stg_orders o LEFT JOIN
    /// stg_customers c ON o.customer_id = c.id`.
    fn orders_customers_fact(projects: bool, nulls: &[&str], distinct: bool) -> LeftJoinFact {
        join_fact(
            "stg_customers",
            vec![key_pair("stg_orders", "customer_id", "id")],
            nulls,
            projects,
            distinct,
        )
    }

    /// A graph with no CTE nodes carrying one terminal-body fact — the
    /// WITH-less direct-join model.
    fn direct_join_graph(fact: LeftJoinFact) -> CteGraph {
        CteGraph::new(vec![], vec![]).with_left_join_facts(vec![fact])
    }

    fn unit_test_on(model_bare: &str, givens: Vec<UnitTestGiven>) -> UnitTest {
        UnitTest::new(
            format!("test_{model_bare}"),
            NodeId::new(model_bare),
            givens,
            UnitTestExpect::new(serde_json::json!([{ "order_id": 1 }]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    const EMAILS_TEST: &str = "unit_test.shop.fct_order_emails.test_fct_order_emails";

    fn emails_manifest_with_test(givens: Vec<UnitTestGiven>) -> Manifest {
        manifest_with_unit_tests(
            vec![emails_model()],
            vec![(EMAILS_TEST, unit_test_on("fct_order_emails", givens))],
        )
    }

    const EMAILS_CONSTRUCT: &str = "left_join[(final select):stg_customers]";

    #[test]
    fn left_null_uncovered_with_zero_unit_tests_carries_the_no_match_sketch() {
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.check, HeuristicId::JoinLeftNullPropagation);
        assert_eq!(finding.tier, Tier::High);
        assert_eq!(finding.instrument, Instrument::Both);
        assert_eq!(finding.construct, EMAILS_CONSTRUCT);
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert!(finding.recommendation.is_some());
        let join = finding
            .evidence
            .iter()
            .find(|e| e.label == "left join")
            .expect("join evidence present");
        assert_eq!(
            join.value,
            "(final select) — LEFT JOIN stg_customers ON stg_orders.customer_id = stg_customers.id",
        );
        let sketch = finding
            .evidence
            .iter()
            .find(|e| e.label == "suggested given")
            .expect("no-match sketch present");
        assert_eq!(
            sketch.value,
            "- input: ref('stg_orders')\n  rows:\n    - {customer_id: 404}   # 404 has no match below — the no-match path\n- input: ref('stg_customers')\n  rows:\n    - {id: 1}\n# expect: the no-match row with NULL stg_customers columns (or the intended fallback)",
        );
    }

    #[test]
    fn left_null_is_silent_without_provable_right_projection() {
        // Paired negative test for the declared exclusion: right-side
        // columns reaching the output only through expressions are not
        // attributed — conservative, never a false fire.
        let graph = direct_join_graph(orders_customers_fact(false, &[], false));
        let manifest = manifest_of(vec![emails_model()]);
        assert!(run_with_graph(&manifest, EMAILS, Some(&graph)).is_empty());
    }

    #[test]
    fn left_null_covered_when_a_given_left_row_has_no_right_match() {
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        let manifest = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "customer_id": 1 }, { "customer_id": 404 }]),
            ),
            given("ref('stg_customers')", serde_json::json!([{ "id": 1 }])),
        ]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec![EMAILS_TEST.to_owned()],
            },
        );
        assert!(finding.recommendation.is_none());
    }

    #[test]
    fn left_null_uncovered_when_every_left_row_matches() {
        // The catalog C4 gap: every left given row has a right match —
        // the no-match path runs on zero rows in every test.
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        let manifest = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "customer_id": 1 }]),
            ),
            given("ref('stg_customers')", serde_json::json!([{ "id": 1 }])),
        ]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    #[test]
    fn left_null_null_or_absent_left_key_exercises_the_no_match_path() {
        // SQL join semantics: a NULL key never matches — a left row with
        // a NULL (or sparse-dict absent) key IS a no-match row.
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        for left_rows in [
            serde_json::json!([{ "customer_id": null }]),
            serde_json::json!([{ "customer_id": 1, "amount": 2 }, { "amount": 5 }]),
        ] {
            let manifest = emails_manifest_with_test(vec![
                given("ref('stg_orders')", left_rows.clone()),
                given("ref('stg_customers')", serde_json::json!([{ "id": 1 }])),
            ]);
            let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
            assert!(
                matches!(finding.verdict, Verdict::Covered { .. }),
                "left rows {left_rows} exercise the no-match path: {:?}",
                finding.verdict,
            );
        }
    }

    #[test]
    fn left_null_unrecoverable_key_is_unknown_never_uncovered() {
        // Paired negative test for the declared exclusion: a non-equi /
        // expression ON clause leaves no statically-recoverable key.
        let graph = direct_join_graph(join_fact("stg_customers", Vec::new(), &[], true, false));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Unknown);
        assert!(finding.recommendation.is_none());
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "unattributable join"),
        );
    }

    #[test]
    fn left_null_unresolvable_side_binding_is_unknown_never_uncovered() {
        // Paired negative test for the declared exclusion: the right
        // side is a CTE whose body is NOT the simple-FROM shape (a join
        // chain inside) — key columns may not survive, so the binding
        // degrades honestly.
        let graph = CteGraph::new(
            vec![cte("paid_orders", &["stg_orders", "stg_payments"])],
            vec![],
        )
        .with_left_join_facts(vec![join_fact(
            "paid_orders",
            vec![key_pair("stg_customers", "customer_id", "customer_id")],
            &[],
            true,
            false,
        )]);
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Unknown);
        assert!(finding.recommendation.is_none());
    }

    #[test]
    fn left_null_binds_through_a_simple_from_cte_closure() {
        // The dbt-style import-CTE chain: both join sides are CTEs whose
        // simple-FROM closures each read exactly one external relation —
        // givens bind at the bottom of the chain.
        let graph = CteGraph::new(
            vec![
                simple_cte("conditions", &["stg_conditions"]),
                simple_cte("condition_stats", &["conditions"]),
                simple_cte("patients", &["dim_patients"]),
            ],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        )
        .with_left_join_facts(vec![join_fact(
            "condition_stats",
            vec![key_pair("patients", "patient_id", "patient_id")],
            &[],
            true,
            false,
        )]);
        let manifest = emails_manifest_with_test(vec![
            given(
                "ref('dim_patients')",
                serde_json::json!([{ "patient_id": 404 }]),
            ),
            given(
                "ref('stg_conditions')",
                serde_json::json!([{ "patient_id": 1 }]),
            ),
        ]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert!(
            matches!(finding.verdict, Verdict::Covered { .. }),
            "closure-bound givens attribute coverage: {:?}",
            finding.verdict,
        );
    }

    #[test]
    fn left_null_external_fixture_or_opaque_sql_given_is_unknown() {
        // Paired negative tests for the declared exclusion: rows not in
        // the manifest are never judged.
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        let fixture_given = UnitTestGiven::new(
            "ref('stg_customers')",
            Value::Null,
            Some("csv".to_owned()),
            Some("customers_fixture".to_owned()),
        );
        let opaque_sql = UnitTestGiven::new(
            "ref('stg_customers')",
            Value::from("select * from somewhere"),
            Some("sql".to_owned()),
            None,
        );
        for right in [fixture_given, opaque_sql] {
            let manifest = emails_manifest_with_test(vec![
                given(
                    "ref('stg_orders')",
                    serde_json::json!([{ "customer_id": 9 }]),
                ),
                right,
            ]);
            let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
            assert_eq!(finding.verdict, Verdict::Unknown);
            assert!(finding.recommendation.is_none());
        }
    }

    #[test]
    fn left_null_ungiven_seed_side_is_unknown_never_uncovered() {
        // Paired negative test for the declared exclusion: an ungiven
        // seed input reads real seed data (fusion `render_unit_test`).
        let graph = direct_join_graph(join_fact(
            "raw_customers",
            vec![key_pair("stg_orders", "customer_id", "id")],
            &[],
            true,
            false,
        ));
        let seed = Node::new(
            NodeId::new("seed.shop.raw_customers"),
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
            vec![emails_model(), seed],
            vec![(
                EMAILS_TEST,
                unit_test_on(
                    "fct_order_emails",
                    vec![given(
                        "ref('stg_orders')",
                        serde_json::json!([{ "customer_id": 1 }]),
                    )],
                ),
            )],
        );
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Unknown);
    }

    #[test]
    fn left_null_ungiven_non_seed_right_is_the_empty_mock_and_covers() {
        // Pins the union.arm-coverage premise: only `given` entries are
        // mocked — an ungiven non-seed right side is empty, so every
        // left row IS a no-match row and the path runs.
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        let manifest = emails_manifest_with_test(vec![given(
            "ref('stg_orders')",
            serde_json::json!([{ "customer_id": 1 }]),
        )]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn left_null_distinct_dedup_routes_to_the_data_test() {
        // THE cute-dbt#173 instrument-routing AC (catalog C4/C10):
        // dedup after a fan-out join — the DATA-TEST recommendation wins
        // over the unit-test fixture; never unit-test-maximalist.
        let graph = direct_join_graph(orders_customers_fact(true, &[], true));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "instrument routing"
                    && e.value.contains("data-test recommendation wins")),
            "routing evidence present: {:?}",
            finding.evidence,
        );
        let sketch = finding
            .evidence
            .iter()
            .find(|e| e.label == "suggested data test")
            .expect("routed sketch is the data test");
        assert!(sketch.value.contains("- unique"), "{}", sketch.value);
        assert!(
            !finding
                .evidence
                .iter()
                .any(|e| e.label == "suggested given"),
            "the unit-test fixture sketch must NOT win on the dedup shape",
        );
    }

    #[test]
    fn left_null_csv_and_dict_cells_match_on_the_normalized_key() {
        // #127 value-normalization pays off: a fusion raw-csv "1" and a
        // dict 1 are the same key.
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        let csv_right = |body: &str| {
            UnitTestGiven::new(
                "ref('stg_customers')",
                Value::from(body.to_owned()),
                Some("csv".to_owned()),
                None,
            )
        };
        let matched = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "customer_id": 1 }]),
            ),
            csv_right("id\n1\n"),
        ]);
        let finding = single_finding(run_with_graph(&matched, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered, "csv 1 matches dict 1");
        let unmatched = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "customer_id": 1 }]),
            ),
            csv_right("id\n2\n"),
        ]);
        let finding = single_finding(run_with_graph(&unmatched, EMAILS, Some(&graph)));
        assert!(matches!(finding.verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn left_null_composite_key_requires_every_pair_to_match() {
        let graph = direct_join_graph(join_fact(
            "stg_rates",
            vec![
                key_pair("stg_orders", "currency", "currency"),
                key_pair("stg_orders", "order_date", "rate_date"),
            ],
            &[],
            true,
            false,
        ));
        let manifest = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "currency": "usd", "order_date": "2026-01-01" }]),
            ),
            given(
                "ref('stg_rates')",
                serde_json::json!([{ "currency": "usd", "rate_date": "2026-01-02" }]),
            ),
        ]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert!(
            matches!(finding.verdict, Verdict::Covered { .. }),
            "a half-matching composite key is a no-match row: {:?}",
            finding.verdict,
        );
    }

    #[test]
    fn repeated_join_sites_discriminate_by_ordinal() {
        let graph = CteGraph::new(vec![], vec![]).with_left_join_facts(vec![
            orders_customers_fact(true, &[], false),
            orders_customers_fact(true, &[], false),
        ]);
        let manifest = manifest_of(vec![emails_model()]);
        let findings = run_with_graph(&manifest, EMAILS, Some(&graph));
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].construct, EMAILS_CONSTRUCT);
        assert_eq!(
            findings[1].construct,
            "left_join[(final select):stg_customers#2]",
        );
    }

    // ----- join.anti-join ---------------------------------------------

    #[test]
    fn anti_join_fires_with_the_inverted_recommendation() {
        let graph = direct_join_graph(orders_customers_fact(false, &["id"], false));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.check, HeuristicId::JoinAntiJoin);
        assert_eq!(finding.tier, Tier::High);
        assert_eq!(finding.instrument, Instrument::UnitTest);
        assert_eq!(finding.construct, EMAILS_CONSTRUCT);
        assert_eq!(finding.verdict, Verdict::Uncovered);
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "anti-join filter"
                    && e.value == "WHERE stg_customers.id IS NULL"),
            "{:?}",
            finding.evidence,
        );
        let sketch = finding
            .evidence
            .iter()
            .find(|e| e.label == "suggested given")
            .expect("inverted sketch present");
        assert_eq!(
            sketch.value,
            "- input: ref('stg_orders')\n  rows:\n    - {customer_id: 1}   # matches the right row below\n- input: ref('stg_customers')\n  rows:\n    - {id: 1}\n# expect: rows WITHOUT the matched left row — the anti-join must exclude it",
        );
    }

    #[test]
    fn anti_join_is_silent_when_is_null_is_on_a_non_key_column() {
        // Paired negative test for the declared exclusion: IS NULL on a
        // non-key right column is a data filter, not the anti-join
        // idiom — left-null-propagation governs the construct.
        let graph = direct_join_graph(orders_customers_fact(true, &["email"], false));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.check, HeuristicId::JoinLeftNullPropagation);
    }

    #[test]
    fn anti_join_satisfied_by_a_matching_given_pair() {
        // The INVERSION pinned: the exact fixture that leaves
        // left-null-propagation uncovered (every left row matches) is
        // what SATISFIES the anti-join.
        let graph = direct_join_graph(orders_customers_fact(false, &["id"], false));
        let manifest = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "customer_id": 1 }]),
            ),
            given("ref('stg_customers')", serde_json::json!([{ "id": 1 }])),
        ]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec![EMAILS_TEST.to_owned()],
            },
        );
    }

    #[test]
    fn anti_join_uncovered_when_no_given_row_matches() {
        let graph = direct_join_graph(orders_customers_fact(false, &["id"], false));
        let manifest = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "customer_id": 404 }]),
            ),
            given("ref('stg_customers')", serde_json::json!([{ "id": 1 }])),
        ]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Uncovered);
    }

    // ----- join.anti-join subquery arms (cute-dbt#196) -----------------

    use crate::domain::cte::{SubqueryFact, SubqueryKind};

    /// The NOT EXISTS twin of [`orders_customers_fact`]: `stg_orders o
    /// WHERE NOT EXISTS (SELECT 1 FROM stg_customers c WHERE
    /// c.id = o.customer_id)` — same key vocabulary, inner relation in
    /// the right-leaf role.
    fn not_exists_fact(pairs: Vec<JoinKeyPair>) -> SubqueryFact {
        SubqueryFact::new(
            SubqueryKind::NotExists,
            "(final select)",
            "stg_customers",
            pairs,
        )
    }

    fn subquery_graph(fact: SubqueryFact) -> CteGraph {
        CteGraph::new(vec![], vec![]).with_subquery_facts(vec![fact])
    }

    #[test]
    fn not_exists_site_fires_anti_join_with_the_inverted_recommendation() {
        let graph = subquery_graph(not_exists_fact(vec![key_pair(
            "stg_orders",
            "customer_id",
            "id",
        )]));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.check, HeuristicId::JoinAntiJoin);
        assert_eq!(
            finding.construct,
            "not_exists[(final select):stg_customers]"
        );
        assert_eq!(finding.verdict, Verdict::Uncovered);
        let form = finding
            .evidence
            .iter()
            .find(|e| e.label == "anti-join (NOT EXISTS)")
            .expect("form-specific evidence present");
        assert_eq!(
            form.value,
            "(final select) — WHERE NOT EXISTS (SELECT … FROM stg_customers WHERE stg_customers.id = stg_orders.customer_id)",
        );
        let sketch = finding
            .evidence
            .iter()
            .find(|e| e.label == "suggested given")
            .expect("inverted sketch present");
        assert_eq!(
            sketch.value,
            "- input: ref('stg_orders')\n  rows:\n    - {customer_id: 1}   # matches the right row below\n- input: ref('stg_customers')\n  rows:\n    - {id: 1}\n# expect: rows WITHOUT the matched left row — the anti-join must exclude it",
            "the SAME inverted sketch builder serves the subquery arm",
        );
    }

    #[test]
    fn not_in_site_fires_anti_join_with_the_membership_evidence() {
        let graph = subquery_graph(SubqueryFact::new(
            SubqueryKind::NotIn,
            "(final select)",
            "stg_refunds",
            vec![key_pair("stg_orders", "order_id", "order_id")],
        ));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.check, HeuristicId::JoinAntiJoin);
        assert_eq!(finding.construct, "not_in[(final select):stg_refunds]");
        assert_eq!(finding.verdict, Verdict::Uncovered);
        let form = finding
            .evidence
            .iter()
            .find(|e| e.label == "anti-join (NOT IN)")
            .expect("form-specific evidence present");
        assert_eq!(
            form.value,
            "(final select) — WHERE stg_orders.order_id NOT IN (SELECT order_id FROM stg_refunds)",
        );
    }

    #[test]
    fn subquery_site_covered_by_a_matching_given_pair_with_attribution() {
        // The satisfaction inversion carries over: the matching pair
        // that proves the exclusion COVERS the subquery arm, attributed.
        let graph = subquery_graph(not_exists_fact(vec![key_pair(
            "stg_orders",
            "customer_id",
            "id",
        )]));
        let manifest = emails_manifest_with_test(vec![
            given(
                "ref('stg_orders')",
                serde_json::json!([{ "customer_id": 1 }]),
            ),
            given("ref('stg_customers')", serde_json::json!([{ "id": 1 }])),
        ]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec![EMAILS_TEST.to_owned()],
            },
        );
        assert!(finding.recommendation.is_none());
    }

    #[test]
    fn subquery_site_with_empty_keys_is_unknown_never_uncovered() {
        // The unrecoverable-key degrade mirrors the LEFT JOIN path:
        // empty equi keys fail the bind — honest UNKNOWN.
        let graph = subquery_graph(not_exists_fact(Vec::new()));
        let manifest = manifest_of(vec![emails_model()]);
        let finding = single_finding(run_with_graph(&manifest, EMAILS, Some(&graph)));
        assert_eq!(finding.verdict, Verdict::Unknown);
        assert!(finding.recommendation.is_none());
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.label == "unattributable join"),
        );
    }

    #[test]
    fn left_null_never_fires_on_subquery_constructs() {
        // join.left-null-propagation consumes left_join_facts ONLY — a
        // graph carrying nothing but subquery facts can never trip it
        // (verified, not assumed: the supersedes contract on subquery
        // constructs is trivially correct because there is nothing to
        // supersede).
        let graph = subquery_graph(not_exists_fact(vec![key_pair(
            "stg_orders",
            "customer_id",
            "id",
        )]));
        let manifest = manifest_of(vec![emails_model()]);
        let findings = run_with_graph(&manifest, EMAILS, Some(&graph));
        assert!(
            findings
                .iter()
                .all(|f| f.check != HeuristicId::JoinLeftNullPropagation),
            "{findings:?}",
        );
    }

    #[test]
    fn repeated_subquery_sites_discriminate_by_ordinal() {
        let graph = CteGraph::new(vec![], vec![]).with_subquery_facts(vec![
            not_exists_fact(vec![key_pair("stg_orders", "customer_id", "id")]),
            not_exists_fact(vec![key_pair("stg_orders", "customer_id", "id")]),
        ]);
        let manifest = manifest_of(vec![emails_model()]);
        let findings = run_with_graph(&manifest, EMAILS, Some(&graph));
        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].construct,
            "not_exists[(final select):stg_customers]"
        );
        assert_eq!(
            findings[1].construct,
            "not_exists[(final select):stg_customers#2]"
        );
    }

    #[test]
    fn left_join_and_subquery_sites_coexist_on_one_graph() {
        // A model can carry BOTH anti-join forms; each site gets its
        // own finding under its own construct.
        let graph = CteGraph::new(vec![], vec![])
            .with_left_join_facts(vec![orders_customers_fact(false, &["id"], false)])
            .with_subquery_facts(vec![SubqueryFact::new(
                SubqueryKind::NotIn,
                "(final select)",
                "stg_refunds",
                vec![key_pair("stg_orders", "order_id", "order_id")],
            )]);
        let manifest = manifest_of(vec![emails_model()]);
        let findings = run_with_graph(&manifest, EMAILS, Some(&graph));
        let constructs: Vec<&str> = findings
            .iter()
            .filter(|f| f.check == HeuristicId::JoinAntiJoin)
            .map(|f| f.construct.as_str())
            .collect();
        assert_eq!(
            constructs,
            vec![
                "left_join[(final select):stg_customers]",
                "not_in[(final select):stg_refunds]",
            ],
        );
    }

    // ----- the supersedes showcase (production registry) ---------------

    /// The `SELECT * … LEFT JOIN … WHERE right.key IS NULL` shape: both
    /// detectors' conditions match on the SAME construct.
    fn anti_join_with_star_projection() -> CteGraph {
        direct_join_graph(orders_customers_fact(true, &["id"], false))
    }

    #[test]
    fn anti_join_supersedes_left_null_on_the_same_construct() {
        // THE cute-dbt#173 AC, end-to-end on the PRODUCTION registry:
        // stage 1 emits BOTH findings (proving left-null fired and was
        // silenced by RESOLUTION, not by a detector-level skip), stage 2
        // keeps only the anti-join.
        let graph = anti_join_with_star_projection();
        let manifest = manifest_of(vec![emails_model()]);
        let model = manifest.node(&node_id(EMAILS)).expect("model exists");
        let ctx = CheckContext {
            manifest: &manifest,
            model,
            cte_graph: Some(&graph),
        };
        let evaluated = evaluate_all::<HeuristicId>(&ctx);
        let evaluated_checks: Vec<HeuristicId> = checks_of(&evaluated);
        assert!(
            evaluated_checks.contains(&HeuristicId::JoinLeftNullPropagation),
            "left-null FIRES on the anti-join construct (its own conditions match)",
        );
        assert!(evaluated_checks.contains(&HeuristicId::JoinAntiJoin));
        let constructs: BTreeSet<&str> = evaluated.iter().map(|f| f.construct.as_str()).collect();
        assert_eq!(
            constructs.len(),
            1,
            "both checks discriminate the SAME construct: {constructs:?}",
        );
        let resolved = model_findings(&manifest, model, Some(&graph));
        assert_eq!(
            checks_of(&resolved),
            vec![HeuristicId::JoinAntiJoin],
            "resolution silences left-null on the anti-join construct",
        );
    }

    #[test]
    fn left_null_survives_alone_when_the_anti_join_shape_is_absent() {
        let graph = direct_join_graph(orders_customers_fact(true, &[], false));
        let manifest = manifest_of(vec![emails_model()]);
        let findings = run_with_graph(&manifest, EMAILS, Some(&graph));
        assert_eq!(
            checks_of(&findings),
            vec![HeuristicId::JoinLeftNullPropagation],
        );
    }

    #[test]
    fn disabling_anti_join_via_checks_config_never_resurrects_left_null() {
        // THE display-layer invariant (cute-dbt#173 AC), against the
        // REAL registry through #193's [checks] config path: disabling
        // the superseding check removes its finding WITHOUT resurrecting
        // the superseded one.
        let graph = anti_join_with_star_projection();
        let manifest = manifest_of(vec![emails_model()]);
        let model = manifest.node(&node_id(EMAILS)).expect("model exists");
        let resolved = model_findings(&manifest, model, Some(&graph));
        let config: ChecksConfig =
            toml::from_str("disable = [\"join.anti-join\"]").expect("config parses");
        let policy = resolve_check_policy::<HeuristicId>(&config).expect("policy resolves");
        let displayed = apply_check_policy(resolved, &policy);
        assert!(
            !displayed.iter().any(|f| f.check.spec().group == "join"),
            "no join finding may survive — disabling anti-join must NOT resurrect left-null: {displayed:?}",
        );
    }

    #[test]
    fn suppressing_anti_join_marks_it_and_never_resurrects_left_null() {
        let graph = anti_join_with_star_projection();
        let manifest = manifest_of(vec![emails_model()]);
        let model = manifest.node(&node_id(EMAILS)).expect("model exists");
        let resolved = model_findings(&manifest, model, Some(&graph));
        let policy = CheckPolicy {
            suppressions: vec![SuppressRule {
                check: HeuristicId::JoinAntiJoin,
                model: "fct_order_emails".to_owned(),
                reason: Some("exclusion proven downstream".to_owned()),
                source: SuppressionSource::Config,
            }],
            ..Default::default()
        };
        let displayed = apply_check_policy(resolved, &policy);
        assert_eq!(checks_of(&displayed), vec![HeuristicId::JoinAntiJoin]);
        assert!(
            displayed[0].suppressed.is_some(),
            "the anti-join finding is kept and marked",
        );
    }

    #[test]
    fn production_registry_pins_the_anti_join_supersedes_edge() {
        // Registry-generic production-shape assertion extended to the
        // cute-dbt#173 pair: the edge exists, points at the general
        // check, and the graph stays acyclic.
        assert_eq!(
            HeuristicId::JoinAntiJoin.spec().supersedes,
            &[HeuristicId::JoinLeftNullPropagation],
        );
        assert!(
            HeuristicId::JoinLeftNullPropagation
                .spec()
                .supersedes
                .is_empty(),
        );
        assert!(supersedes_is_acyclic::<HeuristicId>());
    }

    // ===== incremental.branch-coverage detector (cute-dbt#164) =======

    const EVENTS_INC: &str = "model.shop.order_events";

    /// An incremental model with an optional extra flat-config pair.
    fn incremental_model(extra: &[(&str, Value)]) -> Node {
        let mut config = vec![("materialized", Value::from("incremental"))];
        config.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
        model_with_config(EVENTS_INC, &config)
    }

    /// A unit test on the incremental model carrying an explicit
    /// `is_incremental` override mode (`None` = no override, dbt's
    /// full-build default).
    fn mode_test(name: &str, mode: Option<bool>) -> UnitTest {
        UnitTest::new(
            name,
            NodeId::new("order_events"),
            Vec::new(),
            UnitTestExpect::new(serde_json::json!([{ "order_id": 1 }]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_incremental_mode(mode)
    }

    /// Manifest of the incremental model plus mode-carrying unit tests,
    /// keyed `unit_test.shop.order_events.<name>`.
    fn incremental_manifest(extra: &[(&str, Value)], modes: &[(&str, Option<bool>)]) -> Manifest {
        let tests: Vec<(String, UnitTest)> = modes
            .iter()
            .map(|(name, mode)| {
                (
                    format!("unit_test.shop.order_events.{name}"),
                    mode_test(name, *mode),
                )
            })
            .collect();
        Manifest::new(
            ManifestMetadata::new("v12"),
            [incremental_model(extra)]
                .into_iter()
                .map(|n| (n.id().clone(), n))
                .collect(),
            tests.into_iter().collect(),
            HashMap::new(),
        )
    }

    /// The incremental.branch-coverage findings only.
    fn incremental_findings(manifest: &Manifest) -> Vec<Finding<HeuristicId>> {
        run(manifest, EVENTS_INC)
            .into_iter()
            .filter(|f| f.check == HeuristicId::IncrementalBranchCoverage)
            .collect()
    }

    #[test]
    fn branch_classifier_covers_all_four_states_exhaustively() {
        // Exhaustive coverage over sampling (the StateComparator test
        // posture — no proptest dep): EVERY multiset of override modes
        // up to length 3 classifies Both iff a true-exerciser AND a
        // false-exerciser are present; FalseOnly/TrueOnly iff only one
        // side is; None iff the mode list is empty.
        let modes = [Some(true), Some(false), None];
        let mut cases: Vec<Vec<Option<bool>>> = vec![vec![]];
        for a in modes {
            cases.push(vec![a]);
            for b in modes {
                cases.push(vec![a, b]);
                for c in modes {
                    cases.push(vec![a, b, c]);
                }
            }
        }
        for case in cases {
            let any_true = case.contains(&Some(true));
            let any_false = case.iter().any(|m| !matches!(m, Some(true)));
            let expected = match (any_true, any_false) {
                (true, true) => BranchCoverage::Both,
                (true, false) => BranchCoverage::TrueOnly,
                (false, true) => BranchCoverage::FalseOnly,
                (false, false) => BranchCoverage::None,
            };
            assert_eq!(
                classify_branch_coverage(case.iter().copied()),
                expected,
                "case {case:?}"
            );
        }
    }

    #[test]
    fn no_override_defaults_to_the_full_build_branch() {
        // The cute-dbt#164 AC case: a single test with NO override
        // exercises the false branch (dbt compiles is_incremental() as
        // false in unit tests by default) — the incremental branch is
        // the gap, and the sketch recommends the true override.
        let manifest = incremental_manifest(&[], &[("test_full_build", None)]);
        let finding = single_finding(incremental_findings(&manifest));
        assert_eq!(finding.tier, Tier::High);
        assert_eq!(finding.instrument, Instrument::UnitTest);
        assert_eq!(finding.construct, INCREMENTAL_BRANCH_CONSTRUCT);
        assert_eq!(finding.verdict, Verdict::Uncovered);
        let rollup = finding
            .evidence
            .iter()
            .find(|e| e.label == "branch coverage")
            .expect("rollup evidence present");
        assert!(
            rollup.value.starts_with("false-only"),
            "no-override classifies false-only: {}",
            rollup.value,
        );
        let sketch = finding
            .evidence
            .iter()
            .find(|e| e.label == "suggested given")
            .expect("sketch present");
        assert!(
            sketch.value.contains("is_incremental: true") && sketch.value.contains("input: this"),
            "the sketch recommends the true override + a `this` given: {}",
            sketch.value,
        );
        assert!(finding.recommendation.is_some());
    }

    #[test]
    fn true_only_tests_leave_the_full_build_branch_uncovered() {
        let manifest = incremental_manifest(
            &[],
            &[("test_inc_a", Some(true)), ("test_inc_b", Some(true))],
        );
        let finding = single_finding(incremental_findings(&manifest));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        let rollup = finding
            .evidence
            .iter()
            .find(|e| e.label == "branch coverage")
            .expect("rollup evidence present");
        assert!(
            rollup.value.starts_with("true-only"),
            "true-only rollup: {}",
            rollup.value,
        );
        let sketch = finding
            .evidence
            .iter()
            .find(|e| e.label == "suggested given")
            .expect("sketch present");
        assert!(
            sketch.value.contains("no overrides block"),
            "the missing-false sketch is the no-override test: {}",
            sketch.value,
        );
    }

    #[test]
    fn incremental_model_with_no_tests_fires_none_with_both_sketches() {
        let manifest = incremental_manifest(&[], &[]);
        let finding = single_finding(incremental_findings(&manifest));
        assert_eq!(finding.verdict, Verdict::Uncovered);
        let rollup = finding
            .evidence
            .iter()
            .find(|e| e.label == "branch coverage")
            .expect("rollup evidence present");
        assert!(rollup.value.starts_with("none"), "{}", rollup.value);
        let sketches: Vec<&Evidence> = finding
            .evidence
            .iter()
            .filter(|e| e.label == "suggested given")
            .collect();
        assert_eq!(
            sketches.len(),
            2,
            "a test-less incremental model gets one sketch per branch"
        );
    }

    #[test]
    fn incremental_sketches_name_a_versioned_model_by_its_ingested_name() {
        // cute-dbt#256 (the #254 handoff): a versioned model's id leaf
        // is the version suffix — a sketch saying `model: v2` is not
        // valid authoring YAML. The ingested wire `name` is the
        // truthful handle; bare_name's leaf fallback preserves the
        // pre-#256 sketch text for fixtures without a name.
        let versioned = model_with_config(
            "model.shop.order_events.v2",
            &[("materialized", Value::from("incremental"))],
        )
        .with_identity(Some("order_events".to_owned()), Some("shop".to_owned()));
        let manifest = Manifest::new(
            ManifestMetadata::new("v12"),
            [versioned]
                .into_iter()
                .map(|n| (n.id().clone(), n))
                .collect(),
            HashMap::new(),
            HashMap::new(),
        );
        let finding = single_finding(
            run(&manifest, "model.shop.order_events.v2")
                .into_iter()
                .filter(|f| f.check == HeuristicId::IncrementalBranchCoverage)
                .collect(),
        );
        let sketches: Vec<&str> = finding
            .evidence
            .iter()
            .filter(|e| e.label == "suggested given")
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(sketches.len(), 2);
        for sketch in sketches {
            assert!(
                sketch.contains("model: order_events"),
                "the sketch names the authored model, not the version suffix: {sketch}",
            );
            assert!(
                !sketch.contains("model: v2"),
                "no version-suffix model label: {sketch}",
            );
        }
    }

    #[test]
    fn both_branches_covered_attributes_every_test() {
        // An explicit-false override AND a no-override test both count
        // toward the full-build branch; either pairing with a true
        // override is BOTH.
        let manifest = incremental_manifest(
            &[("incremental_strategy", Value::from("merge"))],
            &[
                ("test_full_build", Some(false)),
                ("test_incremental_run", Some(true)),
            ],
        );
        let finding = single_finding(incremental_findings(&manifest));
        assert_eq!(
            finding.verdict,
            Verdict::Covered {
                by: vec![
                    "unit_test.shop.order_events.test_full_build".to_owned(),
                    "unit_test.shop.order_events.test_incremental_run".to_owned(),
                ],
            },
            "BOTH attributes every test on the model, sorted by id",
        );
        assert!(finding.recommendation.is_none());
        let materialized = finding
            .evidence
            .iter()
            .find(|e| e.label == "materialized")
            .expect("materialized evidence present");
        assert_eq!(materialized.value, "incremental (strategy: merge)");
    }

    #[test]
    fn non_incremental_and_unknown_materializations_emit_nothing() {
        // Miss direction is silence, never misclassification: view /
        // table / ephemeral, an absent materialized key, and a
        // non-string value all stay silent even with zero unit tests.
        for config in [
            vec![("materialized", Value::from("view"))],
            vec![("materialized", Value::from("table"))],
            vec![("materialized", Value::from("ephemeral"))],
            vec![("materialized", Value::from(42))],
            vec![("materialized", Value::Null)],
            vec![],
        ] {
            let manifest = manifest_of(vec![model_with_config(EVENTS_INC, &config)]);
            assert!(
                incremental_findings(&manifest).is_empty(),
                "config {config:?} must emit no incremental finding",
            );
        }
    }

    #[test]
    fn microbatch_models_are_never_classified() {
        // The declared rule-#1 exclusion: microbatch (by strategy or by
        // a non-null event_time) is OUT — silent even with a coverage
        // gap that would otherwise fire.
        for extra in [
            vec![("incremental_strategy", Value::from("microbatch"))],
            vec![("event_time", Value::from("occurred_at"))],
            vec![
                ("incremental_strategy", Value::from("microbatch")),
                ("event_time", Value::from("occurred_at")),
            ],
        ] {
            let manifest = incremental_manifest(&extra, &[("test_full_build", None)]);
            assert!(
                incremental_findings(&manifest).is_empty(),
                "microbatch config {extra:?} must emit no finding",
            );
        }
        // A null event_time is fusion's unset Option fill (cute-dbt#145)
        // — NOT microbatch; the check classifies normally.
        let manifest =
            incremental_manifest(&[("event_time", Value::Null)], &[("test_full_build", None)]);
        assert_eq!(incremental_findings(&manifest).len(), 1);
        // A non-microbatch strategy string is classified normally too.
        let manifest = incremental_manifest(
            &[("incremental_strategy", Value::from("delete+insert"))],
            &[("test_full_build", None)],
        );
        assert_eq!(incremental_findings(&manifest).len(), 1);
    }

    #[test]
    fn non_model_resource_types_are_silent_for_the_incremental_check() {
        // A snapshot node can carry materialized config shapes; only
        // `model` nodes trigger the check.
        let mut config = BTreeMap::new();
        config.insert("materialized".to_owned(), Value::from("incremental"));
        let node = Node::new(
            NodeId::new("snapshot.shop.order_events"),
            "snapshot",
            Checksum::new("sha256", "x"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_of(vec![node]);
        let model = manifest
            .node(&NodeId::new("snapshot.shop.order_events"))
            .expect("node exists");
        let findings = model_findings(&manifest, model, None);
        assert!(
            findings
                .iter()
                .all(|f| f.check != HeuristicId::IncrementalBranchCoverage),
            "non-model resource types never trigger: {findings:?}",
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

    // ===== cute-dbt#260 Slice 3: enforcement.constraint-unbacked =====

    /// A model with a model-level constraint of `kind` on `column`.
    fn model_with_constraint(full_id: &str, kind: &str, column: &str) -> Node {
        let constraint = crate::domain::manifest::Constraint::new(
            kind,
            vec![column.to_owned()],
            None,
            None,
            None,
            Vec::new(),
        );
        model_with_config(full_id, &[]).with_contract_facts(vec![constraint], Vec::new(), None)
    }

    /// A manifest with an adapter type set on its metadata.
    fn manifest_on_adapter(adapter: &str, nodes: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12").with_adapter_type(Some(adapter.to_owned())),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[test]
    fn enforcement_fires_for_a_declared_unbacked_pk_on_a_metadata_only_adapter() {
        // duckdb: PK is NotEnforced → a declared PK with no unique test
        // is UNCOVERED.
        let manifest = manifest_on_adapter(
            "duckdb",
            vec![model_with_constraint(
                "model.shop.orders",
                "primary_key",
                "id",
            )],
        );
        let findings = run(&manifest, "model.shop.orders");
        let enforcement: Vec<_> = findings
            .iter()
            .filter(|f| f.check == HeuristicId::EnforcementConstraintUnbacked)
            .collect();
        assert_eq!(enforcement.len(), 1);
        assert_eq!(enforcement[0].verdict, Verdict::Uncovered);
        assert_eq!(enforcement[0].construct, "constraint.primary_key[id]");
    }

    #[test]
    fn enforcement_silent_when_a_backing_unique_test_exists() {
        let manifest = manifest_on_adapter(
            "duckdb",
            vec![
                model_with_constraint("model.shop.orders", "primary_key", "id"),
                test_node(
                    "test.shop.u",
                    "model.shop.orders",
                    Some("id"),
                    unique_metadata("id"),
                    &[],
                ),
            ],
        );
        let findings = run(&manifest, "model.shop.orders");
        let enforcement: Vec<_> = findings
            .iter()
            .filter(|f| f.check == HeuristicId::EnforcementConstraintUnbacked)
            .collect();
        assert_eq!(enforcement.len(), 1);
        // Backed ⇒ Covered, not a gap.
        assert!(matches!(enforcement[0].verdict, Verdict::Covered { .. }));
    }

    #[test]
    fn enforcement_silent_when_the_adapter_enforces_the_constraint() {
        // not_null on duckdb is Enforced → never a gap.
        let manifest = manifest_on_adapter(
            "duckdb",
            vec![model_with_constraint("model.shop.orders", "not_null", "id")],
        );
        let findings = run(&manifest, "model.shop.orders");
        assert!(
            !findings
                .iter()
                .any(|f| f.check == HeuristicId::EnforcementConstraintUnbacked),
            "an enforced constraint is never an enforcement gap",
        );
    }

    #[test]
    fn enforcement_silent_without_an_adapter_type() {
        // No adapter_type ⇒ the matrix can't be applied ⇒ stay silent.
        let manifest = manifest_of(vec![model_with_constraint(
            "model.shop.orders",
            "primary_key",
            "id",
        )]);
        let findings = run(&manifest, "model.shop.orders");
        assert!(
            !findings
                .iter()
                .any(|f| f.check == HeuristicId::EnforcementConstraintUnbacked),
        );
    }

    #[test]
    fn enforcement_silent_for_a_model_with_no_declared_constraint() {
        let manifest =
            manifest_on_adapter("duckdb", vec![model_with_config("model.shop.orders", &[])]);
        let findings = run(&manifest, "model.shop.orders");
        assert!(
            !findings
                .iter()
                .any(|f| f.check == HeuristicId::EnforcementConstraintUnbacked),
        );
    }

    #[test]
    fn enforcement_fires_for_a_column_level_unique_constraint() {
        // A COLUMN-level unique constraint (rides ColumnFacts) —
        // exercises column_level_unique_columns.
        use crate::domain::manifest::{ColumnFacts, Constraint};
        let unique = Constraint::new("unique", Vec::new(), None, None, None, Vec::new());
        let mut column_facts = BTreeMap::new();
        column_facts.insert(
            "email".to_owned(),
            ColumnFacts::new(None, Vec::new(), Vec::new(), vec![unique]),
        );
        let model = model_with_config("model.shop.users", &[]).with_column_facts(column_facts);
        let manifest = manifest_on_adapter("snowflake", vec![model]);
        let findings = run(&manifest, "model.shop.users");
        let enforcement: Vec<_> = findings
            .iter()
            .filter(|f| f.check == HeuristicId::EnforcementConstraintUnbacked)
            .collect();
        assert_eq!(enforcement.len(), 1);
        assert_eq!(enforcement[0].construct, "constraint.unique[email]");
        assert_eq!(enforcement[0].verdict, Verdict::Uncovered);
    }

    #[test]
    fn enforcement_skips_a_check_constraint() {
        // A check constraint is out of the unique/not_null inference.
        use crate::domain::manifest::Constraint;
        let check = Constraint::new(
            "check",
            vec!["amount".to_owned()],
            Some("amount > 0".to_owned()),
            None,
            None,
            Vec::new(),
        );
        let model = model_with_config("model.shop.orders", &[]).with_contract_facts(
            vec![check],
            Vec::new(),
            None,
        );
        let manifest = manifest_on_adapter("duckdb", vec![model]);
        let findings = run(&manifest, "model.shop.orders");
        assert!(
            !findings
                .iter()
                .any(|f| f.check == HeuristicId::EnforcementConstraintUnbacked),
        );
    }
}
