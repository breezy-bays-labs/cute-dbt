// The highlighter registration + preload module — the LOAD-BEARING local-first seam.
//
// Every theme/lang is a STATIC import so Vite bundles it into the main chunk; the
// register/preload path pre-empts Pierre's dynamic-import resolver so NO
// theme/lang/wasm chunk is ever runtime-fetched.
//
// CONTRACT (proven by the design harness, do not change):
//   - registerCustomTheme(name, () => Promise.resolve(themeObj))  — Pierre
//     unwrapDefault's the result, so return the object directly.
//   - registerCustomLanguage(name, () => Promise.resolve({ default: langObj }), [exts])
//     — the lang path destructures `{ default }`, so it MUST be wrapped.
//   - preloadHighlighter REJECTS loudly for any unregistered theme — we let it
//     throw (no silent fallback). ensureHighlighter rethrows.
import {
  registerCustomTheme,
  registerCustomLanguage,
  preloadHighlighter,
} from "@pierre/diffs";
import type { ThemeRegistration } from "@shikijs/core";

// ── STATIC theme imports (12) — bundled into the main chunk ──────────────────
import tokyoNight from "@shikijs/themes/tokyo-night";
import dracula from "@shikijs/themes/dracula";
import catppuccinMocha from "@shikijs/themes/catppuccin-mocha";
import oneDarkPro from "@shikijs/themes/one-dark-pro";
import gruvboxDarkMedium from "@shikijs/themes/gruvbox-dark-medium";
import githubDark from "@shikijs/themes/github-dark";
import githubLight from "@shikijs/themes/github-light";
import catppuccinLatte from "@shikijs/themes/catppuccin-latte";
import oneLight from "@shikijs/themes/one-light";
import vitesseLight from "@shikijs/themes/vitesse-light";
import everforestLight from "@shikijs/themes/everforest-light";
import solarizedLight from "@shikijs/themes/solarized-light";

// ── STATIC lang imports — sql + yaml. csv has no grammar (uses "text"). ───────
import sqlLang from "@shikijs/langs/sql";
import yamlLang from "@shikijs/langs/yaml";

// ── The app-theme → shiki-theme map. ─────────────────────────────────────────
export type AppTheme =
  | "tokyo" | "dracula" | "mocha" | "onedark" | "gruvbox" | "dark"
  | "light" | "latte" | "onelight" | "vitesse" | "everforest" | "solarized";

export const APP_THEMES: readonly AppTheme[] = [
  "tokyo", "dracula", "mocha", "onedark", "gruvbox", "dark",
  "light", "latte", "onelight", "vitesse", "everforest", "solarized",
] as const;

const THEME_OBJECTS: Record<AppTheme, { shiki: string; obj: ThemeRegistration }> = {
  tokyo: { shiki: "tokyo-night", obj: tokyoNight },
  dracula: { shiki: "dracula", obj: dracula },
  mocha: { shiki: "catppuccin-mocha", obj: catppuccinMocha },
  onedark: { shiki: "one-dark-pro", obj: oneDarkPro },
  gruvbox: { shiki: "gruvbox-dark-medium", obj: gruvboxDarkMedium },
  dark: { shiki: "github-dark", obj: githubDark },
  light: { shiki: "github-light", obj: githubLight },
  latte: { shiki: "catppuccin-latte", obj: catppuccinLatte },
  onelight: { shiki: "one-light", obj: oneLight },
  vitesse: { shiki: "vitesse-light", obj: vitesseLight },
  everforest: { shiki: "everforest-light", obj: everforestLight },
  solarized: { shiki: "solarized-light", obj: solarizedLight },
};

/** The shiki theme name for an app theme (e.g. "tokyo" -> "tokyo-night"). */
export function shikiName(theme: AppTheme): string {
  return THEME_OBJECTS[theme].shiki;
}

/** True iff `s` is one of the 12 registered app themes. */
export function isAppTheme(s: string): s is AppTheme {
  return (APP_THEMES as readonly string[]).includes(s);
}

// ── Register every theme + the two langs ONCE at module load. ────────────────
for (const app of APP_THEMES) {
  const { shiki, obj } = THEME_OBJECTS[app];
  registerCustomTheme(shiki, () => Promise.resolve(obj) as never);
}
registerCustomLanguage("sql", () => Promise.resolve({ default: sqlLang }) as never, ["sql"]);
registerCustomLanguage("yaml", () => Promise.resolve({ default: yamlLang }) as never, ["yaml", "yml"]);

const LANGS = ["sql", "yaml"] as const;

// Cumulative set of every shiki theme ever requested. We pass the WHOLE set to
// each preloadHighlighter call (NOT just the new request) — see the contract
// note on ensureHighlighter.
const preloaded = new Set<string>();

/**
 * Preload the given shiki theme names (plus the sql/yaml langs) into Pierre's
 * shared highlighter BEFORE first render. Rethrows on failure — an unregistered
 * theme makes preloadHighlighter REJECT, which is the LOUD-FAIL path (no silent
 * github-dark fallback).
 *
 * NON-ADDITIVE CONTRACT (cute-dbt#488, @pierre/diffs 1.2.11): `preloadHighlighter`
 * takes the FULL desired `{ themes, langs }` set on every call — its public
 * contract is "make the shared highlighter hold exactly this set", NOT "add this
 * to whatever is already loaded". This module is called MULTIPLE times (main.tsx
 * once at boot + App.tsx on every theme switch), each with only the active theme.
 * So we accumulate every requested theme in `preloaded` and pass the UNION to
 * each call — a theme switch never drops previously-loaded themes. Do NOT
 * "optimize" this to pass only the newly-requested name: that relies on the
 * highlighter's current internal accumulation (an implementation detail), and a
 * `disposeHighlighter`/version bump would silently break multi-theme render. The
 * S5 multi-theme work depends on this union semantics.
 */
export async function ensureHighlighter(themeShikiNames: string[]): Promise<void> {
  // Union of every previously-loaded theme + the new request; commit to
  // `preloaded` only AFTER a successful preload so a rejected (unregistered)
  // theme is not re-sent on the next call (it would re-reject loudly).
  const wanted = new Set(preloaded);
  for (const t of themeShikiNames) wanted.add(t);
  await preloadHighlighter({
    themes: [...wanted],
    langs: [...LANGS],
    preferredHighlighter: "shiki-js",
  });
  for (const t of wanted) preloaded.add(t);
}
