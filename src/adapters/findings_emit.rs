//! Findings-envelope emit adapter (cute-dbt#386, epic #261).
//!
//! Collects the in-scope [`Finding`]s — mirroring the report's per-model
//! check pipeline exactly — wraps them in a versioned
//! [`FindingsEnvelope`], serializes the envelope to pretty JSON, and writes
//! it as a **sidecar** beside the HTML report (`--findings-out <path>`).
//!
//! This adapter is the *only* findings-collection caller outside the HTML
//! render path: it parses each in-scope model's CTE graph and runs
//! `model_findings → apply_check_policy` exactly as
//! [`crate::adapters::render`] does during payload assembly, so the
//! envelope's findings are byte-for-byte the same vocabulary the report
//! displays — without touching `report.html` or the render path
//! (`ARCHITECTURE.md` §1: this collection is an adapter concern; the POD +
//! gate predicate live in [`crate::domain::findings_envelope`]).
//!
//! `generated_at` is supplied by the caller (the CLI I/O boundary), keeping
//! this adapter a pure function of `(manifest, scope, policy, version,
//! generated_at)` so the committed envelope golden is byte-stable.

use std::fs;
use std::io;
use std::path::Path;

use crate::adapters::cte_engine::parse_cte_graph;
use crate::domain::{
    CheckPolicy, EnvelopeFinding, EnvelopeMetadata, EnvelopeScope, Finding, FindingAnchor,
    FindingsEnvelope, HeuristicId, Manifest, ModelInScopeSet, ResolvedAnchor, apply_check_policy,
    model_findings,
};

/// Collect the flat in-scope finding set the envelope reports over.
///
/// Mirrors [`crate::adapters::render`]'s per-model pipeline exactly: for
/// every model in `models_in_scope`, parse its `compiled_code` once into a
/// CTE graph and run `apply_check_policy(model_findings(.., Some(&graph)),
/// policy)`. The same parse-and-evaluate the renderer performs during
/// payload assembly — so the envelope and the HTML report carry the
/// identical findings (the same suppression/selection policy applied).
///
/// Models are visited in `models_in_scope` iteration order; within a model
/// the order is `model_findings`' deterministic output. An id that resolves
/// to no manifest node is skipped (the renderer's `continue` arm).
#[must_use]
pub fn collect_in_scope_findings(
    manifest: &Manifest,
    models_in_scope: &ModelInScopeSet,
    check_policy: &CheckPolicy<HeuristicId>,
) -> Vec<Finding<HeuristicId>> {
    let mut findings = Vec::new();
    for model_id in models_in_scope.iter() {
        let Some(model) = manifest.node(model_id) else {
            continue;
        };
        // The renderer's exact graph build: parse the compiled SQL once,
        // tolerating an empty/uncompiled body as an empty graph.
        let graph = parse_cte_graph(model.compiled_code().unwrap_or_default()).unwrap_or_default();
        findings.extend(apply_check_policy(
            model_findings(manifest, model, Some(&graph)),
            check_policy,
        ));
    }
    findings
}

/// Assemble the full [`FindingsEnvelope`] for the in-scope set.
///
/// `cute_dbt_version` is typically `env!("CARGO_PKG_VERSION")`;
/// `generated_at` the RFC3339 timestamp computed at the CLI I/O boundary;
/// `scope` the arm-specific [`EnvelopeScope`] built from the run's scope
/// source. The findings are collected via [`collect_in_scope_findings`].
#[must_use]
pub fn build_findings_envelope(
    manifest: &Manifest,
    models_in_scope: &ModelInScopeSet,
    check_policy: &CheckPolicy<HeuristicId>,
    cute_dbt_version: impl Into<String>,
    generated_at: impl Into<String>,
    scope: EnvelopeScope,
) -> FindingsEnvelope {
    let findings = collect_in_scope_findings(manifest, models_in_scope, check_policy);
    envelope_from_findings(findings, cute_dbt_version, generated_at, scope)
}

/// Assemble a [`FindingsEnvelope`] from an **already-collected** finding set.
///
/// The same builder as [`build_findings_envelope`] but over a finding `Vec`
/// the caller already holds — so the cli run loop can collect once, run the
/// `--fail-on-uncovered` gate on the raw `Finding`s
/// ([`has_total_uncovered`](crate::domain::has_total_uncovered) operates on
/// `&[Finding]`, not the anchor-wrapped envelope entries), and build the
/// envelope from the same `Vec` without a second collection pass.
#[must_use]
pub fn envelope_from_findings(
    findings: Vec<Finding<HeuristicId>>,
    cute_dbt_version: impl Into<String>,
    generated_at: impl Into<String>,
    scope: EnvelopeScope,
) -> FindingsEnvelope {
    let metadata = EnvelopeMetadata::new(cute_dbt_version, generated_at, scope);
    FindingsEnvelope::new(metadata, findings)
}

