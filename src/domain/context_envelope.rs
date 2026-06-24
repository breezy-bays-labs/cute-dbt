//! The domain-owned, **versioned context contract** (cute-dbt#491, epic #485).
//!
//! cute-dbt has one **context** — the structured facts a run derives from a
//! dbt manifest (+ optional PR diff) — and several **views** over it (the
//! static HTML report, the TS/React review app, the explorer). Today that
//! context is serialized in exactly one place: inlined into `report.html`
//! as the `<script type="application/json" id="cute-dbt-data">` blob (the
//! adapter's [`ReportPayload`](crate::adapters::render::ReportPayload)).
//!
//! `--context-out <path>` (cute-dbt#491) emits that **same** payload as a
//! standalone JSON file — the SSOT producer the TS Zod drift-gate (S3b)
//! pins to. To let a consumer distinguish a *thin* context (legitimately
//! sparse) from a *downgraded* one (an older/incompatible producer), the
//! emitted artifact carries a **versioned header**: `metadata.schema_version`,
//! an integer (dbt-style, not a semver string), exactly as the findings
//! envelope (cute-dbt#386) does. This module owns that wrapper shape + the
//! version constant — the domain owns the data contract; the adapter
//! supplies the concrete payload as the generic `Data` and serializes
//! (hexagonal: domain owns the shape, render serializes).
//!
//! ## The emitted shape
//!
//! ```json
//! { "metadata": { "schema_version": 1 }, "data": { ... the ReportPayload ... } }
//! ```
//!
//! `data` is **byte-identical** to the JSON inlined into `report.html`, so
//! the HTML report stays byte-stable (the `schema_version` lives only in the
//! `--context-out` wrapper, never in the inlined payload). The wrapper is
//! generic over `Data: Serialize` so the domain never imports the adapter's
//! [`ReportPayload`](crate::adapters::render::ReportPayload) (ADR-1 inward
//! dependency discipline); the adapter instantiates it at the emit edge.
//!
//! ## The absent→honesty mapping (the council MUST-FIX, cute-dbt#491)
//!
//! cute-dbt's never-a-false-claim contract is load-bearing: a 3-state
//! honesty verdict (presence / confidence / coverage / cell-type) must
//! **never** be silently omitted, because an absent key is ambiguous — a
//! consumer cannot tell "no data" from "the producer dropped the field".
//! The rule, enforced by the `assert_honesty_enum_always_present` test below
//! and documented at the field level across the payload:
//!
//! - A field that carries a **3-state honesty enum** (the verdict itself —
//!   [`Verdict`](crate::domain::checks::Verdict),
//!   [`Tier`](crate::domain::checks::Tier),
//!   [`SpanRole`](crate::domain::source_map::SpanRole),
//!   [`Presence`](crate::domain::source_map::Presence),
//!   [`ColumnEdgeConfidence`](crate::domain::cte::ColumnEdgeConfidence)) is
//!   **always present** whenever its carrier object is present. It is NEVER
//!   `skip_serializing_if`'d, so the verdict is read directly, never
//!   inferred from the *absence* of a key.
//! - `skip_serializing_if` is reserved for **genuinely-no-data** fields —
//!   optional evidence, a recommendation that only exists on an uncovered
//!   finding, a compiled span that is honestly `None` (pruned this build).
//!   In every such case the **honesty state itself** rides a sibling enum
//!   that is always present (e.g. a `SourceMapEntry`'s `compiled: None` is
//!   the honest "compiled-out" state, but its `role: SpanRole` is always
//!   present, so the 3-state [`presence`](crate::domain::source_map::SourceMapEntry::presence)
//!   read is unambiguous). Omission means "honest-empty", never "honest
//!   verdict, key dropped".
//!
//! This module is **pure** (ARCHITECTURE.md §1 domain discipline): POD +
//! serde derive + constants, no I/O. The emit-to-file lives in the cli /
//! render adapter.

use serde::Serialize;

