// The PR page surfaces (S9 / cute-dbt#501) — three view-layer components rendering
// the pure `domain/pr-page` aggregation: the OVERVIEW (number/title/url + the
// changed-model summary + removed models), the FILES list (one navigable row per
// changed file with its comment counts), and the comment TIMELINE (the per-model +
// unanchored threads, grouped + ordered) WITH an HONEST "no commit/review/CI feed"
// panel for the temporal data the spine does not emit yet.
//
// HONESTY: these components render ONLY what the aggregation carries. The timeline
// NEVER fabricates a commit, a review, a check, or a timestamp — when
// `feed.present === false` it renders the documented "not in this context" panel
// (the tracked T2 spine gap), not an invented event stream.
//
// LAYER: view (imports domain; never chrome). Server-renderable (no browser-only
// APIs) so the Vitest `renderToStaticMarkup` view tests + the Playwright e2e both
// exercise it.
import React from "react";
import type {
  PrOverview as PrOverviewModel,
  PrFilesView,
  PrFileRow,
  CommentTimeline,
  TimelineGroup,
  TimelineThread,
  PrTimelineFeed,
  PrChangeRole,
} from "../../domain/pr-page";

// ── shared change-chip vocabulary ────────────────────────────────────────────
const CHANGE_TONE: Record<PrChangeRole, { bg: string; fg: string; label: string }> = {
  new: { bg: "rgba(158,206,106,0.16)", fg: "#9ece6a", label: "new" },
  added: { bg: "rgba(158,206,106,0.16)", fg: "#9ece6a", label: "added" },
  modified: { bg: "rgba(122,162,247,0.16)", fg: "#7aa2f7", label: "modified" },
  deleted: { bg: "rgba(247,118,142,0.16)", fg: "#f7768e", label: "removed" },
  context: { bg: "rgba(108,112,134,0.16)", fg: "#6c7086", label: "context" },
};

function ChangeChip({ change }: { change: PrChangeRole }): React.ReactElement {
  const t = CHANGE_TONE[change];
  return (
    <span
      data-testid="change-chip"
      data-change={change}
      style={{
        fontSize: 10, textTransform: "uppercase", letterSpacing: "0.04em",
        borderRadius: 4, padding: "1px 6px", fontFamily: "ui-monospace, monospace",
        background: t.bg, color: t.fg,
      }}
    >
      {t.label}
    </span>
  );
}

// ── PR OVERVIEW ──────────────────────────────────────────────────────────────

function Stat({ n, label, testid }: { n: number; label: string; testid: string }): React.ReactElement {
  return (
    <div data-testid={testid} data-count={n} style={{ display: "flex", flexDirection: "column" }}>
      <span style={{ fontSize: 18, fontFamily: "ui-monospace, monospace", color: "#c0caf5", lineHeight: 1 }}>{n}</span>
      <span style={{ fontSize: 10, textTransform: "uppercase", letterSpacing: "0.06em", color: "#6c7086", marginTop: 4 }}>{label}</span>
    </div>
  );
}

