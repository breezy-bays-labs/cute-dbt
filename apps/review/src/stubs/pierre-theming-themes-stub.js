// Local-first replacement for `@pierre/theming/themes`.
//
// PROBLEM: the real module statically constructs two theme collections —
// `shikiThemes` (a map of `() => import("@shikijs/themes/<name>")` for 63
// themes) and `pierreThemes` (a map of `() => import("@pierre/theme/<name>")`
// for 6 themes). Those import-map objects are reachable from Pierre's
// `shared_highlighter.js` (`pierreThemes.getThemes()`) and `themeResolution.js`
// (`shikiThemes.getTheme()`), so the bundler emits a separate fetchable chunk
// for every theme — breaking local-first.
//
// FIX: a Vite `resolve.alias` maps `@pierre/theming/themes` to this stub, which
// builds EMPTY collections with Pierre's own `createThemeCollection` factory.
// Behaviour preserved:
//   - `pierreThemes.getThemes()` -> []  (no pierre themes pre-registered; fine,
//     we don't use them)
//   - `shikiThemes.getTheme(name)` -> undefined  (the built-in fallback finds
//     nothing; ANY theme we haven't explicitly registered via
//     `registerCustomTheme` then throws "No valid theme loader registered" —
//     exactly the LOUD failure we want for unregistered themes)
//   - `createTheme(...)` -> a working factory (registerCustomTheme.js imports it
//     from here; it MUST keep working). Reimplemented inline (1:1 with the real
//     `@pierre/theming/dist/modules/createTheme.js`) so the stub is
//     self-contained and pulls in NO theme import maps.
// We register every app theme ourselves via statically-imported loaders, so the
// built-in collections are dead weight we strip.
import { createThemeCollection } from "@pierre/theming";
import { normalizeTheme } from "shiki/core";

const EMPTY = createThemeCollection({ themes: [] });

export const pierreThemes = EMPTY;
export const shikiThemes = EMPTY;
export const themes = EMPTY;

// 1:1 reimplementation of @pierre/theming/dist/modules/createTheme.js so
// registerCustomTheme keeps working without importing the real (map-bearing)
// themes module.
function unwrapDefault(value) {
  return value !== null && typeof value === "object" && "default" in value
    ? value.default
    : value;
}
function normalizingLoader(loader) {
  return async () => normalizeTheme(unwrapDefault(await loader()));
}
export function createTheme({ name, load, colorScheme, collection, displayName }) {
  return {
    name,
    colorScheme,
    collection,
    displayName,
    load: normalizingLoader(load),
  };
}