/// Assemble a [`FindingsEnvelope`] from an already-collected finding set,
/// **populating each finding's reserved anchor slot** via `anchor_for`
/// (cute-dbt#393 — completing the #261 one-resolver-two-projections arc).
///
/// The anchor-less twin of this is [`envelope_from_findings`]; this is the
/// same builder except each [`EnvelopeFinding`]'s `anchor` is filled from
/// the resolver output (the SAME
/// [`resolve_finding_anchor`](crate::domain::resolve_finding_anchor) the
/// `--annotations` emit consumes). A finding whose anchor does not resolve
/// (its model file is not in the diff, or the run is the baseline arm with
/// no diff index) keeps `anchor: None` — and an unpopulated anchor
/// serializes to **zero** added bytes (the `skip_serializing_if` on every
/// `FindingAnchor` field), so the committed envelope golden only grows the
/// `anchor` objects that actually resolved.
///
/// `anchor_for` is the resolver bound to the run's manifest + diff index
/// (the CLI passes a closure); a [`ResolvedAnchor`] maps into the wire
/// [`FindingAnchor`] via its `From` impl.
#[must_use]
pub fn envelope_from_findings_anchored(
    findings: Vec<Finding<HeuristicId>>,
    cute_dbt_version: impl Into<String>,
    generated_at: impl Into<String>,
    scope: EnvelopeScope,
    anchor_for: &impl Fn(&Finding<HeuristicId>) -> Option<ResolvedAnchor>,
) -> FindingsEnvelope {
    let metadata = EnvelopeMetadata::new(cute_dbt_version, generated_at, scope);
    let entries = findings
        .into_iter()
        .map(|finding| {
            let anchor = anchor_for(&finding).map(FindingAnchor::from);
            EnvelopeFinding { finding, anchor }
        })
        .collect();
    FindingsEnvelope {
        metadata,
        findings: entries,
    }
}

/// Serialize the envelope to pretty (2-space) JSON with a trailing newline.
///
/// Pretty-printed + newline-terminated so the committed golden is a
/// readable, diff-friendly artifact and the byte-identity gate is stable
/// (matching the POSIX text-file convention the other goldens follow).
/// `serde_json::to_string_pretty` is infallible for this POD (no map with
/// non-string keys, no non-finite floats), but the `Result` is surfaced
/// rather than unwrapped so a future field change cannot panic at runtime.
///
/// # Errors
///
/// Returns the underlying `serde_json` error if serialization fails (not
/// reachable for the current POD shape; surfaced for forward safety).
pub fn envelope_to_json(envelope: &FindingsEnvelope) -> Result<String, serde_json::Error> {
    let mut json = serde_json::to_string_pretty(envelope)?;
    json.push('\n');
    Ok(json)
}

