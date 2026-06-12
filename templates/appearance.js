/* cute-dbt shared appearance engine v1 (cute-dbt#242)
   ----------------------------------------------------------------------------
   The cross-page-family half of the appearance system, re-layered out of
   templates/theme.js at cute-dbt#242: reads the persisted appearance
   (localStorage key cute-dbt.appearance.v1, graceful in-memory fallback),
   resolves the prefers-color-scheme default, and applies the html-level
   hooks the shared token partial (templates/partials/tokens.css) keys on:
   [data-theme] + the html.dark family class (the DataTables dark sync),
   [data-style], [data-density], [data-difflayout], [data-coverage],
   [data-project] and the accent custom-property overrides. Embedded into BOTH page families —
   report.html (where templates/theme.js layers the settings UI on top via
   window.CuteAppearance) and explore-dag.html / explore-tests.html (which
   apply the saved appearance read-only; the explore-side settings
   affordance is cute-dbt#219's lane). Plain vanilla ES5, no framework,
   zero egress.

   Application happens at PARSE TIME (the script sits at the end of <body>;
   documentElement already exists), so the attributes are in place before
   any DOMContentLoaded boot — including the report's interaction engine
   and theme.js's control sync.

   Attributes without matching rules on a page are deliberately applied
   anyway ([data-difflayout] / [data-coverage] / [data-project] are inert
   on the explore pages today): ONE attribute contract across page
   families, so pages
   adopt new surfaces without touching this engine. The `engine` field
   (the report's Mermaid <-> Cytoscape DAG pick) is persisted state only
   here — applying it is report-specific (theme.js applyEngine).

   First-party, NOT vendored: this file lives at templates/appearance.js,
   embedded at compile time via asset_embed::APPEARANCE_JS (include_str!)
   and interpolated inline by the askama renderers. Banner-pin +
   end-of-file-sentinel tests in src/adapters/asset_embed.rs guard the
   include. */