export function PrOverview({ overview }: { overview: PrOverviewModel }): React.ReactElement {
  const c = overview.counts;
  return (
    <div data-testid="view-pr-overview" className="min-w-0 flex-1 overflow-auto p-6" style={{ maxWidth: 820, margin: "0 auto" }}>
      {/* identity — number/title/url, all read from the real pr_ref */}
      <h1 data-testid="pr-title" style={{ fontSize: 19, fontWeight: 600, color: "#c0caf5", lineHeight: 1.35 }}>
        {overview.url ? (
          <a
            data-testid="pr-number-link"
            href={overview.url}
            target="_blank"
            rel="noopener noreferrer"
            style={{ color: "#6c7086", fontFamily: "ui-monospace, monospace", fontWeight: 400, marginRight: 8, textDecoration: "none" }}
          >
            PR #{overview.number} ↗
          </a>
        ) : (
          <span data-testid="pr-number" style={{ color: "#6c7086", fontFamily: "ui-monospace, monospace", fontWeight: 400, marginRight: 8 }}>
            PR #{overview.number}
          </span>
        )}
        {overview.title || <span style={{ color: "#6c7086", fontStyle: "italic" }}>untitled</span>}
      </h1>

      {overview.author && (
        <div data-testid="pr-author" style={{ marginTop: 6, fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
          opened by {overview.author}
        </div>
      )}

      {/* changed-model summary — derived from the PR-scope DAG state taxonomy */}
      {overview.hasDag ? (
        <section data-testid="pr-change-summary" className="rounded-lg border" style={{ marginTop: 20, borderColor: "#2a2b36", background: "rgba(26,27,38,0.5)", padding: 16 }}>
          <div style={{ fontSize: 10, textTransform: "uppercase", letterSpacing: "0.06em", color: "#6c7086", marginBottom: 12 }}>
            Changed-model summary
          </div>
          <div style={{ display: "flex", gap: 28, flexWrap: "wrap" }}>
            <Stat testid="count-changed" n={c.changed} label="models changed" />
            <Stat testid="count-modified" n={c.modified} label="modified" />
            <Stat testid="count-new" n={c.new} label="new" />
            <Stat testid="count-added" n={c.added} label="added" />
            <Stat testid="count-deleted" n={c.deleted} label="deleted" />
            <Stat testid="count-removed" n={c.removed} label="removed (file)" />
            <Stat testid="count-context" n={c.context} label="lineage context" />
          </div>
        </section>
      ) : (
        <p data-testid="pr-no-dag" style={{ marginTop: 20, fontSize: 13, color: "#6c7086" }}>
          No PR-scope DAG in this context.
        </p>
      )}

      {/* removed models — the node-less list, surfaced distinctly */}
      {overview.removedModels.length > 0 && (
        <section data-testid="pr-removed-models" className="rounded-lg border" style={{ marginTop: 16, borderColor: "rgba(247,118,142,0.3)", padding: 14 }}>
          <div style={{ fontSize: 10, textTransform: "uppercase", letterSpacing: "0.06em", color: "#f7768e", marginBottom: 8 }}>
            Removed models · {overview.removedModels.length}
          </div>
          <ul style={{ margin: 0, padding: 0, listStyle: "none", display: "flex", flexDirection: "column", gap: 4 }}>
            {overview.removedModels.map((p) => (
              <li key={p} data-testid="removed-model" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#f7768e" }}>
                <span style={{ marginRight: 6 }}>−</span>{p}
              </li>
            ))}
          </ul>
        </section>
      )}

      {/* description — the real pr_ref body when the spine carries it */}
      <section data-testid="pr-description" style={{ marginTop: 20 }}>
        <div style={{ fontSize: 10, textTransform: "uppercase", letterSpacing: "0.06em", color: "#6c7086", marginBottom: 8 }}>Description</div>
        {overview.body ? (
          <p data-testid="pr-body" style={{ fontSize: 13, color: "#a9b1d6", whiteSpace: "pre-wrap", lineHeight: 1.6, margin: 0 }}>{overview.body}</p>
        ) : (
          <p data-testid="pr-no-body" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086", margin: 0 }}>
            no description in this context.
          </p>
        )}
      </section>
    </div>
  );
}

// ── PR FILES ─────────────────────────────────────────────────────────────────

function FileRow({ row, onOpen }: { row: PrFileRow; onOpen?: (id: string) => void }): React.ReactElement {
  const clickable = row.navigable && !!onOpen;
  return (
    <li
      data-testid="pr-file-row"
      data-file={row.id}
      data-navigable={row.navigable}
      style={{ display: "flex", alignItems: "center", gap: 10, padding: "6px 10px", borderBottom: "1px solid #1f2030" }}
    >
      <ChangeChip change={row.change} />
      {clickable ? (
        <button
          data-testid="pr-file-open"
          onClick={() => onOpen!(row.id)}
          style={{ flex: 1, minWidth: 0, textAlign: "left", background: "none", border: "none", padding: 0, cursor: "pointer", fontFamily: "ui-monospace, monospace", fontSize: 12, color: "#c0caf5", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
          title={`Open ${row.id} in the Models review surface`}
        >
          {row.path}
        </button>
      ) : (
        <span style={{ flex: 1, minWidth: 0, fontFamily: "ui-monospace, monospace", fontSize: 12, color: "#6c7086", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {row.path}
        </span>
      )}
      {(row.linesAdded > 0 || row.linesRemoved > 0) && (
        <span data-testid="pr-file-stat" style={{ fontFamily: "ui-monospace, monospace", fontSize: 11, whiteSpace: "nowrap" }}>
          {row.linesAdded > 0 && <span style={{ color: "#9ece6a" }}>+{row.linesAdded}</span>}
          {row.linesRemoved > 0 && <span style={{ color: "#f7768e", marginLeft: 4 }}>−{row.linesRemoved}</span>}
        </span>
      )}
      {row.threadTotal > 0 && (
        <span
          data-testid="pr-file-threads"
          data-resolved={row.threadResolved}
          data-total={row.threadTotal}
          style={{ fontFamily: "ui-monospace, monospace", fontSize: 11, color: row.threadResolved === row.threadTotal ? "#9ece6a" : "#6c7086", whiteSpace: "nowrap" }}
          title="resolved / total conversation threads"
        >
          💬 {row.threadResolved}/{row.threadTotal}
        </span>
      )}
    </li>
  );
}

export function PrFiles({ files, onOpen }: { files: PrFilesView; onOpen?: (id: string) => void }): React.ReactElement {
  return (
    <div data-testid="view-pr-files" className="min-w-0 flex-1 overflow-auto p-6" style={{ maxWidth: 920, margin: "0 auto" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12 }}>
        <span data-testid="pr-files-count" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
          {files.rows.length} changed file{files.rows.length === 1 ? "" : "s"}
        </span>
      </div>
      {files.rows.length > 0 ? (
        <ul className="rounded-lg border" style={{ margin: 0, padding: 0, listStyle: "none", borderColor: "#2a2b36", overflow: "hidden" }}>
          {files.rows.map((r) => (
            <FileRow key={`${r.change}:${r.path}`} row={r} onOpen={onOpen} />
          ))}
        </ul>
      ) : (
        <p data-testid="pr-files-empty" style={{ fontSize: 13, color: "#6c7086" }}>
          No changed files in this context.
        </p>
      )}
    </div>
  );
}

// ── PR COMMENT TIMELINE ──────────────────────────────────────────────────────

function ThreadCard({ thread }: { thread: TimelineThread }): React.ReactElement {
  const sideLabel = thread.side === "Left" ? "old / deletions" : "new / additions";
  return (
    <div
      data-testid="timeline-thread"
      data-resolved={thread.resolved}
      data-outdated={thread.outdated}
      style={{
        border: "1px solid #2a2b36",
        borderLeft: `3px solid ${thread.side === "Left" ? "#f7768e" : "#7aa2f7"}`,
        borderRadius: 6, background: "rgba(122,162,247,0.04)", padding: "8px 12px",
      }}
    >
      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 6 }}>
        <span style={{ fontSize: 11, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
          {thread.line != null ? `line ${thread.line} · ${sideLabel}` : "outdated · was on an old line"}
        </span>
        {thread.resolved && (
          <span data-testid="thread-resolved" style={{ fontSize: 10, background: "#9ece6a", color: "#1a1b26", borderRadius: 10, padding: "1px 8px", fontWeight: 600 }}>
            resolved
          </span>
        )}
        {thread.outdated && (
          <span data-testid="thread-outdated" style={{ fontSize: 10, color: "#e0af68", border: "1px solid rgba(224,175,104,0.4)", borderRadius: 10, padding: "1px 8px" }}>
            outdated
          </span>
        )}
        <span style={{ flex: 1 }} />
        <span style={{ fontSize: 10, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
          {thread.commentCount} comment{thread.commentCount === 1 ? "" : "s"}
        </span>
      </div>
      {thread.comments.map((c, i) => (
        <div key={i} data-testid="timeline-comment" style={{ paddingTop: i > 0 ? 6 : 0, marginTop: i > 0 ? 6 : 0, borderTop: i > 0 ? "1px solid #1f2030" : undefined }}>
          <div style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", fontWeight: 600, color: "#c0caf5", marginBottom: 2 }}>
            {c.author ?? <span style={{ fontStyle: "italic", color: "#6c7086" }}>ghost</span>}
          </div>
          {/* PLAIN TEXT — React escapes; never dangerouslySetInnerHTML. */}
          <div data-testid="timeline-comment-body" style={{ fontSize: 13, color: "#a9b1d6", whiteSpace: "pre-wrap", lineHeight: 1.5 }}>
            {c.body}
          </div>
        </div>
      ))}
    </div>
  );
}

function Group({ group, onOpen }: { group: TimelineGroup; onOpen?: (id: string) => void }): React.ReactElement {
  const clickable = group.model != null && !!onOpen;
  return (
    <section data-testid="timeline-group" data-model={group.model ?? ""}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
        {group.model != null ? (
          clickable ? (
            <button
              data-testid="timeline-group-open"
              onClick={() => onOpen!(group.model!)}
              style={{ background: "none", border: "none", padding: 0, cursor: "pointer", fontSize: 13, fontFamily: "ui-monospace, monospace", fontWeight: 600, color: "#7aa2f7" }}
              title={`Open ${group.model} in the Models review surface`}
            >
              {group.model}
            </button>
          ) : (
            <span style={{ fontSize: 13, fontFamily: "ui-monospace, monospace", fontWeight: 600, color: "#c0caf5" }}>{group.model}</span>
          )
        ) : (
          <span data-testid="timeline-group-unanchored" style={{ fontSize: 13, fontFamily: "ui-monospace, monospace", fontWeight: 600, color: "#e0af68" }}>
            unanchored (file-level / project)
          </span>
        )}
        <span style={{ fontSize: 11, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
          {group.threads.length} thread{group.threads.length === 1 ? "" : "s"} · {group.commentCount} comment{group.commentCount === 1 ? "" : "s"}
          {group.resolvedCount > 0 ? ` · ${group.resolvedCount} resolved` : ""}
        </span>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 18 }}>
        {group.threads.map((t, i) => (
          <ThreadCard key={`${group.model ?? "_"}:${t.path ?? ""}:${t.line ?? "x"}:${i}`} thread={t} />
        ))}
      </div>
    </section>
  );
}

/** The HONEST temporal-feed panel — the commit/review/CI feed the spine does NOT
 *  emit. It renders WHAT IS REAL (the reviewer count + the checks summary when the
 *  context carries them) and an explicit "not in this context" note for the event
 *  stream, with the tracked T2 spine-gap link. NEVER a fabricated commit/check. */
function FeedPanel({ feed }: { feed: PrTimelineFeed }): React.ReactElement {
  return (
    <section
      data-testid="pr-timeline-feed"
      data-present={feed.present}
      className="rounded-lg border"
      style={{ borderColor: "rgba(224,175,104,0.35)", background: "rgba(224,175,104,0.05)", padding: 14, marginBottom: 20 }}
    >
      <div style={{ fontSize: 10, textTransform: "uppercase", letterSpacing: "0.06em", color: "#e0af68", marginBottom: 8 }}>
        Commit · review · CI feed
      </div>
      {feed.present ? (
        <p style={{ fontSize: 13, color: "#a9b1d6", margin: 0 }}>Temporal feed present.</p>
      ) : (
        <p data-testid="feed-spine-gap" style={{ fontSize: 13, color: "#a9b1d6", margin: 0, lineHeight: 1.5 }}>
          The commit / review / CI-check timeline is <strong>not in this context</strong>. cute-dbt&rsquo;s
          spine emits the PR comments, the changed-model DAG, and the PR reference — but not the
          per-commit / per-review / per-check event stream. This is a tracked spine gap
          (cute-dbt#508 · §B6); it is rendered honestly empty rather than fabricated.
        </p>
      )}
      <div style={{ display: "flex", gap: 18, marginTop: 10, flexWrap: "wrap" }}>
        <span data-testid="feed-reviewers" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
          {feed.reviewerCount} reviewer{feed.reviewerCount === 1 ? "" : "s"} in context
        </span>
        {feed.checks ? (
          <span data-testid="feed-checks" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
            checks: <span style={{ color: "#9ece6a" }}>{feed.checks.passed} passing</span>
            {feed.checks.failed > 0 && <span style={{ color: "#f7768e" }}> · {feed.checks.failed} failing</span>}
            {feed.checks.pending > 0 && <span style={{ color: "#e0af68" }}> · {feed.checks.pending} pending</span>}
          </span>
        ) : (
          <span data-testid="feed-no-checks" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
            no CI-check summary in context
          </span>
        )}
      </div>
    </section>
  );
}

export function PrTimeline({
  timeline,
  feed,
  onOpen,
}: {
  timeline: CommentTimeline;
  feed: PrTimelineFeed;
  onOpen?: (id: string) => void;
}): React.ReactElement {
  return (
    <div data-testid="view-pr-timeline" className="min-w-0 flex-1 overflow-auto p-6" style={{ maxWidth: 820, margin: "0 auto" }}>
      <FeedPanel feed={feed} />

      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
        <h2 style={{ fontSize: 13, fontWeight: 600, color: "#c0caf5", margin: 0 }}>Conversation</h2>
        <span data-testid="timeline-total" style={{ fontSize: 11, fontFamily: "ui-monospace, monospace", color: "#6c7086" }}>
          {timeline.total} comment{timeline.total === 1 ? "" : "s"} · {timeline.threadTotal} thread{timeline.threadTotal === 1 ? "" : "s"}
        </span>
      </div>

      {timeline.hasComments ? (
        timeline.groups.map((g) => <Group key={g.model ?? "_unanchored"} group={g} onOpen={onOpen} />)
      ) : (
        <p data-testid="timeline-empty" style={{ fontSize: 13, color: "#6c7086" }}>
          No conversation in this context.
        </p>
      )}
    </div>
  );
}
