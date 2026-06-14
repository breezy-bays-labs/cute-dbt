//! PR review-comment render grouping (cute-dbt#419–#422, epic #353).
//!
//! The render half of the PR-comments arc: take the ingested review
//! threads ([`PrCommentThread`], cute-dbt#395), anchor each onto the
//! report's rendered diff (the **shipped**
//! [`anchor_comment_thread`],
//! cute-dbt#418 — re-used verbatim, never re-anchored here), then bucket
//! the anchored results **per model** so the renderer can place each
//! thread at its diff line under the right model and show a per-model
//! count + a report-wide total.
//!
//! ## Why a separate grouping pass (not in the anchor module)
//!
//! [`anchor_comment_thread`] answers one question — *where does this one
//! thread land on the rendered diff?* — and is deliberately model-blind
//! (it knows the diff index, not the manifest). The report renders **per
//! model**: each model has its own Model-SQL diff, its own count tooltip,
//! and the top-of-report button navigates to a specific model. So a second
//! pure pass maps each
//! [`ResolvedThread`](crate::domain::ResolvedThread)'s project-relative
//! `path` back to the **model node** that owns that file (via
//! [`Node::original_file_path`](crate::domain::Node::original_file_path))
//! and buckets accordingly. The two passes compose cleanly: anchoring is
//! the diff join, grouping is the manifest join.
//!
//! ## Honest placement (never a mis-anchor)
//!
//! The grouping inherits the anchor module's never-a-false-claim posture:
//!
//! - A [`ThreadAnchor::Resolved`] thread on a file that maps to a model
//!   node becomes a [`RenderedThread`] with [`RenderedThread::model`]
//!   `Some(id)` and a concrete [`line`](RenderedThread::line) — the inline
//!   case.
//! - A [`ThreadAnchor::Outdated`] thread is carried as a `RenderedThread`
//!   with [`outdated`](RenderedThread::outdated) `true` and **no live
//!   line** ([`line`](RenderedThread::line) `None`, carrying
//!   `original_line` instead). If its (original) path still maps to a
//!   model node, it is bucketed under that model (so the reviewer sees
//!   "this model has an outdated comment"); otherwise it lands in the
//!   report-wide [`CommentsView::unanchored`] list.
//! - A [`ThreadAnchor::PathNotInDiff`] thread (the file is not in the
//!   rendered diff, or maps to no model node) is carried in
//!   [`CommentsView::unanchored`] — surfaced honestly, never dropped and
//!   never pinned to the wrong line.
//!
//! ## Purity
//!
//! Pure domain (std + serde only): the grouping borrows the already-parsed
//! [`NormalizedDiffIndex`](crate::domain::pr_diff::NormalizedDiffIndex) and
//! [`Manifest`], does no I/O, and re-uses the
//! shipped anchor resolver. The render PODs derive `Serialize` so the
//! renderer can emit them into the report payload (the JS reads
//! `DATA.pr_comments`, the `pr_dag` / `seed_cards` precedent).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::domain::manifest::Manifest;
use crate::domain::pr_comment::{DiffSide, PrCommentThread};
use crate::domain::pr_comment_anchor::{ThreadAnchor, anchor_comment_thread};

/// One comment within a rendered thread — the GitHub-style author + body
/// the report draws as a comment row. The render twin of
/// [`PrComment`](crate::domain::PrComment) (no behavior, just the two
/// rendered facts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderedComment {
    /// The comment author's GitHub login, or `None` for a deleted/ghost
    /// account (a truthful absence — the renderer draws "ghost", never an
    /// empty login).
    pub author: Option<String>,
    /// The comment body, verbatim (GitHub-flavored Markdown source). The
    /// renderer draws it as escaped text — never `|safe`, never parsed as
    /// HTML (the manifest-derived-string XSS posture).
    pub body: String,
}

