// SuggestionBlock — renders ONE ```suggestion block as a "Suggested change"
// diff: anchored (old) lines struck (del), proposed (new) lines added (add),
// with the same per-line Shiki highlight + intra-line word emphasis as the main
// diff viewer. An empty suggestion = a deletion. First-party (never delegated);
// the suggestion-as-diff affordance is a cute-dbt design invariant.
//
// LAYER: view (imports domain only).
import React from "react";
import { pairBlocks, type EmphLine } from "../../domain/diff/inline-emphasis";
import type { CodeLang } from "../../domain/code-highlighter";
import { ShikiLine } from "./ShikiLine";

function SugRow({ sign, line, kind, lang, shiki }: { sign: string; line: EmphLine; kind: "del" | "add"; lang: CodeLang; shiki: string }): React.ReactElement {
  const emphCls = kind === "del" ? "diff-word-del" : "diff-word-add";
  return (
    <tr data-side={kind} className={kind === "del" ? "bg-[var(--diff-del-bg)]" : "bg-[var(--diff-add-bg)]"}>
      <td className="w-5 select-none text-center align-top text-zinc-500">{sign}</td>
      <td className="whitespace-pre-wrap break-words px-2 align-top">
        {line.s === "" ? <span>{" "}</span> : <ShikiLine line={line.s} lang={lang} shiki={shiki} emph={line.emph} emphCls={emphCls} />}
      </td>
    </tr>
  );
}

export function SuggestionBlock({
  oldCode,
  newCode,
  lang = "sql",
  shiki,
}: {
  oldCode: string | null | undefined;
  newCode: string | null | undefined;
  lang?: CodeLang;
  shiki: string;
}): React.ReactElement {
  const oldLinesRaw = oldCode == null ? [] : String(oldCode).split("\n");
  const newCodeStr = String(newCode == null ? "" : newCode);
  const newLinesRaw = newCodeStr === "" ? [] : newCodeStr.split("\n");
  const { oldLines, newLines } = pairBlocks(oldLinesRaw, newLinesRaw);
  return (
    <div data-testid="suggestion-block" className="my-1.5 overflow-hidden rounded-md border border-zinc-700 bg-zinc-900">
      <div className="flex h-7 items-center gap-1.5 border-b border-zinc-800 bg-zinc-800/60 px-2.5">
        <span className="font-mono text-[10px] uppercase tracking-[0.06em] text-zinc-400">Suggested change</span>
      </div>
      <table className="w-full border-collapse font-mono text-[12px] leading-[1.5]">
        <tbody className="align-top">
          {oldLines.map((l, i) => (
            <SugRow key={"o" + i} sign="−" line={l} kind="del" lang={lang} shiki={shiki} />
          ))}
          {newLines.length ? (
            newLines.map((l, i) => <SugRow key={"n" + i} sign="+" line={l} kind="add" lang={lang} shiki={shiki} />)
          ) : (
            <tr className="bg-[var(--diff-del-bg)]">
              <td />
              <td className="px-2 text-[11px] italic text-zinc-500">(lines removed)</td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}
