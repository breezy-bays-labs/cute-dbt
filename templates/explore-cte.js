/* cute-dbt explore CTE engine v1 (cute-dbt#102)
   ----------------------------------------------------------------------------
   The CTE <-> model view toggle on explore's dag.html plus the per-model
   Cytoscape CTE DAG it renders. Binding decision (epic cute-dbt#99 V3):
   the CTE view follows the HIGHLIGHTED model — the same in-page selection
   the click / fuzzy-search-select affordance drives (a Space focus commit
   implies a highlight, so a committed model is always viewable). With no
   highlight the CTE arm is disabled; while the CTE view is active a
   search-select retargets it in place, and a cleared highlight falls back
   to the lineage view.

   View-state contract (the cute-dbt#102 AC): switching views is LOCAL
   STATE on the same page — no reload, and the lineage Cytoscape instance
   is NEVER rebuilt (its pan / zoom / highlight survive a round trip; only
   cy.resize() runs after un-hiding). The CTE instance rebuilds only on a
   model retarget — a view change, the cyto-dag.js model-switch precedent;
   per-interaction work inside a built instance stays in-place.

   Data: the `explore-dag-data` carrier's `cte_dags` map (full model node
   id -> the SAME DagPayload the report renders for that model:
   role-classified nodes + join-typed edges, parsed once in Rust). A
   missing entry on a `not_compiled` model renders the labeled fail-open
   degraded view (never an error); missing on a compiled model renders
   the "no CTE structure" sparse state.

   Init hygiene (the ADR-4-amendment contract, explore-page variant):
   - node labels are CANVAS TEXT — XSS-safe by construction; hostile
     manifest-derived names draw as glyphs, never parse as HTML.
   - explicit non-webfont system fontFamily (zero-egress: no font fetch).
   - the dagre layout runs IN-THREAD via the vendored cytoscape-dagre UMD
     already registered for the lineage view (never the EPL
     cytoscape-elk) and NO workers.
   - measured node widths via canvas measureText (no deprecated
     `width: label`, no custom wheelSensitivity — both emit console
     warnings the headless gate rejects).
   - handlers are bound from THIS file; DOM strings land via textContent
     only; this file never writes data-selected-model (the Space commit
     in explore-lineage.js stays the single write site).

   First-party, NOT vendored: lives at templates/explore-cte.js, embedded
   at compile time via asset_embed::EXPLORE_CTE_JS (include_str!) and
   interpolated inline by the askama renderer. Banner-pin +
   end-of-file-sentinel tests in src/adapters/asset_embed.rs guard the
   include. */