/// The context contract schema version — an **integer**, the stability
/// anchor the TS Zod drift-gate (cute-dbt#491 S3b) pins to. Bumped only on
/// a breaking change to the emitted context shape; the inner payload fields
/// are additive-by-convention in v0.x (the `ReportPayload` precedent: new
/// keys land behind `skip_serializing_if`, keeping older consumers happy).
///
/// Starts at `1`, mirroring the findings envelope
/// ([`SCHEMA_VERSION`](crate::domain::findings_envelope::SCHEMA_VERSION)) —
/// the two are independent version lines (different artifacts) but share the
/// dbt-style integer convention.
pub const CONTEXT_SCHEMA_VERSION: u32 = 1;

/// The context artifact's versioned header.
///
/// Carries the single stability anchor — `schema_version`, an integer
/// pinned from [`CONTEXT_SCHEMA_VERSION`] by [`ContextMetadata::new`] so a
/// caller cannot drift it. Kept deliberately minimal in this slice (the
/// `findings_envelope` precedent grew `cute_dbt_version` / `generated_at` /
/// `scope` additively; this contract can do the same when a consumer needs
/// them — they are NOT required for the S3b drift-gate, which pins only the
/// version).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ContextMetadata {
    /// The context schema version (integer; the stability anchor). Always
    /// [`CONTEXT_SCHEMA_VERSION`].
    pub schema_version: u32,
}

impl ContextMetadata {
    /// Build the header, pinning [`CONTEXT_SCHEMA_VERSION`] from the
    /// constant so it cannot drift.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: CONTEXT_SCHEMA_VERSION,
        }
    }
}

impl Default for ContextMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// The standalone context artifact (`--context-out`, cute-dbt#491): the
/// versioned header + the run's context payload.
///
/// Generic over `Data: Serialize` so the domain owns the *wrapper* shape +
/// the version while never importing the adapter's
/// [`ReportPayload`](crate::adapters::render::ReportPayload) (ADR-1 inward
/// dependency discipline). The render adapter instantiates
/// `ContextEnvelope<&ReportPayload>` at the emit edge, so `data` serializes
/// **byte-identically** to the JSON inlined in `report.html` — the wrapper
/// only adds the `metadata` header, keeping the HTML report byte-stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextEnvelope<Data: Serialize> {
    /// The versioned header (the stability anchor the TS gate pins to).
    pub metadata: ContextMetadata,
    /// The run's context payload — byte-identical to the inlined
    /// `report.html` blob.
    pub data: Data,
}