/// One review thread, anchored and ready to render.
///
/// `model` is the node-id of the model whose file the thread anchors to
/// (`None` when the path maps to no model node — those land in
/// [`CommentsView::unanchored`]). `line` is the live diff line the thread
/// pins to on its [`side`](Self::side) (`None` for an outdated thread,
/// whose [`original_line`](Self::original_line) carries where it used to
/// be). `within_hunk` records whether a live line falls inside a changed
/// hunk (the usual review case) or only in the rendered file context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderedThread {
    /// The model node-id this thread belongs to, when its file maps to a
    /// model node. `None` ⇒ the thread is report-wide
    /// ([`CommentsView::unanchored`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The file the thread anchors to, project-relative (the normalized
    /// path the anchor resolver returned).
    pub path: String,
    /// The live 1-based diff line on [`side`](Self::side). `None` for an
    /// outdated thread (no live line) — see
    /// [`original_line`](Self::original_line).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The line the thread referred to at its original commit, present
    /// when known. The renderer shows it for an outdated thread ("was on
    /// line N").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_line: Option<u32>,
    /// Which side of the diff the (live) line lives on. Carried even for an
    /// outdated thread (it describes the original anchor side).
    pub side: DiffSide,
    /// Whether [`line`](Self::line) falls inside a changed hunk (`true`)
    /// or only in the file's rendered context (`false`). Always `false`
    /// for an outdated thread (there is no live line to be inside a hunk).
    pub within_hunk: bool,
    /// Whether the thread has been marked resolved on GitHub. The renderer
    /// draws resolved threads visually distinct (collapsed).
    pub resolved: bool,
    /// Whether the thread is outdated — its anchored line changed since the
    /// comment was written. The renderer labels it and shows
    /// `original_line` instead of a live anchor.
    pub outdated: bool,
    /// The thread's comments, in API order (oldest first).
    pub comments: Vec<RenderedComment>,
}

/// One model's bucket of anchored review threads — the per-model count
/// (the tooltip "N comments on this model") plus the threads themselves
/// (inline placement at their diff lines).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelCommentBucket {
    /// The model node-id (full id, e.g. `model.shop.orders`).
    pub model: String,
    /// The model's project-relative file path (its
    /// `original_file_path`, e.g. `models/orders.sql`). This is the join
    /// key the **renderer** matches on: the JS model payload carries `path`
    /// (not the node-id), so `renderModelCommentCount` / the navigation
    /// match `m.path === bucket.model_path`.
    pub model_path: String,
    /// The number of review threads anchored to this model (resolved + open
    /// + outdated-but-still-mapped). The per-model tooltip count.
    pub count: usize,
    /// The threads, ordered deterministically (by `(line, original_line)`)
    /// so the render is stable.
    pub threads: Vec<RenderedThread>,
}

/// The full per-model PR-comments render view (cute-dbt#419–#422).
///
/// `by_model` is the bucketed inline surface (one entry per model that
/// carries ≥1 thread, in node-id order — deterministic). `total` is the
/// report-wide thread count across **all** buckets plus the unanchored
/// list (the top-of-report navigation button's count). `unanchored`
/// carries threads whose file maps to no model node (path-not-in-diff or
/// an outdated thread on a non-model file) — surfaced honestly, never
/// dropped.
///
/// `Serialize`-only — an additive render payload, never round-tripped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CommentsView {
    /// Per-model buckets (node-id order). Each entry has ≥1 thread.
    pub by_model: Vec<ModelCommentBucket>,
    /// Threads whose file maps to no model node — surfaced report-wide so
    /// reviewer context is never lost. Empty ⇒ omitted from JSON.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unanchored: Vec<RenderedThread>,
    /// The report-wide thread total (every bucketed thread + every
    /// unanchored thread) — the top-of-report count button's number.
    pub total: usize,
}

impl CommentsView {
    /// Whether the view carries nothing to render (no bucketed thread and
    /// no unanchored thread). The cli treats this as "no PR-comment
    /// surface" — it passes `None` so the section emits zero bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_model.is_empty() && self.unanchored.is_empty()
    }
}

