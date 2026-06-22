//! The falsifiable seam test for the cross-model-lineage hoist
//! (cute-dbt#443, source-map-spine S0).
//!
//! The hoist's whole point: the full-manifest `depends_on` self-inversion
//! (producer → the node ids that consume it) happens **exactly once**, in
//! [`cute_dbt::domain::lineage::invert_depends_on`]. The three consumers
//! that each ran their own inversion before the hoist —
//! `adapters::explore::build_lineage`, `domain::pr_dag::compute_pr_dag`'s
//! `ModelAdjacency`, and `domain::governance::reverse_node_adjacency` —
//! become FILTERS / direction-reads over that one source and must NEVER
//! re-implement the inversion loop.
//!
//! This test makes that falsifiable: it scans the three consumer source
//! files for the inversion SHAPE (an adjacency built by iterating
//! `depends_on().nodes()` and pushing the *consumer* id under the
//! *producer* key) and fails if any of them still contains it. The ONE
//! inversion site (`src/domain/lineage.rs`) is asserted to contain it.
//! If a consumer re-grows its own inversion, the seam isn't real and this
//! gate goes red — never a prose note someone has to remember.

use std::fs;
use std::path::Path;

/// Read a `src/`-relative file as a single string.
fn read_src(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()))
}

/// `true` when `src` contains a `depends_on` producer→consumer inversion
/// loop: a line reading `node.depends_on().nodes()` (or `.depends_on()`'s
/// `.nodes()`) co-located in the same file with the
/// `.entry(producer…).or_default().push(consumer…)` adjacency-build
/// idiom. We look for the structural fingerprint rather than exact text so
/// a behaviour-preserving rename can't silently re-introduce the
/// inversion under a new variable name.
fn contains_inversion_loop(src: &str) -> bool {
    // The producer→consumer push under an `or_default()` adjacency entry,
    // fed by `depends_on().nodes()`, is the inversion fingerprint.
    let reads_depends_on_nodes = src.contains("depends_on().nodes()");
    let builds_reverse_adjacency = src.contains(".or_default()") && src.contains(".push(consumer");
    reads_depends_on_nodes && builds_reverse_adjacency
}

/// The ONE inversion site DOES carry the loop (the test's own oracle —
/// if this regresses, the fingerprint is wrong, not the consumers).
#[test]
fn the_single_inversion_site_owns_the_loop() {
    let lineage = read_src("domain/lineage.rs");
    assert!(
        contains_inversion_loop(&lineage),
        "src/domain/lineage.rs must contain the ONE depends_on inversion \
         loop — the seam test's fingerprint drifted from the real code"
    );
}

/// `compute_pr_dag`'s `ModelAdjacency` must be a model→model VIEW of the
/// hoisted source, NOT a re-inversion.
#[test]
fn pr_dag_does_not_reinvert_depends_on() {
    let pr_dag = read_src("domain/pr_dag.rs");
    // The non-test prefix: stop at the `#[cfg(test)]` module so a test
    // helper that inlines a builder cannot trip the structural scan.
    let prod = non_test_prefix(&pr_dag);
    assert!(
        !contains_inversion_loop(prod),
        "domain::pr_dag re-inverts depends_on — it must read the ONE \
         source (crate::domain::lineage::invert_depends_on) and filter to \
         the model→model subgraph (cute-dbt#443 seam)"
    );
    assert!(
        prod.contains("invert_depends_on"),
        "domain::pr_dag must read the hoisted inversion via \
         invert_depends_on (the seam)"
    );
}

/// `governance::reverse_node_adjacency` must delegate to the hoisted
/// source, NOT re-implement the inversion.
#[test]
fn governance_does_not_reinvert_depends_on() {
    let governance = read_src("domain/governance.rs");
    let prod = non_test_prefix(&governance);
    assert!(
        !contains_inversion_loop(prod),
        "domain::governance re-inverts depends_on — reverse_node_adjacency \
         must delegate to crate::domain::lineage::invert_depends_on \
         (cute-dbt#443 seam)"
    );
    assert!(
        prod.contains("invert_depends_on"),
        "domain::governance must read the hoisted inversion via \
         invert_depends_on (the seam)"
    );
}

/// `adapters::explore::build_lineage` reads the FORWARD `depends_on`
/// direction (upstream→downstream edges) — it never inverted, and must
/// still never invert (no producer→consumer reverse-adjacency build).
#[test]
fn explore_build_lineage_does_not_invert_depends_on() {
    let explore = read_src("adapters/explore.rs");
    let prod = non_test_prefix(&explore);
    assert!(
        !contains_inversion_loop(prod),
        "adapters::explore must not build a producer→consumer reverse \
         adjacency — build_lineage is a forward-edge scoped view over the \
         lineage facts (cute-dbt#443 seam)"
    );
}

/// Return the source up to the first `#[cfg(test)]` line, so a `#[cfg(test)]`
/// helper that inlines a builder for assertion purposes is excluded from
/// the production-code structural scan.
fn non_test_prefix(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(idx) => &src[..idx],
        None => src,
    }
}
