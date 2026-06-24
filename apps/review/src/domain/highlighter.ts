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

const preloaded = new Set<string>();

/**
 * Preload the given shiki theme names (plus the sql/yaml langs) into Pierre's
 * shared highlighter BEFORE first render. Rethrows on failure — an unregistered
 * theme makes preloadHighlighter REJECT, which is the LOUD-FAIL path (no silent
 * github-dark fallback).
 */
export async function ensureHighlighter(themeShikiNames: string[]): Promise<void> {
  const fresh = themeShikiNames.filter((t) => !preloaded.has(t));
  await preloadHighlighter({
    themes: themeShikiNames,
    langs: [...LANGS],
    preferredHighlighter: "shiki-js",
  });
  for (const t of fresh) preloaded.add(t);
}
