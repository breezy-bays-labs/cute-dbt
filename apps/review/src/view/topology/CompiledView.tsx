// CompiledView — the topology code pane (S6b). The VERBATIM-port of the
// prototype diff.js CompiledView/FileView, wired to the §3a source-map spine: a
// line-numbered, Shiki-highlighted listing that
//   • TINTS the selected node/zone's span (the `span` prop, compiled or raw
//     coords) so the DAG selection is visible in the code,
//   • marks the keyboard line CURSOR (the reverse-sync end),
//   • DIRECT-scrolls the active block into view + RING-FLASHES it when `scrollKey`
//     bumps (the forward sync's scroll nonce — a DAG node was picked), and
//   • renders an HONEST-EMPTY state when there is no source (a model without a
//     code_map spine) — never a fabricated listing.
//
// The two visible ends of the bidirectional CTE⇄code sync live here; the pure S6a
// machine computes the cursor/scroll/selection, this pane reflects them.
//
// LAYER: view (renders domain facts onto the DOM; imports domain only).
import React, { useEffect, useMemo, useRef, useState } from "react";
import { highlightLines, type CodeLineTokens } from "../../domain/code-lines";
import type { CodeLang } from "../../domain/code-highlighter";

/** A tinted block span — the renderer reads only the 1-based start/end LINE. */
export interface SpanLines {
  start: { line: number };
  end: { line: number };
}

export interface CompiledViewProps {
  /** the source text (compiled SQL, or the raw file). Empty ⇒ honest-empty. */
  text: string;
  lang: CodeLang;
  shiki: string;
  /** the selected node/zone's block span (tinted); null when nothing selected. */
  span?: SpanLines | null;
  /** the 1-based keyboard line cursor (reverse-sync end); null when none. */
  cursorLine?: number | null;
  /** a monotonic nonce — bumping it scrolls the active block into view + flashes
   *  it. The forward sync's scrollNonce feeds this (a DAG node was picked). */
  scrollKey?: number;
  /** ring-flash the active block on a scrollKey bump (the forward-sync signal). */
  flash?: boolean;
  /** test/measure hook to bypass the async highlight (renders plain rows). */
  noHighlight?: boolean;
}

/**
 * Scroll the nearest scrollable ancestor so `row` is visible. DIRECT scrollTop
 * assignment — NOT `scrollIntoView({behavior:"smooth"})`, which the preview iframe
 * (and many headless contexts) silently drop, leaving the block off-screen. Walks
 * UP to the first ancestor that actually overflows (the topology shelf nests
 * several overflow-auto containers, where own-ref scrolling misses).
 */
function scrollRowIntoView(row: HTMLElement | null, pad: number): void {
  if (!row) return;
  let sc: HTMLElement | null = row.parentElement;
  while (sc && sc.scrollHeight <= sc.clientHeight + 2) sc = sc.parentElement;
  if (!sc) return;
  const rt = row.getBoundingClientRect();
  const st = sc.getBoundingClientRect();
  if (rt.top >= st.top + pad && rt.bottom <= st.bottom - pad) return; // already comfortably in view
  sc.scrollTop = rt.top - st.top + sc.scrollTop - pad; // DIRECT (no smooth — preview iframe drops it)
}

