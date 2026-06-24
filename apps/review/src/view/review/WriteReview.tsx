// WriteReview — the portable write-review surface (V1 / cute-dbt#495). The
// council "write-back = export" decision made executable: cute-dbt NEVER posts
// to GitHub. This surface builds a PORTABLE payload (a `gh api …` command + the
// copy-JSON request body) the HOST runs in its own shell — the local-first /
// zero-egress invariant + the never-a-false-claim contract.
//
// The component is a THIN view over the pure `buildReviewPayload` (passed in as
// `onBuild`): it owns the verdict + summary-body inputs, re-derives the payload
// on every change, and renders it. There is NO network code here — no fetch, no
// gh call — by construction. `onPublish` runs the LOCAL bookkeeping (pending →
// published + the checkpoint advance) via the review slice; it does not post.
//
// LAYER: view (imports domain only).
import React, { useMemo, useState } from "react";
import type { ReviewPayload, Verdict } from "../../domain/review/review-machine";

export interface WriteReviewProps {
  /** the REAL pending-draft count (from the review slice — never fabricated). */
  draftCount: number;
  /** build the portable payload for a (verdict, body) — the pure machine fn. */
  onBuild: (verdict: Verdict, body: string) => ReviewPayload;
  /** record the LOCAL review (pending→published + checkpoint advance); never posts. */
  onPublish: (verdict: Verdict, body: string) => void;
  /** close the overlay. */
  onClose: () => void;
}

const VERDICTS: { id: Verdict; label: string }[] = [
  { id: "approve", label: "Approve" },
  { id: "request", label: "Request changes" },
  { id: "comment", label: "Comment" },
];

export function WriteReview({ draftCount, onBuild, onPublish, onClose }: WriteReviewProps): React.ReactElement {
  const [verdict, setVerdict] = useState<Verdict>("comment"); // safe non-blocking default
  const [body, setBody] = useState("");

  // re-derive the portable payload on every verdict/body change (pure).
  const payload = useMemo(() => onBuild(verdict, body), [onBuild, verdict, body]);

  return (
    <div
      data-testid="write-review"
      role="dialog"
      aria-label="Write review"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6"
    >
      <div className="w-full max-w-2xl overflow-hidden rounded-lg border border-zinc-700 bg-zinc-900 shadow-2xl">
        <div className="flex items-center gap-2 border-b border-zinc-800 px-4 py-3">
          <strong className="text-sm text-zinc-100">Write review</strong>
          <span
            data-testid="write-review-draftcount"
            className="rounded bg-zinc-800 px-2 py-0.5 font-mono text-[11px] text-zinc-400"
          >
            {draftCount} pending {draftCount === 1 ? "comment" : "comments"}
          </span>
          <span className="flex-1" />
          <button
            data-testid="write-review-close"
            onClick={onClose}
            className="rounded px-2 py-1 text-[12px] text-zinc-400 hover:bg-zinc-800 hover:text-zinc-100"
          >
            Close (Esc)
          </button>
        </div>

        <div className="space-y-3 px-4 py-3">
          {/* verdict picker */}
          <div className="flex items-center gap-1.5" role="radiogroup" aria-label="Review verdict">
            {VERDICTS.map((v) => (
              <button
                key={v.id}
                data-testid={`write-review-verdict-${v.id}`}
                data-active={verdict === v.id}
                aria-checked={verdict === v.id}
                role="radio"
                onClick={() => setVerdict(v.id)}
                className={
                  "rounded-md border px-2.5 py-1 text-[12px] font-medium " +
                  (verdict === v.id
                    ? "border-sky-400 bg-sky-500/10 text-sky-200"
                    : "border-zinc-700 text-zinc-300 hover:bg-zinc-800")
                }
              >
                {v.label}
              </button>
            ))}
          </div>

          {/* the review-level summary body */}
          <textarea
            data-testid="write-review-body"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            placeholder="Leave a review summary (optional)…"
            className="block min-h-[4rem] w-full resize-y rounded-md border border-zinc-700 bg-zinc-950 px-2.5 py-2 font-sans text-[13px] text-zinc-200 focus:outline-none"
          />

          {/* the PORTABLE payload — copy-JSON + the gh-CLI command the HOST runs. */}
          <div className="rounded-md border border-zinc-800 bg-zinc-950 p-3">
            <div className="mb-1 text-[11px] uppercase tracking-wide text-zinc-500">
              Portable review payload
            </div>
            <p data-testid="write-review-note" className="mb-2 text-[11px] text-amber-300/80">
              cute-dbt never posts to GitHub. Copy this payload and run the command
              yourself — your review stays local until <em>you</em> send it.
            </p>
            <label className="mb-1 block text-[10px] uppercase tracking-wide text-zinc-500">
              gh command
            </label>
            <code
              data-testid="write-review-gh-command"
              className="mb-2 block overflow-x-auto whitespace-pre rounded bg-zinc-900 px-2 py-1.5 font-mono text-[11px] text-emerald-300"
            >
              {payload.ghCommand}
            </code>
            <label className="mb-1 block text-[10px] uppercase tracking-wide text-zinc-500">
              request body (copy as the command stdin)
            </label>
            <textarea
              data-testid="write-review-json"
              readOnly
              value={payload.json}
              className="block h-40 w-full resize-y rounded bg-zinc-900 px-2 py-1.5 font-mono text-[11px] text-zinc-300 focus:outline-none"
            />
          </div>
        </div>

        <div className="flex items-center gap-2 border-t border-zinc-800 bg-zinc-900/60 px-4 py-3">
          <span className="flex-1 text-[11px] text-zinc-500">
            Publish records the review locally + advances your review checkpoint.
          </span>
          <button
            data-testid="write-review-publish"
            onClick={() => onPublish(verdict, body)}
            className="rounded-md bg-sky-500 px-3 py-1.5 text-[12px] font-medium text-white hover:bg-sky-400"
          >
            Publish (local)
          </button>
        </div>
      </div>
    </div>
  );
}
