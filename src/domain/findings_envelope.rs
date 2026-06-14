//! The machine-readable **findings envelope** (cute-dbt#386, epic #261).
//!
//! A thin `metadata.schema_version` header wrapped around the already-
//! serializing [`Finding`] POD — emitted as a **sidecar** JSON file
//! beside the HTML report (`--findings-out <path>`), never a `--format
//! json` swap. This is the sanctioned reversal of `ARCHITECTURE.md`
//! conscious-simplification row-5 ("no JSON wire envelope"): the founder's
//! ADR-4 amendment (2026-06-11) + the #261 locked decisions make the
//! additive sidecar land while the HTML output stays byte-identical.
//!
//! ## The four locked decisions ([#261](https://github.com/breezy-bays-labs/cute-dbt/issues/261))
//!
//! - **D1 — sidecar delivery.** Additive emit beside the HTML report in
//!   one run; mirrors dbt (`manifest`/`run_results` are sidecars).
//! - **D2 — check-id instability.** Check-ids churn freely in v0.x and
//!   freeze at v1.0. The envelope carries a machine-readable
//!   [`ID_STABILITY`] notice so consumers pin [`SCHEMA_VERSION`] — the
//!   *only* stability anchor pre-v1.0 — not individual check-ids.
//! - **D3 — gate semantics.** [`has_total_uncovered`] is true iff ≥1
//!   [`Tier::Total`] + [`Verdict::Uncovered`] finding exists. Total-only,
//!   not configurable — `Total` is deterministic (zero false positives),
//!   so gating on it never breaks a build on a heuristic guess.
//! - **D4 — `OpenLineage`.** The fields are shaped to map cleanly onto a
//!   future `OpenLineage` facet projection, but **no** OL output is emitted
//!   in this slice.
//!
//! `severity` is the existing [`Tier`] enum (no new field). `schema_version`
//! is an **integer** starting at `1` (dbt-style, not a semver string).
//! Owner fields are reserved for #256 (populated later, additively) — this
//! slice does **not** carry them.
//!
//! ## Layering
//!
//! This module is **pure** ([`ARCHITECTURE.md` §1] domain discipline):
//! POD + serde derive + the gate predicate, no I/O. `generated_at` is
//! threaded in as a parameter computed at the CLI I/O boundary
//! (golden-determinism rule) so the envelope golden is byte-stable and the
//! domain stays a pure function of `(facts, generated_at)`. The
//! *emit-to-file* lives in the [`findings_emit`](crate::adapters) adapter.
//!
//! [`Finding`]: crate::domain::checks::Finding
//! [`Tier::Total`]: crate::domain::checks::Tier::Total
//! [`Verdict::Uncovered`]: crate::domain::checks::Verdict::Uncovered
//! [`ARCHITECTURE.md` §1]: https://github.com/breezy-bays-labs/cute-dbt/blob/main/ARCHITECTURE.md

use serde::Serialize;

use crate::domain::checks::{Finding, HeuristicId, Tier};
use crate::domain::finding_anchor::{AnchorSide, ResolvedAnchor};

/// The envelope schema version — an **integer**, the only stability anchor
/// pre-v1.0 (decision D2). Bumped only on a breaking envelope-shape change;
/// the inner check-ids are explicitly unstable in v0.x.
pub const SCHEMA_VERSION: u32 = 1;

/// The machine-readable check-id-instability notice (decision D2). Consumers
/// pin [`SCHEMA_VERSION`], not individual check-ids, until v1.0.
pub const ID_STABILITY: &str = "unstable-v0.x";

/// Which scope source produced the in-scope set this envelope reports
/// over — the machine-readable twin of the report's diff-scope banner.
///
/// Serializes with an internal `mode` tag (`"pr-diff"` | `"baseline"`) and
/// the arm-specific source field, mirroring the existing scope plumbing
/// (the `--baseline-manifest` path verbatim; the `--pr-diff` source label).
/// A field is omitted when absent so the shape stays minimal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum EnvelopeScope {
    /// `--baseline-manifest` arm — diff against a baseline manifest.
    Baseline {
        /// The baseline manifest path the run diffed against (the
        /// `--baseline-manifest` value verbatim). Omitted when empty.
        #[serde(skip_serializing_if = "str::is_empty")]
        baseline: String,
    },
    /// `--pr-diff` arm — scope from a unified-diff patch.
    #[serde(rename = "pr-diff")]
    PrDiff {
        /// The PR-diff source label (the `--pr-diff @file` argument, e.g.
        /// `@diff.patch`). Omitted when absent.
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
}

