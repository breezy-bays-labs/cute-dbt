/* cute-dbt report appearance settings v1 (cute-dbt#178, re-layered cute-dbt#242)
   ----------------------------------------------------------------------------
   The REPORT-ONLY half of the appearance system: wires the theme / style /
   accent / density / diff-layout / DAG-engine / coverage controls in the
   report's settings panel (the markup is static in templates/report.html —
   the askama DOM contract), syncs their visual state, reflows DataTables on
   metric-affecting flips and dispatches the DAG-engine pick + re-tint.

   The load/apply/persist core (localStorage key cute-dbt.appearance.v1,
   prefers-color-scheme default, the html-level attribute application)
   moved to the SHARED appearance engine at cute-dbt#242 —
   templates/appearance.js, which parses immediately before this file and
   exposes window.CuteAppearance. This file drives that engine; it never
   touches the storage key or the html attributes directly. The #188
   diff-cells colour/marks control was retired by design pass-2
   (cute-dbt#198): cells always render in colour; a legacy persisted
   `diffstyle` key is ignored gracefully by the shared engine's load().

   First-party, NOT vendored: this file lives at templates/theme.js, embedded
   at compile time via asset_embed::THEME_JS (include_str!) and interpolated
   inline by the askama renderer. Banner-pin + end-of-file-sentinel tests in
   src/adapters/asset_embed.rs guard the include. */
(function () {
  "use strict";

  // The shared appearance engine (templates/appearance.js) parses before
  // this file in the report's script order; its boot already loaded +
  // applied the persisted appearance. `pref` is the ONE live state object,
  // shared by reference — the engine's apply* mutate it, save() persists it.
  var appearance = window.CuteAppearance;
  var pref = appearance.pref;

  // ES5-safe NodeList iteration (gemini review, PR #188): older engines ship
  // querySelectorAll results without NodeList.prototype.forEach; iterate via
  // an index loop so this file keeps its deliberate ES5 posture throughout.
  function qsaForEach(selector, fn) {
    var list = document.querySelectorAll(selector);
    for (var i = 0; i < list.length; i++) fn(list[i]);
  }

  // cute-dbt#180 — push the picker's engine to the render dispatcher
  // (interaction.js owns the DAG; this file just records the choice on the
  // shared pref). REPORT-ONLY by design: the explore pages run no DAG
  // engine picker, so this apply stays out of the shared engine. The
  // caller follows up with rerenderDag() so the flip happens in place.
  function applyEngine(e) {
    var engine = e === "cytoscape" ? "cytoscape" : "mermaid";
    pref.engine = engine;
    if (typeof window.__cuteSetDagEngine === "function") window.__cuteSetDagEngine(engine);
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
    qsaForEach(".difflayout-seg button", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-difflayout") === pref.difflayout ? "true" : "false");
    });
    qsaForEach(".engine-seg button", function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-engine") === pref.engine ? "true" : "false");
    });
    // cute-dbt#219 — the coverage switch mirrors the pref (a checkbox owns
    // its checked state natively; no aria-pressed on an <input>).
    var cov = document.getElementById("settings-coverage-input");
    if (cov) cov.checked = pref.coverage !== "off";
  }

  function wire() {
    qsaForEach(".style-opt", function (b) {
      b.addEventListener("click", function () { appearance.applyStyle(b.getAttribute("data-style-id")); appearance.save(); syncControls(); reflowTables(); rerenderDag(); });
    });
    qsaForEach(".theme-chip", function (b) {
      b.addEventListener("click", function () { appearance.applyTheme(b.getAttribute("data-theme-id")); appearance.save(); syncControls(); reflowTables(); rerenderDag(); });
    });
    qsaForEach(".accent-swatch", function (b) {
      b.addEventListener("click", function () { appearance.applyAccent(b.getAttribute("data-accent")); appearance.save(); syncControls(); });
    });
    qsaForEach(".density-seg button", function (b) {
      b.addEventListener("click", function () { appearance.applyDensity(b.getAttribute("data-density")); appearance.save(); syncControls(); reflowTables(); });
    });
    qsaForEach(".difflayout-seg button", function (b) {
      b.addEventListener("click", function () { appearance.applyDiffLayout(b.getAttribute("data-difflayout")); appearance.save(); syncControls(); });
    });
    // cute-dbt#180 — the DAG-engine picker. The swap is IN PLACE: push the
    // engine to the dispatcher, persist, then rerenderDag() tears down the
    // old engine and builds the new one. No reload.
    qsaForEach(".engine-seg button", function (b) {
      b.addEventListener("click", function () { applyEngine(b.getAttribute("data-engine")); appearance.save(); syncControls(); rerenderDag(); });
    });
    // cute-dbt#219 — the coverage-intelligence switch: flip the attribute,
    // persist. Display-only — no re-render, no DAG/table reflow needed.
    var cov = document.getElementById("settings-coverage-input");
    if (cov) {
      cov.addEventListener("change", function () { appearance.applyCoverage(cov.checked ? "on" : "off"); appearance.save(); });
    }
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
    // The shared engine already loaded + applied the persisted appearance
    // at parse time; this boot owns the report-only side: control state,
    // wiring, and the deferred DAG-engine apply.
    syncControls();
    wire();
    // Re-tint the legend + the active DAG engine once the theme's .dark
    // class is on: interaction.js drew them on DOM-ready, so the edge
    // swatches would otherwise keep the light palette on a dark boot.
    // The engine apply rides the same deferred tick (cute-dbt#180): by
    // then interaction.js's boot has installed __cuteSetDagEngine, so a
    // persisted "cytoscape" choice flips the DAG in place right after the
    // default Mermaid render.
    setTimeout(function () {
      applyEngine(pref.engine);
      rerenderDag();
    }, 0);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
/* end of cute-dbt report appearance settings v1 (cute-dbt#178) */