/// Write the envelope JSON sidecar to `path`.
///
/// Additive to the HTML report — the run loop calls this *after* the HTML
/// is written, in the same invocation. Any parent directory must already
/// exist (the same contract as the `--out` HTML path); a write failure is
/// surfaced to the caller to map onto the run-loop's output-error path.
///
/// # Errors
///
/// Returns the I/O error if the file cannot be written, or the
/// serialization error if the envelope cannot be encoded.
pub fn write_sidecar(envelope: &FindingsEnvelope, path: &Path) -> io::Result<()> {
    let json = envelope_to_json(envelope)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::checks::CheckId;
    use crate::domain::{
        Checksum, DependsOn, InScopeSet, ManifestMetadata, Node, NodeConfig, NodeId,
        has_total_uncovered,
    };
    use std::collections::{BTreeMap, HashMap};

    fn checksum(value: &str) -> Checksum {
        Checksum::new("sha256", value)
    }

    // A model declaring config.unique_key with no backing uniqueness test —
    // the production registry's Total-tier `grain.unique-key-unbacked`
    // check fires UNCOVERED on it (the render layer's
    // `build_payload_carries_findings_for_an_unbacked_unique_key` fixture).
    fn unbacked_unique_key_model() -> Node {
        let mut config = BTreeMap::new();
        config.insert("unique_key".to_owned(), serde_json::json!("order_id"));
        Node::new(
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
        )
    }

    fn manifest_with(nodes: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[test]
    fn collect_mirrors_the_render_findings_for_an_in_scope_model() {
        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let findings = collect_in_scope_findings(&manifest, &models, &CheckPolicy::default());
        assert_eq!(findings.len(), 1, "the grain check fires once");
        assert_eq!(findings[0].check.as_str(), "grain.unique-key-unbacked");
        assert_eq!(findings[0].model_id.as_str(), "model.shop.orders_rollup");
        assert!(has_total_uncovered(&findings), "Total-tier uncovered gap");
    }

    #[test]
    fn collect_skips_an_unresolvable_model_id() {
        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        // A model id NOT in the manifest is skipped (the renderer's
        // `continue` arm) — no panic, no phantom finding.
        let models = ModelInScopeSet::from_iter([
            NodeId::new("model.shop.orders_rollup"),
            NodeId::new("model.shop.ghost"),
        ]);
        let findings = collect_in_scope_findings(&manifest, &models, &CheckPolicy::default());
        assert_eq!(findings.len(), 1, "only the resolvable model contributes");
    }

    #[test]
    fn collect_is_empty_when_no_models_are_in_scope() {
        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        let findings = collect_in_scope_findings(
            &manifest,
            &ModelInScopeSet::from_iter([]),
            &CheckPolicy::default(),
        );
        assert!(findings.is_empty());
        // Sanity: the InScopeSet import is exercised so the empty-scope
        // contract is explicit (no in-scope tests ⇒ no findings).
        assert!(InScopeSet::new().iter().next().is_none());
    }

    #[test]
    fn build_envelope_carries_metadata_and_collected_findings() {
        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let envelope = build_findings_envelope(
            &manifest,
            &models,
            &CheckPolicy::default(),
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::PrDiff {
                source: Some("@diff.patch".to_owned()),
            },
        );
        assert_eq!(envelope.metadata.schema_version, 1);
        assert_eq!(envelope.metadata.cute_dbt_version, "0.1.0");
        assert_eq!(envelope.metadata.generated_at, "2026-01-15");
        assert_eq!(envelope.findings.len(), 1);
    }

    #[test]
    fn json_is_pretty_printed_and_newline_terminated() {
        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let envelope = build_findings_envelope(
            &manifest,
            &models,
            &CheckPolicy::default(),
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::Baseline {
                baseline: "baseline.json".to_owned(),
            },
        );
        let json = envelope_to_json(&envelope).expect("serializes");
        assert!(json.ends_with("}\n"), "trailing newline: {json:?}");
        assert!(json.contains("  \"metadata\""), "2-space indent");
        // Round-trips back to an equal value (structural stability).
        let value: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(value["metadata"]["id_stability"], "unstable-v0.x");
        assert_eq!(value["metadata"]["scope"]["mode"], "baseline");
    }

    #[test]
    fn write_sidecar_writes_the_json_to_disk() {
        let dir = std::env::temp_dir().join(format!("cute-dbt-envelope-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("findings.json");
        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let envelope = build_findings_envelope(
            &manifest,
            &models,
            &CheckPolicy::default(),
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::PrDiff { source: None },
        );
        write_sidecar(&envelope, &path).expect("writes");
        let on_disk = fs::read_to_string(&path).expect("reads back");
        assert_eq!(on_disk, envelope_to_json(&envelope).expect("serializes"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- envelope anchor population (cute-dbt#393) -------------------

    #[test]
    fn anchored_builder_populates_each_finding_anchor_from_the_resolver() {
        use crate::domain::{AnchorSide, ResolvedAnchor};

        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let findings = collect_in_scope_findings(&manifest, &models, &CheckPolicy::default());
        // A resolver that always resolves to a fixed line.
        let resolver = |_: &Finding<HeuristicId>| {
            Some(ResolvedAnchor {
                path: "models/orders_rollup.sql".to_owned(),
                line: 9,
                diff_context: AnchorSide::Modified,
            })
        };
        let envelope = envelope_from_findings_anchored(
            findings,
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::PrDiff { source: None },
            &resolver,
        );
        assert_eq!(envelope.findings.len(), 1);
        let anchor = envelope.findings[0]
            .anchor
            .as_ref()
            .expect("anchor populated");
        assert_eq!(anchor.path.as_deref(), Some("models/orders_rollup.sql"));
        assert_eq!(anchor.line, Some(9));
        // The wire shape nests the resolved anchor under `anchor`.
        let value = serde_json::to_value(&envelope).expect("serializes");
        assert_eq!(value["findings"][0]["anchor"]["line"], 9);
        assert_eq!(value["findings"][0]["anchor"]["diff_context"], "modified");
    }

    #[test]
    fn anchored_builder_leaves_unresolved_findings_anchor_less_and_byte_minimal() {
        let manifest = manifest_with(vec![unbacked_unique_key_model()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let findings = collect_in_scope_findings(&manifest, &models, &CheckPolicy::default());
        // A resolver that resolves nothing (baseline arm / model not in diff).
        let none_resolver = |_: &Finding<HeuristicId>| None;
        let anchored = envelope_from_findings_anchored(
            findings.clone(),
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::PrDiff { source: None },
            &none_resolver,
        );
        // With no anchor resolved, the anchored builder produces the SAME
        // bytes as the anchor-less builder — the `anchor` key is omitted
        // entirely, so the committed golden only grows the anchors that
        // actually resolved.
        let plain = envelope_from_findings(
            findings,
            "0.1.0",
            "2026-01-15",
            EnvelopeScope::PrDiff { source: None },
        );
        assert!(anchored.findings.iter().all(|f| f.anchor.is_none()));
        assert_eq!(
            envelope_to_json(&anchored).expect("serializes"),
            envelope_to_json(&plain).expect("serializes"),
            "an all-None-anchor envelope is byte-identical to the anchor-less one"
        );
    }
}
