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
//! files for the node-graph inversion SHAPE — an adjacency built by
//! pushing the node-id binding of a `for (id, …) in ….nodes()` walk under
//! the producer key of a `for <p> in …depends_on().nodes()` pass — and
//! fails if any of them still contains it. The ONE inversion site
//! (`src/domain/lineage.rs`) is asserted to contain it. If a consumer
//! re-grows its own inversion, the seam isn't real and this gate goes red
//! — never a prose note someone has to remember.
//!
//! The fingerprint is name-independent (it reads the loop bindings, never
//! a hard-coded identifier), so a behaviour-preserving rename of the
//! pushed variable cannot evade it; see [`contains_inversion_loop`] for
//! the exact shape it does and does NOT cover.

use std::fs;
use std::path::Path;

/// Read a `src/`-relative file as a single string.
fn read_src(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()))
}

/// `true` when `src` contains a node-graph `depends_on` producer→consumer
/// inversion loop — the full-manifest self-inversion this hoist owns
/// exactly once.
///
/// What this fingerprint covers (and what it does NOT): it matches the
/// **structural shape** of a node self-inversion, not any one variable
/// name. The shape is an `.entry(<P>).or_default().push(<C>)` adjacency
/// build where the entry key `<P>` is the loop variable of a
/// `for <P> in …depends_on().nodes()` pass, and the pushed value `<C>` is
/// the node-id binding of the enclosing `for (<C>, …) in ….nodes()` walk
/// over the manifest's node map. Because BOTH the key and the pushed value
/// are read out of the surrounding loop bindings (not compared against any
/// hard-coded identifier), a behaviour-preserving rename of the pushed
/// variable — `consumer` → `child_id`, say — keeps tripping the check: a
/// rename moves the binding and the `.push(<that binding>)` together, so
/// the derived shape still matches. (The earlier fingerprint matched a
/// `.push(consumer` literal and so claimed a rename-robustness it did not
/// have — this is the honest replacement.)
///
/// It is deliberately NARROW to the node-graph self-inversion: a
/// producer-keyed `.entry().or_default().push()` build fed by a DIFFERENT
/// source (e.g. governance's `exposure.depends_on().nodes()` over
/// `manifest.exposures()`, or the explorer's macro/folder grouping) does
/// NOT match, because its pushed value is not the node-id binding of a
/// `….nodes()` node-graph walk. That is the point: only the one true
/// `manifest.nodes()` → invert `depends_on` site should light up.
fn contains_inversion_loop(src: &str) -> bool {
    // (1) The producer loop variable: `for <P> in …depends_on().nodes()`.
    let Some(producer) = depends_on_nodes_loop_var(src) else {
        return false;
    };
    // (2) The node-graph outer binding: `for (<C>, …) in ….nodes()` — the
    //     consumer node id pushed under the producer key. (Excludes the
    //     exposure-sink inversion, fed by `exposures().values()`.)
    let Some(consumer) = node_graph_tuple_binding(src) else {
        return false;
    };
    // (3) The adjacency build itself: `.entry(<P>)` … `.or_default()` …
    //     `.push(<C>)`, with the key the producer and the value the
    //     consumer node-id binding — name-independent (both derived from
    //     the loop bindings above, never a hard-coded identifier).
    src.contains(&format!(".entry({producer}"))
        && src.contains(".or_default()")
        && src.contains(&format!(".push({consumer}"))
}

/// The loop variable of a `for <V> in …depends_on().nodes()` pass, if any.
/// `<V>` is the producer id under which consumers are bucketed.
fn depends_on_nodes_loop_var(src: &str) -> Option<&str> {
    for line in src.lines() {
        let line = line.trim_start();
        if let Some(var) = line.strip_prefix("for ")
            && let Some((var, rest)) = var.split_once(" in ")
            && rest.contains("depends_on().nodes()")
        {
            return Some(var.trim());
        }
    }
    None
}

/// The node-id binding of a `for (<C>, …) in ….nodes()` walk over the
/// manifest's node map, if any. `<C>` is the consumer id pushed under each
/// producer. The destructured tuple shape (`(<C>, node)`) is what
/// distinguishes the node-graph self-inversion from the
/// `exposures().values()` / single-binding loops.
fn node_graph_tuple_binding(src: &str) -> Option<&str> {
    for line in src.lines() {
        let line = line.trim_start();
        if let Some(var) = line.strip_prefix("for (")
            && let Some((tuple, rest)) = var.split_once(" in ")
            && rest.contains(".nodes()")
        {
            // First element of the `(c, node)` destructure.
            let first = tuple.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(first);
            }
        }
    }
    None
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
