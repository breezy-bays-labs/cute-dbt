//! The [`SeedCard`] POD — a seed's render payload (cute-dbt#350, epic #350).
//!
//! A dbt **seed** is a flat CSV the engine loads into a warehouse table.
//! In a manifest v12 node it is pure metadata — `resource_type:"seed"`, a
//! SHA-256 `checksum`, a `config` block, an empty `columns` map, an empty
//! `depends_on.nodes` (seeds are graph **roots**), and an
//! `original_file_path` of `seeds/<name>.csv`. The CSV **row data is not in
//! the manifest** — it lives only on disk in the working tree, exactly like
//! the cute-dbt#126 external unit-test fixture. The render path therefore
//! reuses the existing tabular machinery wholesale: the RFC 4180 parser
//! ([`external_fixture_table`](crate::domain::external_fixture_table) →
//! [`FixtureTable`]) and, on the pr-diff arm, the NULL-aware cell-diff
//! ([`reconstruct_external_fixture_diff`](crate::domain::reconstruct_external_fixture_diff)
//! → [`FixtureTableDiff`]).
//!
//! `SeedCard` is the POD carrier the CLI gather stage fills and the report
//! renderer consumes. This slice (the walking-skeleton domain core) defines
//! the POD plus the [`seeds_in_scope`](crate::domain::state::StateComparator::seeds_in_scope)
//! projection and the `feeds_models` derivation; the working-tree CSV read,
//! the report section, and the cell-diff wiring land in later slices. Until
//! then [`SeedCard::table`] and [`SeedCard::diff`] stay `None` and the
//! config-display strings stay `None` (the adapter fills them once the
//! real-fusion seed-config wire shape is pinned — cute-dbt#350 S2/S5).
//!
//! POD-only (ADR-2): owned data + a constructor, no method machinery beyond
//! what the run loop reads. `std` + `serde` derive only (the domain-purity
//! invariant) — the carried [`FixtureTable`] / [`FixtureTableDiff`] are
//! themselves domain PODs, so `SeedCard` introduces no new dependency.

use serde::{Deserialize, Serialize};

use crate::domain::cell_diff::FixtureTableDiff;
use crate::domain::manifest::NodeId;
use crate::domain::unit_test_table::FixtureTable;

/// One seed's render payload (cute-dbt#350).
///
/// Built per in-scope seed by
/// [`seeds_in_scope`](crate::domain::state::StateComparator::seeds_in_scope)
/// (identity + lineage), then enriched by the CLI gather stage (the
/// working-tree CSV → [`Self::table`]) and, on the pr-diff arm, the
/// cell-diff ([`Self::diff`]). A seed whose data cannot be read (no
/// `--project-root`, or the file is missing) keeps `table: None` and renders
/// a labeled empty-data state — a **truthful degrade**, never a silent blank
/// grid (the cute-dbt#126 lesson).
///
/// Additive POD (ADR-5): `Serialize`/`Deserialize` so the pre-composed
/// payload crosses to the renderer; new facts arrive as additive fields, not
/// a restructure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedCard {
    /// The seed's full manifest node id (e.g.
    /// `seed.jaffle_shop.raw_customers`).
    pub id: NodeId,
    /// The seed's authored bare name (e.g. `raw_customers`) — the handle a
    /// reviewer recognizes. From [`Node::bare_name`](crate::domain::Node::bare_name).
    pub name: String,
    /// The seed source file, project-relative (`seeds/<name>.csv`). Safe to
    /// inline — unlike the seed node's absolute `root_path`, this carries no
    /// home-path leak (cute-dbt#114/#123). `None` only for a synthetic node
    /// that omits the path.
    pub original_file_path: Option<String>,
    /// The bare names of the **direct** downstream models that `ref()` this
    /// seed — the seed's immediate blast radius (cute-dbt#350 critique S5:
    /// *direct* consumers, not the transitive closure). Sorted for a stable
    /// "feeds N models" line. Empty for an unreferenced seed.
    pub feeds_models: Vec<String>,
    /// The seed's `delimiter` config, as a pre-composed display string —
    /// `None` when unauthored (the `jaffle_shop` default; the key is absent
    /// from the real fusion config map, not present-but-null — cute-dbt#350
    /// critique S2). Filled by the adapter once the wire shape is pinned.
    pub delimiter: Option<String>,
    /// The seed's `quote_columns` config, as a display string — `None` when
    /// unauthored. Same provenance caveat as [`Self::delimiter`].
    pub quote_columns: Option<String>,
    /// The seed's `column_types` config, as a display string — `None` when
    /// unauthored. **Display only**: cell types are value-normalized from the
    /// CSV tokens, never derived from this declared/inferred schema (the
    /// cute-dbt#127 finding). Same provenance caveat as [`Self::delimiter`].
    pub column_types: Option<String>,
    /// The parsed CSV rows, filled by the CLI gather stage from the
    /// working-tree file. `None` until that slice lands, and `None` at render
    /// time when the file could not be read (the truthful-degrade state).
    pub table: Option<FixtureTable>,
    /// The aligned cell-diff, filled **only on the pr-diff arm** from the
    /// seed file's git hunks. `None` on the baseline arm (no old CSV exists
    /// in either manifest — manifests carry zero row data) and until the
    /// diff slice lands.
    pub diff: Option<FixtureTableDiff>,
}

