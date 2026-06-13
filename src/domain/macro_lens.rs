//! Macro perspective (cute-dbt#265) — the reverse macro blast radius and
//! changed-macro detection.
//!
//! A `macros/*.sql` edit is invisible to cute-dbt's model-and-unit-test
//! scope selection: a macro file matches no model `original_file_path`
//! and macros are not unit-test targets, so a macro change slips through
//! entirely. This module gives the report a **macro perspective** — the
//! two pure-domain building blocks the render lane (Slice B) consumes:
//!
//! - [`macro_blast_radius`] — given a changed macro `unique_id` **M**,
//!   every root-project **model** whose `depends_on.macros` *transitive*
//!   closure contains M.
//! - [`changed_macros_pr_diff`] / [`changed_macros_baseline`] — which
//!   macros a PR changed, one function per scope arm (a parsed PR diff or
//!   a baseline manifest comparison).
//!
//! ## The transitive walk is load-bearing (spike correction, 2026-06-13)
//!
//! A model's `depends_on.macros` is the **DIRECT call set, not the
//! flattened closure** — verified on a real fusion 2.0-preview.177
//! manifest: `customer_order_days` lists `is_incremental` but **not**
//! `should_full_refresh`, which `is_incremental` itself calls. So a
//! naive first-order membership test (`M ∈ model.depends_on.macros`)
//! under-reports: a model that reaches M only through an intermediate
//! macro would be missed. The blast radius therefore **forward-BFS-walks
//! the [`Manifest::macro_refs`] macro→macro family** (the cute-dbt#271
//! reference channel) from each model's direct set and marks the model
//! when the walk hits M. The macro→macro edges in `macro_refs` ARE the
//! transitive channel (fusion records the full macro-to-macro closure at
//! render time), so the model→macro edge being direct-only is the gap
//! this walk closes. Reuses the cycle-guarded BFS shape of
//! [`crate::domain::vars`]'s `scan_macro_closure`.
//!
//! ## Two mandatory filters
//!
//! 1. **`resource_type == "model"`** — generic schema-test nodes also
//!    carry `depends_on.macros` (the `get_where_subquery` edge), so a
//!    reverse over *all* nodes would flood the radius with hundreds of
//!    test nodes. Only models surface.
//! 2. **`package_name == metadata().project_name()`** — root-project
//!    models only. A vendor-package model that happens to call a
//!    root-project macro is not the reviewer's concern, and the project
//!    name is free + pr-diff-available (cute-dbt#256).
//!
//! ## Known fidelity limits (named, not silently dropped)
//!
//! - **Materialization macros are out of scope.** A materialization
//!   macro (`materialization xxx, adapter='yyy'`) is runtime-invoked, so
//!   it never appears in a model's `depends_on.macros` — editing one
//!   produces an empty blast radius here. The render lane names this in
//!   the section banner (the same posture as `ARCHITECTURE.md` §4).
//! - **Dispatch needs no special handling.** Dispatch indirection is
//!   pre-resolved on the wire (`macro_refs` records the adapter-resolved
//!   impl edge), so the closure walk crosses it transparently.
//! - **Render-observed, not static.** `depends_on.macros` is populated by
//!   the fusion renderer; a macro→macro edge on a Jinja branch not taken
//!   at compile time may be absent. The blast radius is "render-observed
//!   usage", inheriting dbt's own behavior.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::domain::manifest::{Manifest, NodeId};
use crate::domain::pr_diff::NormalizedDiffIndex;