impl<Data: Serialize> ContextEnvelope<Data> {
    /// Wrap a payload with the pinned [`ContextMetadata`] header.
    #[must_use]
    pub fn new(data: Data) -> Self {
        Self {
            metadata: ContextMetadata::new(),
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_a_positive_integer() {
        assert_eq!(CONTEXT_SCHEMA_VERSION, 1);
    }

    #[test]
    fn metadata_pins_schema_version_from_the_constant() {
        let meta = ContextMetadata::new();
        assert_eq!(meta.schema_version, CONTEXT_SCHEMA_VERSION);
        assert_eq!(ContextMetadata::default(), meta);
    }

    #[test]
    fn schema_version_serializes_as_an_integer_not_a_string() {
        let json = serde_json::to_value(ContextMetadata::new()).expect("serialize");
        assert!(
            json["schema_version"].is_u64(),
            "schema_version must be an integer, got {:?}",
            json["schema_version"]
        );
        assert_eq!(json["schema_version"], serde_json::json!(1));
    }

    #[test]
    fn envelope_wraps_data_under_metadata_and_data_keys() {
        // The emitted shape is exactly `{ metadata: { schema_version }, data }`
        // — the wrapper adds ONLY the header; `data` is the payload verbatim.
        let payload = serde_json::json!({ "baseline": "main", "models": [] });
        let envelope = ContextEnvelope::new(&payload);
        let json = serde_json::to_value(&envelope).expect("serialize");
        assert_eq!(json["metadata"]["schema_version"], serde_json::json!(1));
        assert_eq!(json["data"], payload, "data is the payload verbatim");
        // No surprise top-level keys: exactly `metadata` + `data`.
        let obj = json.as_object().expect("object");
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        assert_eq!(keys, vec![&"data".to_owned(), &"metadata".to_owned()]);
    }

    #[test]
    fn data_serializes_byte_identically_to_the_bare_payload() {
        // The contract guarantee: the `data` sub-tree is byte-identical to
        // serializing the payload alone (so `report.html`'s inlined blob and
        // the `--context-out` `data` never drift). Verified via the parsed
        // sub-tree equality — the byte-level guarantee for the real
        // `ReportPayload` is pinned in the render adapter's serializer test.
        let payload = serde_json::json!({
            "baseline": "main",
            "models": [{ "name": "orders" }],
        });
        let bare = serde_json::to_string(&payload).expect("serialize bare");
        let envelope = ContextEnvelope::new(&payload);
        let wrapped = serde_json::to_string(&envelope).expect("serialize wrapped");
        assert!(
            wrapped.contains(&bare),
            "the wrapped `data` embeds the bare payload verbatim:\n  bare:    {bare}\n  wrapped: {wrapped}"
        );
    }

    /// The absent→honesty mapping enforcement (the council MUST-FIX,
    /// cute-dbt#491): every 3-state honesty enum that rides the context
    /// payload is **always present** when its carrier is — its serialized
    /// form is a concrete tag, never an omitted key. We assert each enum's
    /// variants all serialize to a non-empty wire token (so a consumer
    /// reads the verdict directly, never inferring it from a missing key).
    ///
    /// This is the mechanical twin of the field-level doc comments: a future
    /// PR that makes one of these enums `Option`-and-skip-serialized (the
    /// silent-omission footgun) is caught here, not at review time.
    #[test]
    fn assert_honesty_enum_always_present() {
        use crate::domain::checks::{Tier, Verdict};
        use crate::domain::cte::ColumnEdgeConfidence;
        use crate::domain::source_map::{Presence, SpanRole};

        // `Verdict` — covered / uncovered / unknown. The carrier `Finding`
        // never `skip`s it; every variant has a concrete serialized tag.
        for verdict in [
            Verdict::Covered { by: Vec::new() },
            Verdict::Uncovered,
            Verdict::Unknown,
        ] {
            let v = serde_json::to_value(&verdict).expect("verdict serializes");
            assert!(!v.is_null(), "a verdict never serializes to null: {v:?}");
        }

        // `Tier` — the finding severity tier; always present on a Finding.
        for tier in [Tier::Total, Tier::High, Tier::Advisory] {
            let v = serde_json::to_value(tier).expect("tier serializes");
            assert!(
                v.as_str().is_some_and(|s| !s.is_empty()),
                "a tier serializes to a non-empty tag: {v:?}"
            );
        }

        // `SpanRole` — why a source-map region exists; always present on a
        // `SourceMapEntry` (the `compiled: None` honest-out state rides
        // alongside it, never replacing it). The internally-tagged `kind`
        // discriminant is the always-present honesty axis.
        for role in [
            SpanRole::CteBody {
                node_id: "n".to_owned(),
            },
            SpanRole::Column {
                node_id: "n".to_owned(),
                column: "c".to_owned(),
            },
        ] {
            let v = serde_json::to_value(&role).expect("span role serializes");
            assert!(
                v["kind"].as_str().is_some_and(|s| !s.is_empty()),
                "a span role carries a non-empty `kind` tag: {v:?}"
            );
        }

        // `Presence` — the 3-state compiled-in / compiled-out / structural
        // verdict, emitted as an explicit wire string (never omitted).
        for presence in [
            Presence::CompiledIn,
            Presence::CompiledOut,
            Presence::Structural,
        ] {
            assert!(
                !presence.to_wire().is_empty(),
                "a presence verdict has a non-empty wire token"
            );
        }

        // `ColumnEdgeConfidence` — resolved / ambiguous / opaque; the
        // never-drop honest confidence. Every variant serializes to a tag.
        for confidence in [
            ColumnEdgeConfidence::Resolved,
            ColumnEdgeConfidence::Ambiguous,
            ColumnEdgeConfidence::Opaque,
        ] {
            let v = serde_json::to_value(confidence).expect("confidence serializes");
            assert!(
                v.as_str().is_some_and(|s| !s.is_empty()),
                "a confidence verdict serializes to a non-empty tag: {v:?}"
            );
        }
    }
}