/// The envelope header (decision-locked shape, #261).
///
/// `schema_version` + `id_stability` are constants pinned from
/// [`SCHEMA_VERSION`] / [`ID_STABILITY`] by [`EnvelopeMetadata::new`] so a
/// caller cannot drift them. `cute_dbt_version` is the crate version;
/// `generated_at` is threaded from the CLI I/O boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvelopeMetadata {
    /// The envelope schema version (integer; the only pre-v1.0 stability
    /// anchor). Always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The producing `cute-dbt` crate version (e.g. `"0.1.0"`).
    pub cute_dbt_version: String,
    /// Generation timestamp, RFC3339 (computed at the CLI I/O boundary so
    /// the golden is deterministic).
    pub generated_at: String,
    /// Which scope source produced the in-scope finding set.
    pub scope: EnvelopeScope,
    /// The machine-readable check-id-instability notice (D2). Always
    /// [`ID_STABILITY`] in v0.x.
    pub id_stability: String,
}

impl EnvelopeMetadata {
    /// Build the metadata header, pinning [`SCHEMA_VERSION`] /
    /// [`ID_STABILITY`] from the constants so they cannot drift.
    ///
    /// `cute_dbt_version` is typically `env!("CARGO_PKG_VERSION")` and
    /// `generated_at` the RFC3339 timestamp computed at the CLI I/O
    /// boundary — both supplied by the caller so the domain stays a pure
    /// function of its inputs.
    #[must_use]
    pub fn new(
        cute_dbt_version: impl Into<String>,
        generated_at: impl Into<String>,
        scope: EnvelopeScope,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            cute_dbt_version: cute_dbt_version.into(),
            generated_at: generated_at.into(),
            scope,
            id_stability: ID_STABILITY.to_owned(),
        }
    }
}

/// Which side of a diff a finding's anchor sits on (cute-dbt#386 — the
/// reserved finding→line projection slot).
///
/// Reserved for the follow-on resolver that maps a finding onto a concrete
/// `(path, line)` in a PR diff; this slice emits **no** value (every
/// envelope `anchor` is `None`). serde `snake_case` (`added` / `removed` /
/// `modified`) so a future projection (GitHub annotations, #353 PR
/// comments, SARIF) keys on a stable wire token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffContext {
    /// The anchored line is a `+` addition.
    Added,
    /// The anchored line is a `-` removal.
    Removed,
    /// The anchored construct spans a modified region.
    Modified,
}

impl From<AnchorSide> for DiffContext {
    /// Project the resolver's [`AnchorSide`] onto the envelope's wire
    /// [`DiffContext`] (cute-dbt#393): the two enums share the same three
    /// `added`/`removed`/`modified` tokens by design — the resolver computes
    /// the value, the envelope serializes it. Keeping them distinct types
    /// keeps the domain resolver independent of the wire DTO while this
    /// single conversion is the one place the projection happens.
    fn from(side: AnchorSide) -> Self {
        match side {
            AnchorSide::Added => Self::Added,
            AnchorSide::Removed => Self::Removed,
            AnchorSide::Modified => Self::Modified,
        }
    }
}

/// The reserved **finding→line projection anchor** (cute-dbt#386).
///
/// The envelope is the canonical agent channel, and downstream projections
/// — GitHub annotations, #353 PR comments, SARIF — all need a finding's
/// concrete `(path, line)`. This POD **reserves that slot now**; a follow-on
/// resolver populates it. In this slice every field is `None`/reserved — no
/// resolver logic, no value emitted — so an `EnvelopeFinding` with
/// `anchor: None` serializes byte-identically to a bare [`Finding`] (the
/// committed envelope golden is unchanged). Pure data, no methods beyond the
/// constructor.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct FindingAnchor {
    /// The project-relative source path the finding anchors to. Omitted from
    /// JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The 1-based line within `path`. Omitted from JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Which diff side the line sits on (PR-diff projections). Omitted from
    /// JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_context: Option<DiffContext>,
    /// A content-addressed hash stabilizing the anchor across reformatting
    /// (the resolver's drift guard). Omitted from JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_hash: Option<String>,
}