impl SeedCard {
    /// Construct the identity-and-lineage skeleton produced by the
    /// `seeds_in_scope` projection: id, bare name, project-relative path, and
    /// the direct downstream model names. The data-bearing fields
    /// ([`Self::table`], [`Self::diff`]) and the config-display strings start
    /// empty — the adapter fills them.
    #[must_use]
    pub fn new(
        id: NodeId,
        name: impl Into<String>,
        original_file_path: Option<String>,
        feeds_models: Vec<String>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            original_file_path,
            feeds_models,
            delimiter: None,
            quote_columns: None,
            column_types: None,
            table: None,
            diff: None,
        }
    }

    /// The number of direct downstream models this seed feeds — the
    /// "feeds N models" lineage count (cute-dbt#350). Direct `ref()`
    /// consumers only, never the transitive closure.
    #[must_use]
    pub fn feeds_count(&self) -> usize {
        self.feeds_models.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> SeedCard {
        SeedCard::new(
            NodeId::new("seed.shop.raw_customers"),
            "raw_customers",
            Some("seeds/raw_customers.csv".to_owned()),
            vec!["stg_customers".to_owned(), "stg_orders".to_owned()],
        )
    }

    #[test]
    fn new_sets_identity_and_lineage_and_leaves_data_empty() {
        let c = card();
        assert_eq!(c.id, NodeId::new("seed.shop.raw_customers"));
        assert_eq!(c.name, "raw_customers");
        assert_eq!(
            c.original_file_path.as_deref(),
            Some("seeds/raw_customers.csv")
        );
        assert_eq!(c.feeds_models, vec!["stg_customers", "stg_orders"]);
        // The data-bearing + config-display fields start empty — the adapter
        // fills them in later slices.
        assert!(c.delimiter.is_none());
        assert!(c.quote_columns.is_none());
        assert!(c.column_types.is_none());
        assert!(c.table.is_none());
        assert!(c.diff.is_none());
    }

    #[test]
    fn feeds_count_is_the_direct_downstream_model_count() {
        assert_eq!(card().feeds_count(), 2);
    }

    #[test]
    fn feeds_count_is_zero_for_an_unreferenced_seed() {
        let c = SeedCard::new(
            NodeId::new("seed.shop.lonely"),
            "lonely",
            Some("seeds/lonely.csv".to_owned()),
            Vec::new(),
        );
        assert_eq!(c.feeds_count(), 0);
        assert!(c.feeds_models.is_empty());
    }

    #[test]
    fn name_accepts_owned_and_borrowed() {
        // `impl Into<String>` — pin both call shapes compile and store.
        let owned = SeedCard::new(NodeId::new("seed.s.a"), String::from("a"), None, Vec::new());
        let borrowed = SeedCard::new(NodeId::new("seed.s.a"), "a", None, Vec::new());
        assert_eq!(owned.name, "a");
        assert_eq!(borrowed.name, "a");
        assert_eq!(owned, borrowed);
    }

    #[test]
    fn serde_round_trips_the_skeleton() {
        // The payload crosses to the renderer as JSON — pin the round-trip
        // so a future field addition cannot silently break the wire shape.
        let c = card();
        let json = serde_json::to_string(&c).expect("serialize SeedCard");
        let back: SeedCard = serde_json::from_str(&json).expect("deserialize SeedCard");
        assert_eq!(c, back);
    }
}