(function () {
  "use strict";

  var carrier = document.getElementById("explore-dag-data");
  if (!carrier) return;
  var data = JSON.parse(carrier.textContent);
  if (!data.nodes || !data.nodes.length) return; // empty manifest: no views
  var cteDags = data.cte_dags || {};

  var toggle = document.querySelector(".view-toggle");
  var lineageView = document.querySelector(".lineage-view");
  var cteView = document.querySelector(".cte-view");
  if (!toggle || !lineageView || !cteView) return;
  var btnLineage = toggle.querySelector('[data-view="lineage"]');
  var btnCte = toggle.querySelector('[data-view="cte"]');
  var host = cteView.querySelector(".cte-canvas");
  var titleModel = cteView.querySelector(".cte-view-model");
  var degraded = cteView.querySelector(".cte-degraded");
  var sparse = cteView.querySelector(".cte-view-empty");

  // id -> lineage node (bare name + the fail-open not_compiled flag).
  // Prototype-less so a hostile model id can never collide with a
  // built-in prototype property (the cyto-dag.js precedent).
  var modelById = Object.create(null);
  data.nodes.forEach(function (n) { modelById[n.id] = n; });

  var view = "lineage";
  var highlightedId = null;
  var cy = null;                // the CTE instance (rebuilt per retarget)
  var renderedModelId = null;

  // ---- style (canvas text, measured widths, role + join vocabulary) ------
  var FONT_STACK = 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace';
  var measureCtx = document.createElement("canvas").getContext("2d");
  measureCtx.font = "13px " + FONT_STACK;
  function labelWidth(label) {
    return Math.min(300, Math.ceil(measureCtx.measureText(label).width) + 26);
  }

  // The join-type edge label: the DagPayload wire key with underscores
  // opened up (`union_all` -> `union all`).
  function joinLabel(etype) {
    return String(etype || "from").replace(/_/g, " ");
  }

  // Explore-page light palette, role vocabulary aligned with the report
  // (final / import / transform) and the lineage view's hues.
  var ROLES = {
    final: { fill: "#e8f1f8", stroke: "#0072B2", shape: "hexagon" },
    import: { fill: "#eef7ee", stroke: "#1f8a5b", shape: "round-rectangle" },
    transform: { fill: "#f4f4f5", stroke: "#6b6b76", shape: "rectangle" }
  };

  function roleStyle(role) {
    var r = ROLES[role] || ROLES.transform;
    return {
      "background-color": r.fill,
      "border-color": r.stroke,
      "shape": r.shape
    };
  }

  function styleSheet() {
    return [
      { selector: "node", style: {
        "label": "data(label)",
        "text-valign": "center",
        "text-halign": "center",
        "font-family": FONT_STACK,
        "font-size": 13,
        "color": "#1c1c1f",
        "text-wrap": "ellipsis",
        "text-max-width": 280,
        "width": "data(w)",
        "height": 30,
        "border-width": 1.6,
        "background-color": ROLES.transform.fill,
        "border-color": ROLES.transform.stroke,
        "shape": "rectangle"
      }},
      { selector: 'node[role = "import"]', style: roleStyle("import") },
      { selector: 'node[role = "final"]', style: roleStyle("final") },
      { selector: "edge", style: {
        "curve-style": "bezier",
        "width": 1.8,
        "line-color": "#8b8b96",
        "target-arrow-shape": "triangle",
        "target-arrow-color": "#8b8b96",
        "arrow-scale": 1.1,
        "label": "data(elabel)",
        "font-family": FONT_STACK,
        "font-size": 10,
        "color": "#555",
        "text-rotation": "autorotate",
        "text-background-color": "#fcfcfd",
        "text-background-opacity": 1,
        "text-background-padding": "2px"
      }},
      { selector: 'edge[etype = "union_all"], edge[etype = "union_distinct"]', style: {
        "line-style": "dashed",
        "line-dash-pattern": [6, 3]
      }}
    ];
  }

  // ---- elements off one cte_dags DagPayload entry --------------------------
  function buildElements(dag) {
    var els = [];
    dag.nodes.forEach(function (n) {
      var label = String(n.label || n.id);
      els.push({
        group: "nodes",
        // Resolved only via cy.getElementById — a selector-hostile CTE
        // alias is inert (the cute-dbt#155 / #180 precedent).
        data: {
          id: n.id,
          label: label,
          w: labelWidth(label),
          role: n.role || "transform"
        }
      });
    });
    (dag.edges || []).forEach(function (e, i) {
      els.push({
        group: "edges",
        data: {
          id: "e" + i,
          source: e.from,
          target: e.to,
          etype: e.edge_type || "from",
          elabel: joinLabel(e.edge_type)
        }
      });
    });
    return els;
  }

  // ---- CTE render (per-model; a view change, so a rebuild is allowed) -----
  function destroyCte() {
    if (cy) { cy.destroy(); cy = null; }
  }

  function renderCte(modelId) {
    renderedModelId = modelId;
    var m = modelById[modelId];
    titleModel.textContent = m ? m.name : String(modelId);
    host.hidden = false;
    degraded.hidden = true;
    sparse.hidden = true;
    destroyCte();
    var dag = Object.prototype.hasOwnProperty.call(cteDags, modelId)
      ? cteDags[modelId]
      : null;
    if (!dag) {
      host.hidden = true;
      if (m && m.not_compiled) {
        // Fail-open: the labeled degraded view, never an error.
        degraded.textContent = m.name + " has no compiled SQL in this manifest"
          + " (dbt parse) — run dbt compile to fill in its CTE DAG."
          + " The model still renders in the lineage view.";
        degraded.hidden = false;
      } else {
        sparse.hidden = false;
      }
      return;
    }
    cy = window.cytoscape({
      container: host,
      elements: buildElements(dag),
      style: styleSheet(),
      // Same vendored dagre left-to-right ranks as the lineage view —
      // the one registered layout extension on this page.
      layout: { name: "dagre", rankDir: "LR", nodeSep: 18, rankSep: 90, edgeSep: 12, fit: true, padding: 24 },
      minZoom: 0.2,
      maxZoom: 2.5,
      boxSelectionEnabled: false
    });
  }

  // ---- the view toggle (chrome + selection persist; same page) ------------
  function setView(next) {
    if (next === view) return;
    if (next === "cte" && !highlightedId) return; // gated on a highlight
    view = next;
    var isCte = view === "cte";
    lineageView.hidden = isCte;
    cteView.hidden = !isCte;
    btnLineage.classList.toggle("is-active", !isCte);
    btnLineage.setAttribute("aria-pressed", String(!isCte));
    btnCte.classList.toggle("is-active", isCte);
    btnCte.setAttribute("aria-pressed", String(isCte));
    if (isCte) {
      if (renderedModelId !== highlightedId) {
        renderCte(highlightedId);
      } else if (cy) {
        cy.resize(); // dimensions only — pan/zoom persist
      }
    } else if (window.CuteExploreLineage) {
      // The lineage instance was never rebuilt: its pan/zoom/highlight
      // persist; only the canvas dimensions need a refresh.
      var lcy = window.CuteExploreLineage.cyInstance();
      if (lcy) lcy.resize();
    }
  }

  // The lineage engine's page-local highlight event gates the CTE arm
  // and retargets an active CTE view in place.
  document.addEventListener("cute-explore-highlight", function (ev) {
    highlightedId = ev.detail ? ev.detail.id : null;
    btnCte.disabled = !highlightedId;
    if (view !== "cte") return;
    if (!highlightedId) {
      setView("lineage");
      return;
    }
    if (highlightedId !== renderedModelId) renderCte(highlightedId);
  });

  btnLineage.addEventListener("click", function () { setView("lineage"); });
  btnCte.addEventListener("click", function () { setView("cte"); });

  // ---- forward hook: window.setView(kind) (cute-dbt#105) -------------------
  // Programmatic view switch for embedding hosts — the SAME internal
  // transition the toggle buttons drive, with the V3 vocabulary
  // ("lineage" | "cte") and the same gates (the CTE arm needs a
  // highlight; fail-open no-op otherwise). Rebinds the inert default
  // explore-lineage.js declared. Never a commit: this file still never
  // writes data-selected-model and never posts a bridge event.
  window.setView = function (kind) {
    if (kind !== "lineage" && kind !== "cte") return false;
    setView(kind);
    return view === kind;
  };

  // Public seam: the headless suites drive + observe the toggle through
  // this (the window.CuteExploreLineage precedent).
  window.CuteExploreCte = {
    activeView: function () { return view; },
    cyInstance: function () { return cy; },
    renderedModelId: function () { return renderedModelId; }
  };
})();
/* end of cute-dbt explore CTE engine v1 (cute-dbt#102) */