/// Every root-project **model** node whose `depends_on.macros` transitive
/// closure contains the changed macro `changed_macro_id` (**M**).
///
/// A model is in the radius when M is in its **direct** `depends_on.macros`
/// set OR any macro in that set transitively calls M (forward BFS over
/// [`Manifest::macro_refs`], cycle-guarded). Filtered to `model` nodes of
/// the root project (`package_name == metadata().project_name()`); see the
/// module docs for why both filters are mandatory.
///
/// Returns node ids in deterministic id order ([`BTreeSet`]). An unknown
/// `changed_macro_id`, or a manifest with no root project name, yields an
/// empty set (fail-open — the macro section simply shows no impacted
/// models).
#[must_use]
pub fn macro_blast_radius(manifest: &Manifest, changed_macro_id: &str) -> BTreeSet<NodeId> {
    let Some(project) = manifest.metadata().project_name() else {
        // No root project name ⇒ the root-project filter cannot pass for
        // any model. Fail-open empty rather than leaking vendor models.
        return BTreeSet::new();
    };
    // Memo: macro id → does its forward closure reach M? Shared across
    // every model's walk so each macro body is visited at most once.
    let mut reaches: HashMap<String, bool> = HashMap::new();
    manifest
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .filter(|(_, node)| node.package_name() == Some(project))
        .filter(|(_, node)| {
            direct_set_reaches(
                manifest,
                node.depends_on().macros(),
                changed_macro_id,
                &mut reaches,
            )
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// Whether any macro in `roots` (a model's DIRECT `depends_on.macros`
/// set) equals or transitively reaches `target` (**M**) via the
/// [`Manifest::macro_refs`] forward edges.
///
/// Forward breadth-first walk, cycle-guarded — the
/// [`crate::domain::vars`] `scan_macro_closure` shape. The `reaches` memo
/// records each macro id's verdict so a macro shared across many models'
/// closures is walked once.
fn direct_set_reaches(
    manifest: &Manifest,
    roots: &[String],
    target: &str,
    reaches: &mut HashMap<String, bool>,
) -> bool {
    roots
        .iter()
        .any(|root| macro_reaches(manifest, root, target, reaches))
}

/// Whether macro `start` equals or transitively reaches `target` over the
/// `macro_refs` forward edges. Memoized + cycle-guarded.
///
/// A miss is the memo-rich case: every id the forward walk touched
/// provably does NOT reach `target` (reachability is monotone along a
/// path), so all of them settle `false` in one sweep. A hit only proves
/// `start` reaches `target` (the ids past the hit were never explored), so
/// only `start` is memoized — other ids settle on their own walks.
fn macro_reaches(
    manifest: &Manifest,
    start: &str,
    target: &str,
    reaches: &mut HashMap<String, bool>,
) -> bool {
    if let Some(&known) = reaches.get(start) {
        return known;
    }
    let walk = forward_walk_to(manifest, start, target);
    if walk.hit {
        reaches.insert(start.to_owned(), true);
    } else {
        reaches.extend(walk.seen.into_iter().map(|id| (id, false)));
    }
    walk.hit
}

/// The result of one forward BFS from a macro toward `target`.
struct ForwardWalk {
    /// Whether the walk reached `target`.
    hit: bool,
    /// Every macro id the walk visited (the cycle-guard set). On a miss
    /// this is the full not-reaching set; on a hit it is partial.
    seen: HashSet<String>,
}

/// Forward breadth-first walk from `start` over the [`Manifest::macro_refs`]
/// edges, stopping at the first id equal to `target`. The frontier order is
/// wire order (deterministic); `seen` is the cycle guard — the
/// [`crate::domain::vars`] `scan_macro_closure` shape.
fn forward_walk_to(manifest: &Manifest, start: &str, target: &str) -> ForwardWalk {
    let mut queue: Vec<String> = vec![start.to_owned()];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(start.to_owned());
    let mut at = 0;
    while at < queue.len() {
        let current = queue[at].clone();
        at += 1;
        if current == target {
            return ForwardWalk { hit: true, seen };
        }
        for next in manifest.macro_refs(&current) {
            if seen.insert(next.clone()) {
                queue.push(next.clone());
            }
        }
    }
    ForwardWalk { hit: false, seen }
}

/// The macros a PR changed, as the set of macro `unique_id`s, resolved
/// from a parsed PR diff (the CI / PR-review arm).
///
/// **Path-primary, name-fallback union** (the spike's verdict):
///
/// 1. **Path** (primary) — every changed file whose path matches a macro's
///    `original_file_path` ([`Manifest::macro_id_for_path`], rename- and
///    `--project-root`-aware via the [`NormalizedDiffIndex`]). The spike
///    verified `original_file_path` non-null 510/510 on real fusion, so
///    this is the reliable channel.
/// 2. **Name** (fallback) — every `{% macro NAME %}` declaration whose
///    body the diff added/changed, matched by `NAME` against a
///    root-project macro (`macro.<project_name>.<NAME>` via
///    [`Manifest::macro_id_for_name`]). Refines precision (multiple macros
///    per file) and is the safety net for the rare null `original_file_path`.
///
/// Both channels union; an id resolved by either is in the set. Returns
/// ids in deterministic order ([`BTreeSet`]). The name fallback keys under
/// the **root project** only — a `{% macro %}` declaration in a vendor
/// package file the PR happened to vendor never crosses into the root
/// project's resolution.
#[must_use]
pub fn changed_macros_pr_diff(
    manifest: &Manifest,
    index: &NormalizedDiffIndex,
) -> BTreeSet<String> {
    // Path-primary unioned with name-fallback. Each channel is its own
    // iterator-collecting helper, so the union here stays a flat `extend`
    // pair (low branch count); the per-channel nesting lives behind the
    // helper boundaries.
    let mut out = path_resolved_macros(manifest, index);
    out.extend(name_resolved_macros(manifest, index));
    out
}

/// Path-primary channel: every changed file whose path resolves to a
/// macro `unique_id` via [`Manifest::macro_id_for_path`].
fn path_resolved_macros(manifest: &Manifest, index: &NormalizedDiffIndex) -> BTreeSet<String> {
    index
        .changed_paths()
        .filter_map(|path| manifest.macro_id_for_path(path).map(str::to_owned))
        .collect()
}

/// Name-fallback channel: every `{% macro NAME %}` declaration added in a
/// changed file's hunks, resolved to a root-project macro `unique_id`
/// (`macro.<project_name>.<NAME>`). Empty when the manifest carries no
/// project name (the fallback keys under the root project only).
fn name_resolved_macros(manifest: &Manifest, index: &NormalizedDiffIndex) -> BTreeSet<String> {
    let Some(project) = manifest.metadata().project_name() else {
        return BTreeSet::new();
    };
    added_declaration_names(index)
        .filter_map(|name| {
            manifest
                .macro_id_for_name(project, &name)
                .map(str::to_owned)
        })
        .collect()
}

/// Every `{% macro NAME %}` declaration name across all added lines of
/// every changed file's hunks (the name-fallback scan input).
fn added_declaration_names(index: &NormalizedDiffIndex) -> impl Iterator<Item = String> + '_ {
    index
        .changed_paths()
        .flat_map(move |path| index.hunks_for(path))
        .flat_map(|hunk| &hunk.added_lines)
        .flat_map(|line| macro_declaration_names(line))
}

/// The macros a PR changed, resolved by comparing two baseline manifests
/// (the `--baseline-manifest` arm) — matching fusion's
/// `check_modified_macros` semantics EXACTLY.
///
/// Only a macro **present in BOTH manifests** whose `macro_sql` (trimmed)
/// differs is flagged. Added macros (in current, absent from baseline) and
/// removed macros (in baseline, absent from current) are **deliberately
/// NOT flagged** — fusion intentionally disables the add/remove branch
/// (`prev_state.rs:483-514` @ `9977b6cb…`) to avoid false positives from
/// auto-generated test macros that appear/disappear between runs. The
/// `trim()` matches fusion's body-equality comparison (leading/trailing
/// whitespace is not a semantic macro change).
///
/// Returns ids in deterministic order ([`BTreeSet`]).
#[must_use]
pub fn changed_macros_baseline(current: &Manifest, baseline: &Manifest) -> BTreeSet<String> {
    let baseline_macros: &HashMap<String, String> = baseline.macros();
    current
        .macros()
        .iter()
        .filter_map(|(id, current_sql)| {
            baseline_macros.get(id).and_then(|baseline_sql| {
                (current_sql.trim() != baseline_sql.trim()).then(|| id.clone())
            })
        })
        .collect()
}

/// Extract every `NAME` from a `{% macro NAME(...) %}` declaration in one
/// diff line. dbt declarations are one-per-line in practice, but a `for`
/// loop costs nothing and tolerates the pathological multi-declaration
/// line.
///
/// Accepts the whitespace-control variants fusion's lexer does
/// (`{%- macro` / `{%macro` / `{% macro`); the name is the identifier run
/// after the `macro` keyword, terminated by `(` or whitespace. Pure string
/// scanning (no regex dep) — the [`crate::domain::vars`] hand-rolled
/// scanner precedent.
fn macro_declaration_names(line: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = line.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find("{%") {
        let open = search_from + rel;
        search_from = open + 2;
        // Skip past `{%`, an optional `-` whitespace-control marker, and
        // any inter-token whitespace.
        let mut j = open + 2;
        if bytes.get(j) == Some(&b'-') {
            j += 1;
        }
        j = skip_ws(bytes, j);
        // Require the `macro` keyword followed by whitespace (else it is an
        // identifier like `macroni`, not the `{% macro %}` tag).
        let after_kw = j + b"macro".len();
        let is_macro_tag = bytes.get(j..after_kw) == Some(b"macro")
            && bytes.get(after_kw).is_some_and(u8::is_ascii_whitespace);
        if !is_macro_tag {
            continue;
        }
        let name_start = skip_ws(bytes, after_kw);
        let name_end = ident_end(bytes, name_start);
        if name_end > name_start {
            names.push(line[name_start..name_end].to_owned());
        }
    }
    names
}

/// Advance past ASCII whitespace.
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// Byte offset just past a `[A-Za-z0-9_]+` identifier run starting at `i`.
fn ident_end(bytes: &[u8], i: usize) -> usize {
    let mut j = i;
    while bytes
        .get(j)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        j += 1;
    }
    j
}

// A precomputed impacted-models-per-macro index is deliberately NOT built
// here — the render lane (Slice B) calls `macro_blast_radius` once per
// changed macro (few per PR), so a per-macro index would be premature.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, MacroIdentity, ManifestMetadata, Node, NodeConfig,
    };
    use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff};
    use std::collections::BTreeMap;

    const PROJECT: &str = "shop";

    fn id(name: &str) -> NodeId {
        NodeId::new(name)
    }

    /// A root-project `model` node with the given id and DIRECT macro deps.
    fn model_with_macros(full_id: &str, direct_macros: &[&str]) -> Node {
        model_with_macros_in_pkg(full_id, direct_macros, PROJECT)
    }

    /// A `model` node in an arbitrary package (for the root-project filter).
    fn model_with_macros_in_pkg(full_id: &str, direct_macros: &[&str], pkg: &str) -> Node {
        let macros = direct_macros.iter().map(|m| (*m).to_owned()).collect();
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", "abc"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(macros, vec![]),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(None, Some(pkg.to_owned()))
    }

    /// A non-model node (`resource_type` filter test) carrying macro deps.
    fn typed_node_with_macros(full_id: &str, resource_type: &str, direct_macros: &[&str]) -> Node {
        let macros = direct_macros.iter().map(|m| (*m).to_owned()).collect();
        Node::new(
            NodeId::new(full_id),
            resource_type,
            Checksum::new("sha256", "abc"),
            None,
            None,
            DependsOn::new(macros, vec![]),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(None, Some(PROJECT.to_owned()))
    }

    /// Build a manifest with the given nodes + macro→macro reference edges.
    fn manifest_with(
        nodes: Vec<Node>,
        macro_edges: &[(&str, &[&str])],
        macro_bodies: &[(&str, &str)],
    ) -> Manifest {
        let node_map = nodes.into_iter().map(|n| (n.id().clone(), n)).collect();
        let macros = macro_bodies
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let mut macro_depends_on = BTreeMap::new();
        for (from, tos) in macro_edges {
            if !tos.is_empty() {
                macro_depends_on.insert(
                    (*from).to_owned(),
                    tos.iter().map(|t| (*t).to_owned()).collect(),
                );
            }
        }
        Manifest::new(
            ManifestMetadata::new("v12").with_project_name(Some(PROJECT.to_owned())),
            node_map,
            std::collections::HashMap::new(),
            macros,
        )
        .with_macro_depends_on(macro_depends_on)
    }

    // ----- macro_blast_radius: first-order membership ------------------

    #[test]
    fn blast_radius_includes_a_direct_caller() {
        let m = manifest_with(
            vec![model_with_macros(
                "model.shop.orders",
                &["macro.shop.add_dq_flags"],
            )],
            &[],
            &[],
        );
        let radius = macro_blast_radius(&m, "macro.shop.add_dq_flags");
        assert!(radius.contains(&id("model.shop.orders")));
        assert_eq!(radius.len(), 1);
    }

    #[test]
    fn blast_radius_excludes_a_non_caller() {
        let m = manifest_with(
            vec![model_with_macros(
                "model.shop.orders",
                &["macro.shop.other"],
            )],
            &[],
            &[],
        );
        let radius = macro_blast_radius(&m, "macro.shop.add_dq_flags");
        assert!(radius.is_empty());
    }

    // ----- macro_blast_radius: the TRANSITIVE case (the correctness-
    // interesting case the reverse adds over first-order membership) -----

    #[test]
    fn blast_radius_includes_a_model_reaching_m_only_via_an_intermediate_macro() {
        // model -> is_incremental (DIRECT) -> should_full_refresh (M).
        // The model does NOT list should_full_refresh directly (the spike's
        // verified shape). A first-order test would MISS this model; the
        // transitive walk catches it.
        let m = manifest_with(
            vec![model_with_macros(
                "model.shop.customer_order_days",
                &["macro.shop.is_incremental"],
            )],
            &[(
                "macro.shop.is_incremental",
                &["macro.shop.should_full_refresh"],
            )],
            &[
                (
                    "macro.shop.is_incremental",
                    "{% macro is_incremental() %}{% endmacro %}",
                ),
                (
                    "macro.shop.should_full_refresh",
                    "{% macro should_full_refresh() %}{% endmacro %}",
                ),
            ],
        );
        // M is the INTERMEDIATE target, reached only transitively.
        let radius = macro_blast_radius(&m, "macro.shop.should_full_refresh");
        assert!(
            radius.contains(&id("model.shop.customer_order_days")),
            "transitive reach via an intermediate macro must be in the radius",
        );
    }

    #[test]
    fn blast_radius_transitive_property_over_a_macro_chain_of_every_depth() {
        // Property: a chain model -> m0 -> m1 -> ... -> mN. Editing ANY
        // macro mk in the chain (0 <= k <= N) must put the model in the
        // radius (the model reaches every link transitively); editing a
        // macro OFF the chain must not. Exhaustive over chain length 0..6
        // and every edited link, the repo's exhaustive-property convention.
        for chain_len in 0usize..6 {
            let macro_ids: Vec<String> = (0..=chain_len)
                .map(|k| format!("macro.shop.m{k}"))
                .collect();
            // Edges m0->m1->...->mN.
            let edges: Vec<(String, Vec<String>)> = (0..chain_len)
                .map(|k| (macro_ids[k].clone(), vec![macro_ids[k + 1].clone()]))
                .collect();
            let edge_refs: Vec<(&str, &[String])> = edges
                .iter()
                .map(|(f, t)| (f.as_str(), t.as_slice()))
                .collect();
            // The model directly calls only m0.
            let direct = vec![macro_ids[0].as_str()];
            let node = model_with_macros("model.shop.chained", &direct);
            // Build the manifest (custom edge wiring with owned strings).
            let mut macro_depends_on = BTreeMap::new();
            for (from, tos) in &edge_refs {
                macro_depends_on.insert((*from).to_owned(), tos.to_vec());
            }
            let macros = macro_ids
                .iter()
                .map(|mid| (mid.clone(), format!("/* {mid} */")))
                .collect();
            let m = Manifest::new(
                ManifestMetadata::new("v12").with_project_name(Some(PROJECT.to_owned())),
                std::iter::once((node.id().clone(), node)).collect(),
                std::collections::HashMap::new(),
                macros,
            )
            .with_macro_depends_on(macro_depends_on);

            // Editing each link in the chain reaches the model.
            for mid in &macro_ids {
                let radius = macro_blast_radius(&m, mid);
                assert!(
                    radius.contains(&id("model.shop.chained")),
                    "chain_len={chain_len}: editing {mid} (depth in chain) must reach the model",
                );
            }
            // Editing a macro OFF the chain reaches nothing.
            let radius = macro_blast_radius(&m, "macro.shop.unrelated");
            assert!(
                radius.is_empty(),
                "chain_len={chain_len}: an off-chain macro reaches no model",
            );
        }
    }

    #[test]
    fn blast_radius_terminates_on_a_macro_cycle() {
        // a -> b -> a is a (degenerate, but defensible) cycle. The walk
        // must terminate and still find M whether M is on or off the cycle.
        let m = manifest_with(
            vec![model_with_macros("model.shop.x", &["macro.shop.a"])],
            &[
                ("macro.shop.a", &["macro.shop.b"]),
                ("macro.shop.b", &["macro.shop.a"]),
            ],
            &[("macro.shop.a", "/* a */"), ("macro.shop.b", "/* b */")],
        );
        assert!(macro_blast_radius(&m, "macro.shop.b").contains(&id("model.shop.x")));
        assert!(macro_blast_radius(&m, "macro.shop.a").contains(&id("model.shop.x")));
        assert!(macro_blast_radius(&m, "macro.shop.off_cycle").is_empty());
    }

    // ----- S5: dispatch macros do NOT under-report (cute-dbt#265 Slice D)

    #[test]
    fn blast_radius_reaches_a_dispatched_impl_macro_edited_directly() {
        // critique S5 — the dispatch-macro under-report check, verified
        // against TWO real compiled manifests (fusion 2.0-preview.177:
        // 184/184 dispatchers record their impl edge; core 1.11.2: 281/281).
        // The wire shape:
        //   model.shop.orders.depends_on.macros = [generic]   (dispatcher
        //                                                       entrypoint
        //                                                       ONLY — never
        //                                                       the impl)
        //   macro.shop.dateadd ({% ... adapter.dispatch('dateadd') ... %})
        //       .depends_on.macros = [default__dateadd]        (the
        //                                                        adapter-
        //                                                        resolved
        //                                                        impl edge,
        //                                                        baked at
        //                                                        parse time)
        //   macro.shop.default__dateadd.depends_on.macros = [] (leaf impl)
        // Editing the IMPL macro must still reach the model — the dispatcher
        // edge is a static, recorded macro->macro ref the BFS crosses. If
        // this assertion ever fails, dispatch resolution stopped being
        // statically recorded and the lens would under-report — file a
        // tracking issue and surface the limit in the section banner.
        let generic = "macro.shop.dateadd";
        let impl_macro = "macro.shop.default__dateadd";
        let m = manifest_with(
            vec![model_with_macros("model.shop.orders", &[generic])],
            // The dispatcher records its adapter-resolved impl edge; the
            // impl is a leaf (no further macro deps).
            &[(generic, &[impl_macro]), (impl_macro, &[])],
            &[
                (
                    generic,
                    "{% macro dateadd(d, p) %}{{ adapter.dispatch('dateadd')(d, p) }}{% endmacro %}",
                ),
                (
                    impl_macro,
                    "{% macro default__dateadd(d, p) %}dateadd({{ d }}, {{ p }}){% endmacro %}",
                ),
            ],
        );
        // Editing the IMPL reaches the model (the under-report case).
        assert!(
            macro_blast_radius(&m, impl_macro).contains(&id("model.shop.orders")),
            "editing a dispatched impl macro must reach the calling model \
             (dispatch is statically recorded — no under-report)",
        );
        // Editing the GENERIC dispatcher also reaches the model (the direct
        // case — the model lists the dispatcher in depends_on.macros).
        assert!(
            macro_blast_radius(&m, generic).contains(&id("model.shop.orders")),
            "editing the generic dispatcher reaches the model directly",
        );
    }

    // ----- the two mandatory filters -----------------------------------

    #[test]
    fn blast_radius_excludes_non_model_resource_types() {
        // A generic test node carries depends_on.macros (get_where_subquery)
        // — it must NOT flood the radius (filter a).
        let m = manifest_with(
            vec![
                typed_node_with_macros(
                    "test.shop.not_null_orders_id.abc",
                    "test",
                    &["macro.shop.add_dq_flags"],
                ),
                model_with_macros("model.shop.orders", &["macro.shop.add_dq_flags"]),
            ],
            &[],
            &[],
        );
        let radius = macro_blast_radius(&m, "macro.shop.add_dq_flags");
        assert_eq!(
            radius.len(),
            1,
            "only the model surfaces, never the test node"
        );
        assert!(radius.contains(&id("model.shop.orders")));
    }

    #[test]
    fn blast_radius_excludes_non_root_project_models() {
        // A vendor-package model that calls a root-project macro is not the
        // reviewer's concern (filter b).
        let m = manifest_with(
            vec![
                model_with_macros("model.shop.orders", &["macro.shop.add_dq_flags"]),
                model_with_macros_in_pkg(
                    "model.dbt_utils.helper",
                    &["macro.shop.add_dq_flags"],
                    "dbt_utils",
                ),
            ],
            &[],
            &[],
        );
        let radius = macro_blast_radius(&m, "macro.shop.add_dq_flags");
        assert_eq!(radius.len(), 1, "only the root-project model surfaces");
        assert!(radius.contains(&id("model.shop.orders")));
    }

    #[test]
    fn blast_radius_empty_when_no_project_name() {
        // No root project name ⇒ fail-open empty (never leak vendor models).
        let node = model_with_macros("model.shop.orders", &["macro.shop.add_dq_flags"]);
        let m = Manifest::new(
            ManifestMetadata::new("v12"),
            std::iter::once((node.id().clone(), node)).collect(),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        assert!(macro_blast_radius(&m, "macro.shop.add_dq_flags").is_empty());
    }

    #[test]
    fn blast_radius_unknown_macro_is_empty() {
        let m = manifest_with(
            vec![model_with_macros(
                "model.shop.orders",
                &["macro.shop.add_dq_flags"],
            )],
            &[],
            &[],
        );
        assert!(macro_blast_radius(&m, "macro.shop.nonexistent").is_empty());
    }

    // ----- changed_macros_baseline (fusion check_modified_macros) -------

    fn manifest_with_macros(bodies: &[(&str, &str)]) -> Manifest {
        let macros = bodies
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Manifest::new(
            ManifestMetadata::new("v12").with_project_name(Some(PROJECT.to_owned())),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            macros,
        )
    }

    #[test]
    fn baseline_flags_a_body_changed_macro_present_in_both() {
        let current = manifest_with_macros(&[("macro.shop.f", "/* new body */")]);
        let baseline = manifest_with_macros(&[("macro.shop.f", "/* old body */")]);
        let changed = changed_macros_baseline(&current, &baseline);
        assert!(changed.contains("macro.shop.f"));
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn baseline_ignores_a_whitespace_only_macro_change() {
        // fusion compares trimmed bodies — leading/trailing whitespace is
        // not a semantic change.
        let current = manifest_with_macros(&[("macro.shop.f", "  /* body */  ")]);
        let baseline = manifest_with_macros(&[("macro.shop.f", "/* body */")]);
        assert!(changed_macros_baseline(&current, &baseline).is_empty());
    }

    #[test]
    fn baseline_does_not_flag_an_added_macro() {
        // Added (in current, absent from baseline) is DELIBERATELY not
        // flagged — fusion disables the add branch (auto-gen test macros).
        let current = manifest_with_macros(&[("macro.shop.f", "x"), ("macro.shop.g", "new")]);
        let baseline = manifest_with_macros(&[("macro.shop.f", "x")]);
        assert!(changed_macros_baseline(&current, &baseline).is_empty());
    }

    #[test]
    fn baseline_does_not_flag_a_removed_macro() {
        // Removed (in baseline, absent from current) is DELIBERATELY not
        // flagged — fusion disables the remove branch.
        let current = manifest_with_macros(&[("macro.shop.f", "x")]);
        let baseline = manifest_with_macros(&[("macro.shop.f", "x"), ("macro.shop.gone", "old")]);
        assert!(changed_macros_baseline(&current, &baseline).is_empty());
    }

    #[test]
    fn baseline_flags_only_the_differing_macros() {
        let current =
            manifest_with_macros(&[("macro.shop.same", "unchanged"), ("macro.shop.diff", "new")]);
        let baseline =
            manifest_with_macros(&[("macro.shop.same", "unchanged"), ("macro.shop.diff", "old")]);
        let changed = changed_macros_baseline(&current, &baseline);
        assert_eq!(changed.len(), 1);
        assert!(changed.contains("macro.shop.diff"));
    }

    // ----- changed_macros_pr_diff: path-primary + name-fallback ---------

    fn manifest_with_identity(triples: &[(&str, &str, &str, &str)]) -> Manifest {
        // (unique_id, original_file_path, name, package_name)
        let mut identity = BTreeMap::new();
        let mut macros = std::collections::HashMap::new();
        for (uid, ofp, name, pkg) in triples {
            identity.insert(
                (*uid).to_owned(),
                MacroIdentity::new(
                    Some((*ofp).to_owned()),
                    Some((*name).to_owned()),
                    Some((*pkg).to_owned()),
                ),
            );
            macros.insert((*uid).to_owned(), format!("/* {uid} */"));
        }
        Manifest::new(
            ManifestMetadata::new("v12").with_project_name(Some(PROJECT.to_owned())),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            macros,
        )
        .with_macro_identity(identity)
    }

    fn diff_index(files: Vec<FileHunks>) -> NormalizedDiffIndex {
        NormalizedDiffIndex::new(
            &PrDiff {
                files,
                renames: vec![],
            },
            None,
        )
    }

    fn file_with_added(path: &str, added: &[&str]) -> FileHunks {
        FileHunks {
            path: path.to_owned(),
            hunks: vec![Hunk {
                new_start: 1,
                new_len: added.len(),
                removed_lines: vec![],
                added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
            }],
        }
    }

    #[test]
    fn pr_diff_resolves_a_changed_macro_file_by_path() {
        let m = manifest_with_identity(&[(
            "macro.shop.add_dq_flags",
            "macros/add_dq_flags.sql",
            "add_dq_flags",
            "shop",
        )]);
        let index = diff_index(vec![file_with_added(
            "macros/add_dq_flags.sql",
            &["select 1"],
        )]);
        let changed = changed_macros_pr_diff(&m, &index);
        assert!(changed.contains("macro.shop.add_dq_flags"));
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn pr_diff_name_fallback_resolves_a_declaration_when_path_misses() {
        // The macro's original_file_path is NULL (the rare fusion case the
        // spike named), so the path channel cannot resolve it — but the
        // hunk added a `{% macro NAME %}` declaration, so the name fallback
        // catches it under the root project.
        let mut identity = BTreeMap::new();
        identity.insert(
            "macro.shop.add_dq_flags".to_owned(),
            MacroIdentity::new(
                None,
                Some("add_dq_flags".to_owned()),
                Some("shop".to_owned()),
            ),
        );
        let mut macros = std::collections::HashMap::new();
        macros.insert("macro.shop.add_dq_flags".to_owned(), "x".to_owned());
        let m = Manifest::new(
            ManifestMetadata::new("v12").with_project_name(Some(PROJECT.to_owned())),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            macros,
        )
        .with_macro_identity(identity);
        // Path "macros/dq.sql" matches no original_file_path; the added
        // line declares the macro.
        let index = diff_index(vec![file_with_added(
            "macros/dq.sql",
            &[
                "{% macro add_dq_flags(col) %}",
                "  {{ col }}",
                "{% endmacro %}",
            ],
        )]);
        let changed = changed_macros_pr_diff(&m, &index);
        assert!(
            changed.contains("macro.shop.add_dq_flags"),
            "name fallback must resolve the declaration",
        );
    }

    #[test]
    fn pr_diff_name_fallback_handles_whitespace_control_and_no_space_variants() {
        // {%- macro X %} and {%macro X%} both declare X.
        let m = manifest_with_identity(&[
            ("macro.shop.a", "macros/a.sql", "a", "shop"),
            ("macro.shop.b", "macros/b.sql", "b", "shop"),
        ]);
        let index = diff_index(vec![
            file_with_added("macros/changed_a.sql", &["{%- macro a() %}{% endmacro %}"]),
            file_with_added("macros/changed_b.sql", &["{%macro b(x)%}{%endmacro%}"]),
        ]);
        let changed = changed_macros_pr_diff(&m, &index);
        assert!(changed.contains("macro.shop.a"));
        assert!(changed.contains("macro.shop.b"));
    }

    #[test]
    fn pr_diff_name_fallback_keys_under_root_project_only() {
        // A declaration named `helper` exists in BOTH a vendor and the root
        // project. The fallback resolves ONLY the root-project id.
        let m = manifest_with_identity(&[
            ("macro.shop.helper", "macros/helper.sql", "helper", "shop"),
            (
                "macro.dbt_utils.helper",
                "macros/u.sql",
                "helper",
                "dbt_utils",
            ),
        ]);
        let index = diff_index(vec![file_with_added(
            "some/other/path.sql",
            &["{% macro helper() %}{% endmacro %}"],
        )]);
        let changed = changed_macros_pr_diff(&m, &index);
        assert!(changed.contains("macro.shop.helper"));
        assert!(
            !changed.contains("macro.dbt_utils.helper"),
            "the fallback never resolves a vendor-package id",
        );
    }

    #[test]
    fn pr_diff_path_and_name_union_dedupe() {
        // The same macro resolved by BOTH path and name appears once.
        let m = manifest_with_identity(&[(
            "macro.shop.add_dq_flags",
            "macros/add_dq_flags.sql",
            "add_dq_flags",
            "shop",
        )]);
        let index = diff_index(vec![file_with_added(
            "macros/add_dq_flags.sql",
            &["{% macro add_dq_flags() %}{% endmacro %}"],
        )]);
        let changed = changed_macros_pr_diff(&m, &index);
        assert_eq!(changed.len(), 1, "path + name resolve the same id once");
        assert!(changed.contains("macro.shop.add_dq_flags"));
    }

    #[test]
    fn pr_diff_ignores_non_macro_files() {
        let m = manifest_with_identity(&[(
            "macro.shop.add_dq_flags",
            "macros/add_dq_flags.sql",
            "add_dq_flags",
            "shop",
        )]);
        let index = diff_index(vec![
            file_with_added("models/orders.sql", &["select 1"]),
            file_with_added("README.md", &["docs"]),
        ]);
        assert!(changed_macros_pr_diff(&m, &index).is_empty());
    }

    #[test]
    fn macro_declaration_names_ignores_non_declarations() {
        // A `macroni` identifier or a `{% macro %}` with no name yields no
        // names; a `{% set x %}` tag is not a macro declaration.
        assert!(macro_declaration_names("{% set x = 1 %}").is_empty());
        assert!(macro_declaration_names("{{ macroni }}").is_empty());
        assert!(macro_declaration_names("select macro_col from t").is_empty());
        assert_eq!(
            macro_declaration_names("{% macro good_one(a, b) %}"),
            vec!["good_one".to_owned()],
        );
    }
}
