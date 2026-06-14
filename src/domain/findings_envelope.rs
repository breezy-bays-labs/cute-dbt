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

use crate::domain::checks::{Finding, HeuristicId, Tier, Verdict};

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

/// The findings envelope: a `metadata` header + a flat `findings` list.
///
/// Each [`Finding`] already carries `model_id` / `check` / `tier` /
/// `verdict` / `evidence` / `recommendation` / `degraded` / `suppressed`,
/// so consumers group by `model_id` / `check` / `tier` themselves — the
/// envelope adds no grouping, only the versioned header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingsEnvelope {
    /// The versioned header (decision D2 stability anchor lives here).
    pub metadata: EnvelopeMetadata,
    /// The flat in-scope finding set, in the order the run loop collected
    /// it (deterministic over the in-scope model set + each model's
    /// `model_findings` output).
    pub findings: Vec<Finding<HeuristicId>>,
}

impl FindingsEnvelope {
    /// Assemble an envelope from its already-computed parts.
    #[must_use]
    pub fn new(metadata: EnvelopeMetadata, findings: Vec<Finding<HeuristicId>>) -> Self {
        Self { metadata, findings }
    }
}

/// The `--fail-on-uncovered` gate predicate (decision D3).
///
/// `true` iff **any** finding is both [`Tier::Total`] and
/// [`Verdict::Uncovered`] — a deterministic coverage gap on a zero-false-
/// positive check. Not configurable: `Total`-only is the design tenet, so
/// the gate never trips on a `High`/`Advisory` heuristic guess and never
/// on a `Covered`/`Unknown` verdict.
///
/// A `suppressed` Total-tier uncovered finding still counts: suppression is
/// a *display* acknowledgement, not a coverage fact — the gap is real, the
/// operator merely chose not to surface it in the HTML. (If a future slice
/// wants suppression to exempt the gate, that is a deliberate decision, not
/// a silent default; this predicate keeps the honest "the gap exists" read.)
#[must_use]
pub fn has_total_uncovered(findings: &[Finding<HeuristicId>]) -> bool {
    findings
        .iter()
        .any(|f| f.tier == Tier::Total && f.verdict == Verdict::Uncovered)
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
        // The wrapped Finding keeps its existing serialized shape.
        assert_eq!(json["findings"][0]["model_id"], "model.shop.dim_x");
        assert_eq!(json["findings"][0]["tier"], "total");
        assert_eq!(json["findings"][0]["verdict"]["status"], "uncovered");
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
}
