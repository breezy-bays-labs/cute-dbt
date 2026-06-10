/* cute-dbt appearance engine v1 (cute-dbt#178)
   ----------------------------------------------------------------------------
   Wires the theme / style / accent / density / diff-style / diff-layout
   controls in the report's settings panel (the markup is static in
   templates/report.html — the askama DOM contract), persists the choices in
   localStorage (key cute-dbt.appearance.v1) with a graceful in-memory
   fallback, and syncs DataTables dark mode by toggling html.dark. Plain
   vanilla JS, no framework, zero egress.

   First-party, NOT vendored: this file lives at templates/theme.js, embedded
   at compile time via asset_embed::THEME_JS (include_str!) and interpolated
   inline by the askama renderer. Banner-pin + end-of-file-sentinel tests in
   src/adapters/asset_embed.rs guard the include.

   The handoff prototype also carried a Cytoscape DAG-engine picker here; that
   is Bucket 2 (cute-dbt#101 territory) and is deliberately ABSENT from this
   file — adding it back rides the vendored-Cytoscape PR, never this one. */
(function () {
  "use strict";

  // The five [data-theme] packs the chassis CSS ships (templates/report.css).
  // `dark` drives the html.dark class DataTables' vendored dark rules key on.
  var THEMES = [
    { id: "light",     dark: false },
    { id: "dark",      dark: true  },
    { id: "tokyo",     dark: true  },
    { id: "solarized", dark: false },
    { id: "gruvbox",   dark: true  }
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
  // contract (cute-dbt#178 AC3), not a credential.
  var KEY = "cute-dbt.appearance.v1"; // gitleaks:allow — a public storage key name, no secret
  var pref = { theme: null, style: "soft", accent: "theme", density: "auto", diffstyle: "color", difflayout: "auto" };

  // Read the persisted appearance, string-typed keys only. Any storage error
  // (file:// SecurityError, disabled storage) leaves the defaults intact.
  function load() {
    var raw;
    try { raw = window.localStorage && window.localStorage.getItem(KEY); } catch (e) { raw = null; }
    if (!raw) return;
    try {
      var p = JSON.parse(raw);
      if (p && typeof p === "object") {
        ["theme", "style", "accent", "density", "diffstyle", "difflayout"].forEach(function (k) {
          if (typeof p[k] === "string") pref[k] = p[k];
        });
      }
    } catch (e) { /* ignore — defaults hold */ }
  }
  // Persist the current appearance. Swallows any storage error (zero-egress
  // in-memory fallback, mirrors interaction.js saveSettings).
  function save() {
    try { if (window.localStorage) window.localStorage.setItem(KEY, JSON.stringify(pref)); } catch (e) { /* in-memory only */ }
  }

  var root = document.documentElement;

  // ES5-safe NodeList iteration (gemini review, PR #188): older engines ship
  // querySelectorAll results without NodeList.prototype.forEach; iterate via
  // an index loop so this file keeps its deliberate ES5 posture throughout.
  function qsaForEach(selector, fn) {
    var list = document.querySelectorAll(selector);
    for (var i = 0; i < list.length; i++) fn(list[i]);
  }

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
  function applyDiffStyle(s) {
    if (s === "color") root.removeAttribute("data-diffstyle");
    else root.setAttribute("data-diffstyle", s);
    pref.diffstyle = s;
  }
  function applyDiffLayout(v) {
    root.setAttribute("data-difflayout", v);
    pref.difflayout = v;
  }
  function applyStyle(s) {
    root.setAttribute("data-style", s);
    pref.style = s;
  }

  // Re-tint the DAG after a theme flip (the light/dark edge + anchor
  // variants live in interaction.js, which owns the legend + Mermaid).
  function rerenderDag() {
    if (typeof window.__cuteRerenderDag === "function") window.__cuteRerenderDag();
  }

  /* ---- sync visual state of the (static) controls to prefs ------------- */
  function syncControls() {
    qsaForEach(".style-opt", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-style-id") === pref.style ? "true" : "false");
    });
    qsaForEach(".theme-chip", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-theme-id") === pref.theme ? "true" : "false");
    });
    qsaForEach(".accent-swatch", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-accent") === pref.accent ? "true" : "false");
    });
    qsaForEach(".density-seg button", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-density") === pref.density ? "true" : "false");
    });
    qsaForEach(".diff-seg button", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-diffstyle") === pref.diffstyle ? "true" : "false");
    });
    qsaForEach(".difflayout-seg button", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-difflayout") === pref.difflayout ? "true" : "false");
    });
  }

  function wire() {
    qsaForEach(".style-opt", function (b) {
      b.addEventListener("click", function () { applyStyle(b.getAttribute("data-style-id")); save(); syncControls(); reflowTables(); rerenderDag(); });
    });
    qsaForEach(".theme-chip", function (b) {
      b.addEventListener("click", function () { applyTheme(b.getAttribute("data-theme-id")); save(); syncControls(); reflowTables(); rerenderDag(); });
    });
    qsaForEach(".accent-swatch", function (b) {
      b.addEventListener("click", function () { applyAccent(b.getAttribute("data-accent")); save(); syncControls(); });
    });
    qsaForEach(".density-seg button", function (b) {
      b.addEventListener("click", function () { applyDensity(b.getAttribute("data-density")); save(); syncControls(); reflowTables(); });
    });
    qsaForEach(".diff-seg button", function (b) {
      b.addEventListener("click", function () { applyDiffStyle(b.getAttribute("data-diffstyle")); save(); syncControls(); });
    });
    qsaForEach(".difflayout-seg button", function (b) {
      b.addEventListener("click", function () { applyDiffLayout(b.getAttribute("data-difflayout")); save(); syncControls(); });
    });
  }

  // DataTables column widths shift when the font-size changes (density /
  // style / theme flips); re-adjust the visible ones. jQuery is bound
  // locally (gemini review, PR #188) so the lookup is explicit and a bare
  // global `jQuery` reference can never throw mid-iteration.
  function reflowTables() {
    var $ = window.jQuery;
    if ($ && $.fn && $.fn.DataTable) {
      $("table.given-table, table.expected-table").each(function () {
        if ($.fn.DataTable.isDataTable(this) && this.offsetParent !== null) {
          try { $(this).DataTable().columns.adjust(); } catch (e) { /* hidden */ }
        }
      });
    }
  }

  function boot() {
    load();
    // Default theme: saved -> prefers-color-scheme -> light.
    if (!pref.theme) {
      pref.theme = (window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches) ? "dark" : "light";
    }
    applyStyle(pref.style);
    applyTheme(pref.theme);
    applyAccent(pref.accent);
    applyDensity(pref.density);
    applyDiffStyle(pref.diffstyle);
    applyDiffLayout(pref.difflayout);
    syncControls();
    wire();
    // Re-tint the legend + Mermaid host once the theme's .dark class is on:
    // interaction.js drew them on DOM-ready BEFORE this boot applied the
    // theme, so the edge swatches would otherwise keep the light palette.
    setTimeout(rerenderDag, 0);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
/* end of cute-dbt appearance engine v1 (cute-dbt#178) */