/// Convert one ingested [`PrCommentThread`] into its [`RenderedComment`]
/// rows — the render twin of the thread's `comments` vec.
fn rendered_comments(thread: &PrCommentThread) -> Vec<RenderedComment> {
    thread
        .comments
        .iter()
        .map(|c| RenderedComment {
            author: c.author.clone(),
            body: c.body.clone(),
        })
        .collect()
}

/// Build the project-relative-path → model-node-id index the grouping
/// joins on: every `model` node's
/// [`original_file_path`](crate::domain::Node::original_file_path) ↦ its
/// node-id. A model with no `original_file_path` contributes nothing (it
/// cannot be the target of a path-anchored comment). Deterministic by
/// construction (a `BTreeMap` keyed on the path).
fn model_paths(current: &Manifest) -> BTreeMap<String, String> {
    let mut index = BTreeMap::new();
    for (id, node) in current.nodes() {
        if node.resource_type() != "model" {
            continue;
        }
        if let Some(ofp) = node.original_file_path() {
            index.insert(ofp.to_owned(), id.as_str().to_owned());
        }
    }
    index
}

/// Group the ingested review threads into the per-model render view
/// (cute-dbt#419–#422).
///
/// Each thread is anchored with the **shipped**
/// [`anchor_comment_thread`] (cute-dbt#418) — passing the run's diff
/// `index`, `report_basis` (`None` to defer to GitHub's `isOutdated` flag
/// plus the within-hunk test, the report's posture — it tracks no basis
/// SHA), and the run's `project_to_repo_strip` (the same strip the diff
/// index was built with, so a sub-directory dbt project's repo-relative
/// thread path reconciles onto the project-relative diff/manifest keyspace). The
/// anchored result's path is then mapped back to the model node that owns
/// that file and bucketed; an unmappable path lands in
/// [`CommentsView::unanchored`].
///
/// Pure: borrows the parsed diff index + manifest, does no I/O.
///
/// # Arguments
///
/// - `threads` — the ingested GitHub review threads.
/// - `current` — the run's manifest (the path → model-node join source).
/// - `index` — the report's rendered diff (the anchor join source).
/// - `project_to_repo_strip` — the dbt project root relative to the repo
///   root (the same strip the diff index was built with), or `None` for a
///   repo-root project.
#[must_use]
pub fn group_comment_threads(
    threads: &[PrCommentThread],
    current: &Manifest,
    index: &crate::domain::pr_diff::NormalizedDiffIndex,
    project_to_repo_strip: Option<&Path>,
) -> CommentsView {
    let paths = model_paths(current);
    // The reverse id → path map so each finalized bucket can carry its
    // model's project-relative file path (the renderer's join key).
    let id_to_path: BTreeMap<String, String> = paths
        .iter()
        .map(|(p, id)| (id.clone(), p.clone()))
        .collect();
    // Deterministic per-model buckets, node-id ordered (BTreeMap key).
    let mut buckets: BTreeMap<String, Vec<RenderedThread>> = BTreeMap::new();
    let mut unanchored: Vec<RenderedThread> = Vec::new();

    for thread in threads {
        // Re-use the shipped anchoring — never re-anchor here (cute-dbt#418).
        let anchor = anchor_comment_thread(thread, index, None, project_to_repo_strip);
        let comments = rendered_comments(thread);
        match anchor {
            ThreadAnchor::Resolved(resolved) => {
                let model = paths.get(&resolved.path).cloned();
                let rendered = RenderedThread {
                    model: model.clone(),
                    path: resolved.path,
                    line: Some(resolved.line),
                    original_line: thread.original_line,
                    side: resolved.side,
                    within_hunk: resolved.within_hunk,
                    resolved: thread.is_resolved,
                    outdated: false,
                    comments,
                };
                push_thread(&mut buckets, &mut unanchored, rendered, model);
            }
            ThreadAnchor::Outdated {
                path,
                original_line,
            } => {
                let model = paths.get(&path).cloned();
                let rendered = RenderedThread {
                    model: model.clone(),
                    path,
                    line: None,
                    original_line,
                    side: thread.diff_side.clone(),
                    within_hunk: false,
                    resolved: thread.is_resolved,
                    outdated: true,
                    comments,
                };
                push_thread(&mut buckets, &mut unanchored, rendered, model);
            }
            ThreadAnchor::PathNotInDiff { path } => {
                // The file is not in the rendered diff at all → report-wide
                // (a model node may still own the path, but with no diff for
                // it there is no line to inline at, so it is unanchored).
                unanchored.push(RenderedThread {
                    model: None,
                    path,
                    line: None,
                    original_line: thread.original_line,
                    side: thread.diff_side.clone(),
                    within_hunk: false,
                    resolved: thread.is_resolved,
                    outdated: false,
                    comments,
                });
            }
        }
    }

    finalize(buckets, unanchored, &id_to_path)
}