(function () {
  "use strict";

  // The eight [data-theme] packs the token partial ships
  // (templates/partials/tokens.css), light family first (design pass-2,
  // cute-dbt#198) — kept in lockstep with the report's static theme grid.
  // `dark` drives the html.dark class DataTables' vendored dark rules key on.
  var THEMES = [
    { id: "light",     dark: false },
    { id: "solarized", dark: false },
    { id: "latte",     dark: false },
    { id: "rosepine",  dark: false },
    { id: "dark",      dark: true  },
    { id: "tokyo",     dark: true  },
    { id: "gruvbox",   dark: true  },
    { id: "dracula",   dark: true  }
  ];

  // Accent palettes: value / hover / tint triples applied as inline custom
  // properties on <html>. "theme" = remove the override (the active theme's
  // own accent shows through).
  var ACCENTS = [
    { id: "theme" },
    { id: "teal",   v: "#1d7484", h: "#155c69", t: "#e3f0f2" },
    { id: "blue",   v: "#2a6fdb", h: "#1f57b0", t: "#e6eefb" },
    { id: "violet", v: "#7c3aed", h: "#6027c4", t: "#efe8fd" },
    { id: "green",  v: "#1f8a5b", h: "#176d47", t: "#e2f3ec" },
    { id: "amber",  v: "#b45c0c", h: "#8f4708", t: "#fbeede" }
  ];

  // The public localStorage key for the appearance state — a stable consumer
  // contract (cute-dbt#178 AC3), not a credential. Cross-page since #242:
  // every cute-dbt page family reads + applies this same key.
  var KEY = "cute-dbt.appearance.v1"; // gitleaks:allow — a public storage key name, no secret
  // `engine` (cute-dbt#180) is the DAG-engine picker: "mermaid" (the static
  // default) or "cytoscape" (the opt-in interactive engine).
  // `coverage` (cute-dbt#219) is the viewer-side coverage-intelligence
  // display toggle: "off" (the default since cute-dbt#292 — the settings
  // panel's Experimental group; the field may be absent) or the explicit
  // opt-in "on". The CROSS-PAGE contract: any cute-dbt page that renders
  // check-engine-derived content reads this same field — the report keys
  // one CSS rule on html[data-coverage=off]; the explorer pages adopt it
  // as #103/#104 land.
  // `project` (cute-dbt#292) is the viewer-side project-state display
  // toggle: "on" (default; the field may be absent) or "off". Emission is
  // already opt-in at render time (the cute-dbt#291 experimental gate), so
  // the viewer default SHOWS what the producer chose to emit; the report
  // keys CSS rules on html[data-project=off].
  var pref = { theme: null, style: "soft", accent: "theme", density: "auto", difflayout: "auto", engine: "mermaid", coverage: "off", project: "on" };

  // Read the persisted appearance, string-typed keys only. Any storage error
  // (file:// SecurityError, disabled storage) leaves the defaults intact.
  function load() {
    var raw;
    try { raw = window.localStorage && window.localStorage.getItem(KEY); } catch (e) { raw = null; }
    if (!raw) return;
    try {
      var p = JSON.parse(raw);
      if (p && typeof p === "object") {
        ["theme", "style", "accent", "density", "difflayout", "engine", "coverage", "project"].forEach(function (k) {
          if (typeof p[k] === "string") pref[k] = p[k];
        });
      }
    } catch (e) { /* ignore — defaults hold */ }
    // Coerce the engine into its closed vocabulary — an unknown persisted
    // value must fall back to the static default, never reach the dispatcher.
    if (pref.engine !== "cytoscape") pref.engine = "mermaid";
    // Same closed-vocabulary coercion for the coverage toggle (cute-dbt#219;
    // default flipped OFF at cute-dbt#292): anything but the explicit "on"
    // reads as the default OFF.
    if (pref.coverage !== "on") pref.coverage = "off";
    // And for the project-state display toggle (cute-dbt#292): anything but
    // the explicit "off" reads as the default ON — emission is already
    // opt-in at render time (cute-dbt#291).
    if (pref.project !== "off") pref.project = "on";
  }
  // Persist the current appearance. Swallows any storage error (zero-egress
  // in-memory fallback, mirrors interaction.js saveSettings).
  function save() {
    try { if (window.localStorage) window.localStorage.setItem(KEY, JSON.stringify(pref)); } catch (e) { /* in-memory only */ }
  }

  var root = document.documentElement;

  function themeById(id) {
    for (var i = 0; i < THEMES.length; i++) if (THEMES[i].id === id) return THEMES[i];
    return THEMES[0];
  }

  function applyTheme(id) {
    var t = themeById(id);
    root.setAttribute("data-theme", t.id);
    root.classList.toggle("dark", t.dark); // DataTables dark rules follow html.dark
    pref.theme = t.id;
  }
  function applyAccent(id) {
    var a = null;
    for (var i = 0; i < ACCENTS.length; i++) if (ACCENTS[i].id === id) a = ACCENTS[i];
    if (!a || a.id === "theme") {
      root.style.removeProperty("--accent");
      root.style.removeProperty("--accent-hover");
      root.style.removeProperty("--accent-tint");
      pref.accent = "theme";
    } else {
      root.style.setProperty("--accent", a.v);
      root.style.setProperty("--accent-hover", a.h);
      root.style.setProperty("--accent-tint", a.t);
      pref.accent = a.id;
    }
  }
  function applyDensity(d) {
    if (d === "auto") root.removeAttribute("data-density");
    else root.setAttribute("data-density", d);
    pref.density = d;
  }
  function applyDiffLayout(v) {
    root.setAttribute("data-difflayout", v);
    pref.difflayout = v;
  }
  function applyStyle(s) {
    root.setAttribute("data-style", s);
    pref.style = s;
  }
  // cute-dbt#219 — the coverage-intelligence display toggle. PURE display:
  // OFF sets html[data-coverage=off] (one CSS rule hides every
  // check-engine-derived surface); ON removes the attribute and the
  // already-rendered content shows again — no re-render, payload untouched.
  // Default OFF since cute-dbt#292 (the Experimental settings group), so an
  // unknown value reads as OFF.
  function applyCoverage(v) {
    var coverage = v === "on" ? "on" : "off";
    if (coverage === "off") root.setAttribute("data-coverage", "off");
    else root.removeAttribute("data-coverage");
    pref.coverage = coverage;
  }
  // cute-dbt#292 — the project-state display toggle, the applyCoverage
  // twin with the OPPOSITE default polarity (emission is already opt-in at
  // render time, so an unknown value reads as ON): OFF sets
  // html[data-project=off] — CSS hides the project-definition panel and the
  // per-model provenance/var chips; ON removes the attribute. PURE display:
  // no re-render, payload untouched.
  function applyProject(v) {
    var project = v === "off" ? "off" : "on";
    if (project === "off") root.setAttribute("data-project", "off");
    else root.removeAttribute("data-project");
    pref.project = project;
  }

  // ---- boot: load, resolve the default theme, apply everything ----------
  load();
  // Default theme: saved -> prefers-color-scheme -> light.
  if (!pref.theme) {
    pref.theme = (window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches) ? "dark" : "light";
  }
  applyStyle(pref.style);
  applyTheme(pref.theme);
  applyAccent(pref.accent);
  applyDensity(pref.density);
  applyDiffLayout(pref.difflayout);
  applyCoverage(pref.coverage);
  applyProject(pref.project);

  // The page-facing seam: templates/theme.js (the report's settings UI)
  // drives the engine through this object — `pref` is the ONE live state
  // (shared by reference; apply* mutate it, save() persists it).
  window.CuteAppearance = {
    key: KEY,
    pref: pref,
    save: save,
    applyTheme: applyTheme,
    applyStyle: applyStyle,
    applyAccent: applyAccent,
    applyDensity: applyDensity,
    applyDiffLayout: applyDiffLayout,
    applyCoverage: applyCoverage,
    applyProject: applyProject
  };
})();
/* end of cute-dbt shared appearance engine v1 (cute-dbt#242) */