impl From<ResolvedAnchor> for FindingAnchor {
    /// Populate the envelope's reserved anchor slot from the resolver's
    /// fully-resolved [`ResolvedAnchor`] (cute-dbt#393, the #261 arc): map
    /// `path` / `line` / `diff_context` into the wire DTO's `Option` fields.
    ///
    /// `line` is `usize` on the resolver and `u32` on the wire — a source
    /// line that overflows `u32` (≈4 billion lines) is not a real dbt file,
    /// so the lossless [`u32::try_from`] degrades it to `None` rather than
    /// wrapping. `anchor_hash` stays `None`: the resolver primitive does not
    /// yet compute a content-addressed drift hash (a future additive slice),
    /// and an absent hash is omitted from JSON.
    fn from(resolved: ResolvedAnchor) -> Self {
        Self {
            path: Some(resolved.path),
            line: u32::try_from(resolved.line).ok(),
            diff_context: Some(resolved.diff_context.into()),
            anchor_hash: None,
        }
    }
}

/// One envelope finding: the existing [`Finding`] wire shape plus the
/// reserved [`FindingAnchor`] slot (cute-dbt#386).
///
/// The `Finding` is `#[serde(flatten)]`ed, so its keys sit directly on the
/// finding object exactly as before; `anchor` is `skip_serializing_if =
/// "Option::is_none"`, so an unresolved anchor (every anchor in this slice)
/// emits **zero** added bytes — the committed envelope golden stays
/// byte-identical. A populated anchor adds a single nested `anchor` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvelopeFinding {
    /// The finding itself — flattened so its keys (`check` / `tier` /
    /// `model_id` / `verdict` / …) sit on this object directly.
    #[serde(flatten)]
    pub finding: Finding<HeuristicId>,
    /// The reserved finding→line projection anchor. `None` (omitted) in this
    /// slice; populated by a follow-on resolver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<FindingAnchor>,
}

impl EnvelopeFinding {
    /// Wrap a [`Finding`] with no anchor (the v0.1 emit — the slot is
    /// reserved but unpopulated).
    #[must_use]
    pub fn new(finding: Finding<HeuristicId>) -> Self {
        Self {
            finding,
            anchor: None,
        }
    }
}

/// The findings envelope: a `metadata` header + a flat `findings` list.
///
/// Each [`EnvelopeFinding`] flattens the existing [`Finding`] (which already
/// carries `model_id` / `check` / `tier` / `verdict` / `evidence` /
/// `recommendation` / `degraded` / `suppressed`) and adds the reserved
/// `anchor` slot — so consumers group by `model_id` / `check` / `tier`
/// themselves; the envelope adds the versioned header and the (currently
/// unpopulated) anchor slot, nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingsEnvelope {
    /// The versioned header (decision D2 stability anchor lives here).
    pub metadata: EnvelopeMetadata,
    /// The flat in-scope finding set, in the order the run loop collected
    /// it (deterministic over the in-scope model set + each model's
    /// `model_findings` output). Each entry flattens a [`Finding`] + the
    /// reserved anchor slot.
    pub findings: Vec<EnvelopeFinding>,
}

impl FindingsEnvelope {
    /// Assemble an envelope from a header + the already-computed findings,
    /// wrapping each [`Finding`] in an anchor-less [`EnvelopeFinding`].
    #[must_use]
    pub fn new(metadata: EnvelopeMetadata, findings: Vec<Finding<HeuristicId>>) -> Self {
        Self {
            metadata,
            findings: findings.into_iter().map(EnvelopeFinding::new).collect(),
        }
    }
}

