// Inline comment-thread renderer mounted into Pierre's per-line annotation slot.
// The full RenderedThread is the annotation metadata. Bodies render as PLAIN TEXT
// (React escapes by default — never dangerouslySetInnerHTML).
import React from "react";
import type { RenderedThread } from "../domain/context-data";

export function CommentThread({ thread }: { thread: RenderedThread }): React.ReactElement {
  const sideLabel = thread.side === "Left" ? "old / deletions" : "new / additions";
  return (
    <div
      data-testid="comment-thread"
      data-thread-side={thread.side}
      style={{
        border: "1px solid #44485a",
        borderLeft: `3px solid ${thread.side === "Left" ? "#f7768e" : "#7aa2f7"}`,
        borderRadius: 6,
        background: "rgba(122,162,247,0.06)",
        margin: "4px 0 8px 32px",
        padding: "6px 10px",
        font: "13px system-ui, sans-serif",
        maxWidth: 760,
      }}
    >
      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 4 }}>
        <span data-testid="thread-side-badge" style={{ fontSize: 11, opacity: 0.7 }}>
          line {thread.line} · {sideLabel}
        </span>
        {thread.resolved && (
          <span
            data-testid="thread-resolved-badge"
            style={{
              fontSize: 10, background: "#9ece6a", color: "#1a1b26",
              borderRadius: 10, padding: "1px 8px", fontWeight: 600,
            }}
          >
            resolved
          </span>
        )}
      </div>
      {thread.comments.map((c, i) => (
        <div key={i} style={{ marginTop: i === 0 ? 0 : 6 }}>
          <b style={{ color: c.author == null ? "#9aa0b5" : "inherit" }}>{c.author ?? "ghost"}</b>{" "}
          <span style={{ whiteSpace: "pre-wrap" }}>{c.body}</span>
        </div>
      ))}
    </div>
  );
}