/// Push a rendered thread into its model bucket, or the report-wide
/// unanchored list when it carries no model id.
fn push_thread(
    buckets: &mut BTreeMap<String, Vec<RenderedThread>>,
    unanchored: &mut Vec<RenderedThread>,
    rendered: RenderedThread,
    model: Option<String>,
) {
    match model {
        Some(id) => buckets.entry(id).or_default().push(rendered),
        None => unanchored.push(rendered),
    }
}

/// Assemble the [`CommentsView`] from the accumulated buckets + unanchored
/// list: sort each bucket's threads deterministically, compute the
/// per-model count + the report-wide total.
fn finalize(
    buckets: BTreeMap<String, Vec<RenderedThread>>,
    unanchored: Vec<RenderedThread>,
    id_to_path: &BTreeMap<String, String>,
) -> CommentsView {
    let mut by_model = Vec::with_capacity(buckets.len());
    let mut total = unanchored.len();
    for (model, mut threads) in buckets {
        // Deterministic thread order within a model: by (live line, then
        // original line). `None` sorts before `Some` so an outdated thread
        // (no live line) leads, which the renderer groups at the top.
        threads.sort_by(|a, b| {
            (a.line, a.original_line)
                .cmp(&(b.line, b.original_line))
                .then_with(|| a.path.cmp(&b.path))
        });
        total += threads.len();
        // The model's file path is the renderer's join key. Every thread in
        // a bucket shares it (they were bucketed by the model that owns the
        // path); fall back to the first thread's path if the id is somehow
        // absent from the index (never in practice — the bucket key came
        // from the index).
        let model_path = id_to_path
            .get(&model)
            .cloned()
            .or_else(|| threads.first().map(|t| t.path.clone()))
            .unwrap_or_default();
        by_model.push(ModelCommentBucket {
            model,
            model_path,
            count: threads.len(),
            threads,
        });
    }
    CommentsView {
        by_model,
        unanchored,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Checksum, DependsOn, Node, NodeConfig, NodeId};
    use crate::domain::pr_comment::PrComment;
    use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff};
    use std::collections::BTreeMap as Map;

    // ----- builders ------------------------------------------------------

    fn model_node(full_id: &str, ofp: &str) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            Some("select 1".to_owned()),
            DependsOn::default(),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            Map::new(),
        )
    }

    fn manifest_with(nodes: Vec<Node>) -> Manifest {
        let mut map = std::collections::HashMap::new();
        for node in nodes {
            map.insert(node.id().clone(), node);
        }
        Manifest::new(
            crate::domain::manifest::ManifestMetadata::new("v12"),
            map,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
    }

    fn hunk(new_start: usize, new_len: usize, removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            new_start,
            new_len,
            removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn index_for(files: Vec<(&str, Vec<Hunk>)>) -> crate::domain::pr_diff::NormalizedDiffIndex {
        let diff = PrDiff {
            files: files
                .into_iter()
                .map(|(path, hunks)| FileHunks {
                    path: path.to_owned(),
                    hunks,
                })
                .collect(),
            renames: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
        };
        crate::domain::pr_diff::NormalizedDiffIndex::new(&diff, None)
    }

    fn thread(path: &str, line: Option<u32>, side: DiffSide) -> PrCommentThread {
        PrCommentThread {
            path: path.to_owned(),
            line,
            original_line: line,
            diff_side: side,
            is_resolved: false,
            is_outdated: false,
            commit_oid: None,
            diff_hunk: None,
            comments: vec![PrComment {
                author: Some("octocat".to_owned()),
                body: "nit: rename this CTE".to_owned(),
            }],
        }
    }

    // ----- inline anchored thread ---------------------------------------

    #[test]
    fn an_anchored_thread_buckets_under_its_model_with_a_count() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        )]);
        let t = thread("models/orders.sql", Some(5), DiffSide::Right);

        let view = group_comment_threads(&[t], &manifest, &index, None);
        assert_eq!(view.total, 1);
        assert_eq!(view.by_model.len(), 1);
        let bucket = &view.by_model[0];
        assert_eq!(bucket.model, "model.shop.orders");
        assert_eq!(
            bucket.model_path, "models/orders.sql",
            "the bucket carries the model's file path — the renderer's join key"
        );
        assert_eq!(bucket.count, 1);
        let rt = &bucket.threads[0];
        assert_eq!(rt.model.as_deref(), Some("model.shop.orders"));
        assert_eq!(rt.line, Some(5));
        assert!(rt.within_hunk);
        assert!(!rt.outdated);
        assert!(!rt.resolved);
        assert_eq!(rt.comments.len(), 1);
        assert_eq!(rt.comments[0].author.as_deref(), Some("octocat"));
        assert!(view.unanchored.is_empty());
    }

    #[test]
    fn two_threads_on_the_same_model_count_two_and_sort_by_line() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 3, &["old"], &["n1", "n2", "n3"])],
        )]);
        let later = thread("models/orders.sql", Some(7), DiffSide::Right);
        let earlier = thread("models/orders.sql", Some(5), DiffSide::Right);

        // Pass the later thread FIRST to prove the sort, not insertion order.
        let view = group_comment_threads(&[later, earlier], &manifest, &index, None);
        assert_eq!(view.total, 2);
        let bucket = &view.by_model[0];
        assert_eq!(bucket.count, 2);
        assert_eq!(bucket.threads[0].line, Some(5));
        assert_eq!(bucket.threads[1].line, Some(7));
    }

    // ----- outdated thread ----------------------------------------------

    #[test]
    fn an_outdated_thread_buckets_under_its_model_with_no_live_line() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        )]);
        let mut t = thread("models/orders.sql", Some(5), DiffSide::Right);
        t.is_outdated = true;
        t.original_line = Some(12);

        let view = group_comment_threads(&[t], &manifest, &index, None);
        assert_eq!(view.total, 1);
        let rt = &view.by_model[0].threads[0];
        assert!(rt.outdated, "outdated flag is carried");
        assert_eq!(rt.line, None, "no live line on an outdated thread");
        assert_eq!(rt.original_line, Some(12), "original line carried");
        assert!(!rt.within_hunk);
    }

    // ----- resolved thread ----------------------------------------------

    #[test]
    fn a_resolved_thread_carries_its_resolved_flag() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        )]);
        let mut t = thread("models/orders.sql", Some(5), DiffSide::Right);
        t.is_resolved = true;

        let view = group_comment_threads(&[t], &manifest, &index, None);
        assert!(view.by_model[0].threads[0].resolved);
    }

    // ----- path not in diff (unanchored) --------------------------------

    #[test]
    fn a_thread_on_a_file_not_in_the_diff_is_unanchored() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        // The diff touches a DIFFERENT file.
        let index = index_for(vec![(
            "models/customers.sql",
            vec![hunk(1, 1, &[], &["x"])],
        )]);
        let t = thread("models/orders.sql", Some(5), DiffSide::Right);

        let view = group_comment_threads(&[t], &manifest, &index, None);
        assert_eq!(view.total, 1);
        assert!(view.by_model.is_empty(), "nothing inline");
        assert_eq!(view.unanchored.len(), 1);
        assert!(view.unanchored[0].model.is_none());
        assert_eq!(view.unanchored[0].path, "models/orders.sql");
    }

    #[test]
    fn a_thread_on_a_non_model_path_is_unanchored_even_when_in_the_diff() {
        // The diff includes the file, the thread resolves — but no model
        // node owns that path, so there's no model to bucket under.
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![("macros/util.sql", vec![hunk(2, 1, &[], &["x"])])]);
        let t = thread("macros/util.sql", Some(2), DiffSide::Right);

        let view = group_comment_threads(&[t], &manifest, &index, None);
        assert_eq!(view.total, 1);
        assert!(view.by_model.is_empty());
        assert_eq!(view.unanchored.len(), 1);
        assert_eq!(view.unanchored[0].path, "macros/util.sql");
        assert!(view.unanchored[0].model.is_none());
    }

    // ----- empty / no comments ------------------------------------------

    #[test]
    fn no_threads_yields_an_empty_view() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1"])],
        )]);
        let view = group_comment_threads(&[], &manifest, &index, None);
        assert!(view.is_empty());
        assert_eq!(view.total, 0);
    }

    // ----- ghost author -------------------------------------------------

    #[test]
    fn a_ghost_author_comment_renders_as_none() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        )]);
        let mut t = thread("models/orders.sql", Some(5), DiffSide::Right);
        t.comments = vec![PrComment {
            author: None,
            body: "ghost said".to_owned(),
        }];

        let view = group_comment_threads(&[t], &manifest, &index, None);
        let c = &view.by_model[0].threads[0].comments[0];
        assert_eq!(c.author, None);
        assert_eq!(c.body, "ghost said");
    }

    // ----- sub-directory project strip ----------------------------------

    #[test]
    fn the_project_root_strip_reconciles_a_subdirectory_thread_path() {
        // Manifest + diff index keys are project-relative; the GitHub thread
        // path is repo-relative. The strip must map them. (The shipped
        // anchor module already proves the strip — this proves the GROUPING
        // join still finds the model after the strip.)
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        )]);
        let t = thread("dbt_sub/models/orders.sql", Some(5), DiffSide::Right);

        let view = group_comment_threads(&[t], &manifest, &index, Some(Path::new("dbt_sub")));
        assert_eq!(view.by_model.len(), 1);
        assert_eq!(view.by_model[0].model, "model.shop.orders");
    }

    // ----- multiple models ----------------------------------------------

    #[test]
    fn threads_across_models_bucket_separately_in_node_id_order() {
        let manifest = manifest_with(vec![
            model_node("model.shop.zeta", "models/zeta.sql"),
            model_node("model.shop.alpha", "models/alpha.sql"),
        ]);
        let index = index_for(vec![
            ("models/zeta.sql", vec![hunk(1, 1, &[], &["z"])]),
            ("models/alpha.sql", vec![hunk(1, 1, &[], &["a"])]),
        ]);
        let view = group_comment_threads(
            &[
                thread("models/zeta.sql", Some(1), DiffSide::Right),
                thread("models/alpha.sql", Some(1), DiffSide::Right),
            ],
            &manifest,
            &index,
            None,
        );
        assert_eq!(view.total, 2);
        assert_eq!(view.by_model.len(), 2);
        // node-id order: alpha < zeta.
        assert_eq!(view.by_model[0].model, "model.shop.alpha");
        assert_eq!(view.by_model[1].model, "model.shop.zeta");
    }

    // ----- serde ---------------------------------------------------------

    #[test]
    fn comments_view_serializes_with_expected_keys() {
        let manifest = manifest_with(vec![model_node("model.shop.orders", "models/orders.sql")]);
        let index = index_for(vec![(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        )]);
        let view = group_comment_threads(
            &[thread("models/orders.sql", Some(5), DiffSide::Right)],
            &manifest,
            &index,
            None,
        );
        let json = serde_json::to_string(&view).expect("serialize CommentsView");
        assert!(json.contains("\"by_model\""), "{json}");
        assert!(json.contains("\"total\":1"), "{json}");
        assert!(json.contains("\"count\":1"), "{json}");
        // An empty `unanchored` is skipped (byte-minimal payload).
        assert!(!json.contains("\"unanchored\""), "{json}");
    }
}