/// The `--fail-on-uncovered` gate predicate (decision D3).
///
/// `true` iff **any** finding is a [`Tier::Total`] **surfaced** uncovered
/// gap — a deterministic coverage gap on a zero-false-positive check that
/// the operator has **not** suppressed. Not configurable: `Total`-only is
/// the design tenet, so the gate never trips on a `High`/`Advisory`
/// heuristic guess and never on a `Covered`/`Unknown` verdict.
///
/// A **suppressed** Total-tier uncovered finding does **NOT** trip the gate
/// (cute-dbt#406): if the operator suppressed it (`[[checks.suppress]]` /
/// `-- cute-dbt: ignore(...)`), they have acknowledged the gap and do not
/// want it to fail the build. This predicate shares the exact
/// [`Finding::is_surfaced_uncovered`](crate::domain::checks::Finding::is_surfaced_uncovered)
/// suppression check the GitHub annotation emit (`build_annotations`) uses,
/// so the gate and the emit can never disagree: a suppressed-only Total gap
/// trips no gate AND emits no `::error` annotation, preserving the "gate
/// tripped ⇒ at least one error annotation" intuition.
#[must_use]
pub fn has_total_uncovered(findings: &[Finding<HeuristicId>]) -> bool {
    findings
        .iter()
        .any(|f| f.tier == Tier::Total && f.is_surfaced_uncovered())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::checks::{HeuristicId, Verdict};
    use crate::domain::manifest::NodeId;

    // A minimal Finding builder over a chosen production-registry check.
    // `Finding::new` denormalizes tier/instrument from the check's spec, so
    // `total_finding` (the `grain.unique-key-unbacked` Total-tier check at
    // index 0) and `high_finding` (the `union.arm-coverage` High-tier check
    // at index 1) let us exercise the tier-sensitive gate predicate.
    fn finding_with(check: HeuristicId, verdict: Verdict) -> Finding<HeuristicId> {
        Finding::new(
            check,
            NodeId::new("model.shop.dim_x"),
            "config.unique_key",
            verdict,
            Vec::new(),
        )
    }

    fn finding(verdict: Verdict) -> Finding<HeuristicId> {
        finding_with(HeuristicId::GrainUniqueKeyUnbacked, verdict)
    }

    // ---- serde round-trip -------------------------------------------

    #[test]
    fn metadata_pins_schema_version_and_id_stability_from_constants() {
        let meta = EnvelopeMetadata::new(
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::PrDiff {
                source: Some("@diff.patch".to_owned()),
            },
        );
        assert_eq!(meta.schema_version, SCHEMA_VERSION);
        assert_eq!(meta.schema_version, 1);
        assert_eq!(meta.id_stability, ID_STABILITY);
        assert_eq!(meta.id_stability, "unstable-v0.x");
    }

    #[test]
    fn envelope_serializes_with_metadata_header_and_flat_findings() {
        let envelope = FindingsEnvelope::new(
            EnvelopeMetadata::new(
                "0.1.0",
                "2026-01-15",
                EnvelopeScope::Baseline {
                    baseline: "baseline.json".to_owned(),
                },
            ),
            vec![finding(Verdict::Uncovered)],
        );
        let json = serde_json::to_value(&envelope).expect("serializes");
        assert_eq!(json["metadata"]["schema_version"], 1);
        assert_eq!(json["metadata"]["cute_dbt_version"], "0.1.0");
        assert_eq!(json["metadata"]["generated_at"], "2026-01-15");
        assert_eq!(json["metadata"]["id_stability"], "unstable-v0.x");
        assert_eq!(json["metadata"]["scope"]["mode"], "baseline");
        assert_eq!(json["metadata"]["scope"]["baseline"], "baseline.json");
        assert!(json["findings"].is_array());
        assert_eq!(json["findings"].as_array().unwrap().len(), 1);
        // The wrapped Finding keeps its existing serialized shape — the
        // anchor wrapper flattens transparently.
        assert_eq!(json["findings"][0]["model_id"], "model.shop.dim_x");
        assert_eq!(json["findings"][0]["tier"], "total");
        assert_eq!(json["findings"][0]["verdict"]["status"], "uncovered");
        // cute-dbt#386 byte-safety: an unpopulated anchor emits NO `anchor`
        // key, so the envelope stays byte-identical to the pre-anchor shape.
        assert!(
            json["findings"][0].get("anchor").is_none(),
            "an unresolved anchor must be omitted entirely: {}",
            json["findings"][0]
        );
    }

    #[test]
    fn schema_version_is_an_integer_not_a_string() {
        let meta = EnvelopeMetadata::new(
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::Baseline {
                baseline: String::new(),
            },
        );
        let json = serde_json::to_value(&meta).expect("serializes");
        assert!(
            json["schema_version"].is_u64(),
            "schema_version must be an integer, got {:?}",
            json["schema_version"]
        );
    }

    // ---- scope arms --------------------------------------------------

    #[test]
    fn pr_diff_scope_tags_mode_and_carries_source() {
        let json = serde_json::to_value(EnvelopeScope::PrDiff {
            source: Some("@pr.patch".to_owned()),
        })
        .expect("serializes");
        assert_eq!(json["mode"], "pr-diff");
        assert_eq!(json["source"], "@pr.patch");
    }

    #[test]
    fn pr_diff_scope_omits_source_when_absent() {
        let json =
            serde_json::to_value(EnvelopeScope::PrDiff { source: None }).expect("serializes");
        assert_eq!(json["mode"], "pr-diff");
        assert!(
            json.get("source").is_none(),
            "absent source must be omitted: {json:?}"
        );
    }

    #[test]
    fn baseline_scope_omits_empty_label() {
        let json = serde_json::to_value(EnvelopeScope::Baseline {
            baseline: String::new(),
        })
        .expect("serializes");
        assert_eq!(json["mode"], "baseline");
        assert!(
            json.get("baseline").is_none(),
            "empty baseline label must be omitted: {json:?}"
        );
    }

    // ---- gate predicate (D3) ----------------------------------------

    #[test]
    fn gate_trips_on_a_total_uncovered_finding() {
        assert!(has_total_uncovered(&[finding(Verdict::Uncovered)]));
    }

    #[test]
    fn gate_does_not_trip_on_a_total_covered_finding() {
        assert!(!has_total_uncovered(&[finding(Verdict::Covered {
            by: vec!["test.shop.t".to_owned()]
        })]));
    }

    #[test]
    fn gate_does_not_trip_on_a_total_unknown_finding() {
        assert!(!has_total_uncovered(&[finding(Verdict::Unknown)]));
    }

    #[test]
    fn gate_does_not_trip_on_an_empty_finding_set() {
        assert!(!has_total_uncovered(&[]));
    }

    #[test]
    fn gate_does_not_trip_on_a_high_tier_uncovered_finding() {
        // `union.arm-coverage` is a High-tier check — an uncovered verdict
        // on it is a heuristic cue, never a gate trip (D3 Total-only tenet).
        let high = finding_with(HeuristicId::UnionArmCoverage, Verdict::Uncovered);
        assert_eq!(high.tier, Tier::High);
        assert!(!has_total_uncovered(&[high]));
    }

    #[test]
    fn gate_trips_when_any_total_uncovered_is_present_among_others() {
        let findings = vec![
            finding(Verdict::Covered {
                by: vec!["test.shop.t".to_owned()],
            }),
            finding(Verdict::Unknown),
            finding(Verdict::Uncovered),
        ];
        assert!(has_total_uncovered(&findings));
    }

    // ---- suppression-aware gate (cute-dbt#406) ----------------------

    // Mark a finding as operator-suppressed (the cute-dbt#171 display-layer
    // acknowledgement `apply_check_policy` attaches).
    fn suppressed(mut finding: Finding<HeuristicId>) -> Finding<HeuristicId> {
        finding.suppressed = Some(crate::domain::checks::Suppression {
            source: crate::domain::checks::SuppressionSource::Config,
            reason: Some("accepted during backfill".to_owned()),
        });
        finding
    }

    #[test]
    fn gate_does_not_trip_on_a_suppressed_total_uncovered_finding() {
        // cute-dbt#406: a Total-tier uncovered finding the operator
        // suppressed does NOT trip `--fail-on-uncovered`. If you suppressed
        // it, you do not want it to fail the build — and this is the exact
        // suppression check `build_annotations` uses to filter the
        // annotation emit, so the gate and the emit can never disagree.
        let only_gap = suppressed(finding(Verdict::Uncovered));
        assert_eq!(only_gap.tier, Tier::Total);
        assert!(only_gap.suppressed.is_some());
        assert!(
            !has_total_uncovered(&[only_gap]),
            "a suppressed-only Total gap must not trip the gate"
        );
    }

    #[test]
    fn gate_trips_when_an_unsuppressed_total_gap_rides_beside_a_suppressed_one() {
        // A suppressed Total gap is exempt, but a sibling non-suppressed
        // Total gap still trips the gate — suppression exempts only the
        // finding it marks, never the whole run.
        let findings = vec![
            suppressed(finding(Verdict::Uncovered)),
            finding(Verdict::Uncovered),
        ];
        assert!(has_total_uncovered(&findings));
    }

    // ---- reserved anchor slot (cute-dbt#386) ------------------------

    #[test]
    fn an_anchor_less_envelope_finding_is_byte_identical_to_a_bare_finding() {
        // The byte-safety invariant: flattening the Finding + omitting the
        // None anchor reproduces the bare Finding's wire shape exactly, so
        // the committed envelope golden is unchanged by the anchor slot.
        let bare = finding(Verdict::Uncovered);
        let wrapped = EnvelopeFinding::new(bare.clone());
        let bare_json = serde_json::to_string(&bare).expect("serializes");
        let wrapped_json = serde_json::to_string(&wrapped).expect("serializes");
        assert_eq!(
            bare_json, wrapped_json,
            "an anchor-less EnvelopeFinding must serialize byte-identically \
             to the bare Finding"
        );
        // And explicitly: no `anchor` key surfaces.
        let value = serde_json::to_value(&wrapped).expect("serializes");
        assert!(
            value.get("anchor").is_none(),
            "None anchor must be omitted: {value}"
        );
    }

    #[test]
    fn a_populated_anchor_round_trips_and_nests_under_anchor() {
        let mut wrapped = EnvelopeFinding::new(finding(Verdict::Uncovered));
        wrapped.anchor = Some(FindingAnchor {
            path: Some("models/marts/dim_x.sql".to_owned()),
            line: Some(42),
            diff_context: Some(DiffContext::Added),
            anchor_hash: Some("abc123".to_owned()),
        });
        let value = serde_json::to_value(&wrapped).expect("serializes");
        // The finding keys still flatten onto the entry; the anchor nests.
        assert_eq!(value["model_id"], "model.shop.dim_x");
        assert_eq!(value["anchor"]["path"], "models/marts/dim_x.sql");
        assert_eq!(value["anchor"]["line"], 42);
        assert_eq!(value["anchor"]["diff_context"], "added");
        assert_eq!(value["anchor"]["anchor_hash"], "abc123");
    }

    #[test]
    fn a_default_anchor_omits_every_unset_field() {
        // The resolver may populate fields incrementally; an all-None anchor
        // (the Default) serializes to an empty object — every field is
        // skip_serializing_if None.
        let value = serde_json::to_value(FindingAnchor::default()).expect("serializes");
        assert_eq!(
            value,
            serde_json::json!({}),
            "empty anchor is {{}}: {value}"
        );
    }

    #[test]
    fn diff_context_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(DiffContext::Modified).expect("serializes"),
            serde_json::json!("modified")
        );
        assert_eq!(
            serde_json::to_value(DiffContext::Removed).expect("serializes"),
            serde_json::json!("removed")
        );
    }

    // ---- ResolvedAnchor → FindingAnchor projection (cute-dbt#393) ----

    #[test]
    fn anchor_side_maps_onto_the_matching_diff_context() {
        assert_eq!(DiffContext::from(AnchorSide::Added), DiffContext::Added);
        assert_eq!(DiffContext::from(AnchorSide::Removed), DiffContext::Removed);
        assert_eq!(
            DiffContext::from(AnchorSide::Modified),
            DiffContext::Modified
        );
    }

    #[test]
    fn resolved_anchor_populates_the_wire_finding_anchor() {
        let resolved = ResolvedAnchor {
            path: "models/marts/dim_x.sql".to_owned(),
            line: 42,
            diff_context: AnchorSide::Added,
        };
        let anchor = FindingAnchor::from(resolved);
        assert_eq!(anchor.path.as_deref(), Some("models/marts/dim_x.sql"));
        assert_eq!(anchor.line, Some(42));
        assert_eq!(anchor.diff_context, Some(DiffContext::Added));
        // The resolver does not yet compute a drift hash — the slot stays
        // absent (omitted from JSON).
        assert_eq!(anchor.anchor_hash, None);
    }

    #[test]
    fn projected_finding_anchor_serializes_under_the_anchor_key() {
        let mut wrapped = EnvelopeFinding::new(finding(Verdict::Uncovered));
        wrapped.anchor = Some(FindingAnchor::from(ResolvedAnchor {
            path: "models/orders.sql".to_owned(),
            line: 7,
            diff_context: AnchorSide::Modified,
        }));
        let value = serde_json::to_value(&wrapped).expect("serializes");
        assert_eq!(value["anchor"]["path"], "models/orders.sql");
        assert_eq!(value["anchor"]["line"], 7);
        assert_eq!(value["anchor"]["diff_context"], "modified");
        // No drift hash resolved ⇒ the key is omitted entirely.
        assert!(
            value["anchor"].get("anchor_hash").is_none(),
            "an unresolved anchor_hash must be omitted: {}",
            value["anchor"]
        );
    }

    #[test]
    fn envelope_new_wraps_each_finding_with_a_none_anchor() {
        let envelope = FindingsEnvelope::new(
            EnvelopeMetadata::new(
                "0.1.0",
                "2026-01-15",
                EnvelopeScope::PrDiff { source: None },
            ),
            vec![finding(Verdict::Uncovered), finding(Verdict::Unknown)],
        );
        assert_eq!(envelope.findings.len(), 2);
        assert!(
            envelope.findings.iter().all(|f| f.anchor.is_none()),
            "every wrapped finding starts anchor-less"
        );
    }
}