export function CompiledView({
  text,
  lang,
  shiki,
  span,
  cursorLine,
  scrollKey = 0,
  flash = false,
  noHighlight = false,
}: CompiledViewProps): React.ReactElement {
  const lines = useMemo(() => String(text || "").split("\n"), [text]);
  const [tokens, setTokens] = useState<CodeLineTokens[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const spanRef = useRef<HTMLTableRowElement | null>(null);
  const cursorRef = useRef<HTMLTableRowElement | null>(null);
  const flashRef = useRef<{ el: HTMLElement | null; t: ReturnType<typeof setTimeout> | null }>({ el: null, t: null });

  const s = span?.start ? span.start.line : null;
  const e = span?.end ? span.end.line : null;

  // fetch per-line tokens (loud-fail → visible banner, never silent plain text).
  useEffect(() => {
    if (noHighlight || !text) {
      setTokens(null);
      return;
    }
    let cancelled = false;
    setTokens(null);
    setError(null);
    highlightLines(text, lang, shiki)
      .then((rows) => {
        if (!cancelled) setTokens(rows);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [text, lang, shiki, noHighlight]);

  // a node/zone pick (scrollKey bump) → DIRECT-scroll the block's first line into
  // view + RING-FLASH it. Keyed ONLY on scrollKey so re-renders don't re-scroll.
  useEffect(() => {
    const row = cursorRef.current || spanRef.current;
    scrollRowIntoView(row, 72);
    if (flash && row) {
      if (flashRef.current.el) {
        flashRef.current.el.classList.remove("kbd-ring");
        if (flashRef.current.t) clearTimeout(flashRef.current.t);
      }
      row.classList.add("kbd-ring");
      flashRef.current = { el: row, t: setTimeout(() => row.classList.remove("kbd-ring"), 1500) };
    }
    // Keyed ONLY on scrollKey by design: the forward-sync scroll/flash fires once
    // per genuine node pick (the scroll nonce), not on every re-render. (No
    // react-hooks/exhaustive-deps plugin is configured for this app.)
  }, [scrollKey]);

  // cursor-follow: keep the keyboard cursor line on-screen as ↑↓ moves it.
  useEffect(() => {
    scrollRowIntoView(cursorRef.current, 48);
  }, [cursorLine]);

  if (error) {
    return (
      <div
        data-testid="compiled-view-error"
        style={{ color: "#f7768e", font: "13px system-ui", padding: 12, border: "1px solid #f7768e", borderRadius: 8 }}
      >
        Code highlight failed ({shiki}): {error}
      </div>
    );
  }

  // HONEST-EMPTY: no source text ⇒ the source-map spine (code_map) isn't present
  // in this fixture. Never a fabricated listing, never a claimed sync.
  if (!text) {
    return (
      <div
        data-testid="compiled-view-empty"
        style={{ padding: 16, font: "13px system-ui", color: "#6c7086", fontStyle: "italic" }}
      >
        No source on this model — the source-map spine (<span style={{ fontFamily: "ui-monospace, monospace", fontStyle: "normal" }}>code_map</span>) isn't present in this fixture.
      </div>
    );
  }

  return (
    <div
      data-testid="compiled-view"
      style={{ height: "100%", maxHeight: 360, overflow: "auto", border: "1px solid #2a2b36", borderRadius: 8, background: "#16161e" }}
    >
      <table style={{ width: "100%", borderCollapse: "collapse", font: "12px ui-monospace, SFMono-Regular, Menlo, monospace" }}>
        <tbody style={{ verticalAlign: "top" }}>
          {lines.map((ln, i) => {
            const n = i + 1;
            const inSpan = s != null && e != null && n >= s && n <= e;
            const isCursor = cursorLine === n;
            const row = tokens?.[i];
            return (
              <tr
                key={i}
                data-testid="code-line"
                data-line={n}
                data-in-span={inSpan ? "true" : "false"}
                data-cursor={isCursor ? "true" : "false"}
                ref={(el) => {
                  if (n === s) spanRef.current = el;
                  if (isCursor) cursorRef.current = el;
                }}
                style={{
                  background: isCursor ? "rgba(122,162,247,0.20)" : inSpan ? "rgba(122,162,247,0.06)" : undefined,
                }}
              >
                <td
                  data-testid="code-gutter"
                  style={{
                    width: 48, textAlign: "right", paddingRight: 8, userSelect: "none",
                    color: inSpan ? "#7aa2f7" : "#6c7086",
                    borderLeft: inSpan ? "2px solid #7aa2f7" : "2px solid transparent",
                    whiteSpace: "nowrap",
                  }}
                >
                  {isCursor ? "▸ " : ""}
                  {n}
                </td>
                <td style={{ padding: "0 12px", whiteSpace: "pre-wrap", wordBreak: "break-word", color: "#cdd6f4" }}>
                  {row
                    ? row.map((t, ti) => (
                        <span key={ti} style={t.color ? { color: t.color } : undefined}>
                          {t.text}
                        </span>
                      ))
                    : ln /* plain-text fallback until tokens resolve (or noHighlight) */}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
