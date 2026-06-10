/* cute-dbt cytoscape DAG engine v1 (cute-dbt#180)
   ----------------------------------------------------------------------------
   The OPT-IN interactive CTE-DAG engine behind the settings-panel
   Mermaid <-> Cytoscape picker (Mermaid stays the static default; theme.js
   persists the choice under cute-dbt.appearance.v1). Visual parity with the
   Mermaid DAG: same nodes, roles, edge-type colors, selected-pink — every
   color arrives through the window.cuteDagPalette() hook interaction.js
   exposes, so this file carries NO palette table of its own (the
   edge-vocab-completeness CI gate greps interaction.js's
   JOIN_COLORS_LIGHT/_DARK as the single source; a local copy would dodge it).
   The hook returns the dark-bg lightness variant when html.dark is on — the
   handoff's documented dark-theme bug (near-black `from` edges vanishing on
   dark) cannot recur here because both engines read the same dark palette.

   Init hygiene (the cute-dbt#180 / ADR-4 amendment contract):
   - node labels are CANVAS TEXT (Cytoscape's default canvas renderer) —
     XSS-safe by construction: hostile manifest-derived names are drawn as
     glyphs, never parsed as HTML. No cytoscape-node-html-label, ever.
   - explicit non-webfont system fontFamily (zero-egress: no font fetch).
   - NO layout plugin (no cytoscape-dagre, never the EPL-licensed
     cytoscape-elk) and NO workers: positions come from the first-party
     longest-path column layout below, fed to the built-in `preset` layout.
   - handlers are bound from THIS file to rendered elements — never click
     directives executing payload data.
   - per-click interaction mutates classes IN PLACE and must NOT re-call a
     render entry point (the spike's no-renderDag-per-click rule): a rebuild
     would reset pan/zoom and wipe the lineage highlight. Full rebuild is
     reserved for model-switch, engine-switch and theme re-tint.
   - the hover card + node-detail panel are DOM (canvas safety covers labels
     only): every dynamic string lands via textContent, never innerHTML.

   First-party, NOT vendored: lives at templates/cyto-dag.js, embedded at
   compile time via asset_embed::CYTO_DAG_JS (include_str!) and interpolated
   inline by the askama renderer. Banner-pin + end-of-file-sentinel tests in
   src/adapters/asset_embed.rs guard the include. */
(function () {
  "use strict";

  var cy = null;
  var card = null;

  // ---- longest-path column layout (parity with Mermaid `graph LR`) -------
  // Kahn topological order relaxing longest path; each node's depth is its
  // column. The CTE engine guarantees an acyclic graph, but any node a cycle
  // would leave unseen simply keeps depth 0 (defensive, never throws).
  // Every id-keyed map is prototype-less (Object.create(null)) so a CTE
  // legitimately named `toString`/`constructor` can never collide with a
  // built-in prototype property (gemini review, PR #192).
  function computeDepths(nodes, edges) {
    var indeg = Object.create(null), adj = Object.create(null);
    nodes.forEach(function (n) { indeg[n.id] = 0; adj[n.id] = []; });
    edges.forEach(function (e) {
      if (adj[e.from] !== undefined && indeg[e.to] !== undefined) {
        adj[e.from].push(e.to);
        indeg[e.to] += 1;
      }
    });
    var depth = Object.create(null);
    nodes.forEach(function (n) { depth[n.id] = 0; });
    var queue = nodes.filter(function (n) { return indeg[n.id] === 0; })
      .map(function (n) { return n.id; });
    var deg = Object.create(null);
    nodes.forEach(function (n) { deg[n.id] = indeg[n.id]; });
    while (queue.length) {
      var u = queue.shift();
      adj[u].forEach(function (v) {
        if (depth[v] < depth[u] + 1) depth[v] = depth[u] + 1;
        deg[v] -= 1;
        if (deg[v] === 0) queue.push(v);
      });
    }
    return depth;
  }

  function layoutPositions(nodes, edges) {
    var depth = computeDepths(nodes, edges);
    var cols = Object.create(null);
    nodes.forEach(function (n) {
      var d = depth[n.id] || 0;
      (cols[d] = cols[d] || []).push(n.id);
    });
    var colGap = 210, rowGap = 86, pos = Object.create(null);
    var maxRows = 1;
    Object.keys(cols).forEach(function (k) {
      if (cols[k].length > maxRows) maxRows = cols[k].length;
    });
    Object.keys(cols).forEach(function (d) {
      var ids = cols[d];
      var colH = (ids.length - 1) * rowGap;
      var offset = ((maxRows - 1) * rowGap - colH) / 2;
      ids.forEach(function (id, i) {
        pos[id] = { x: (+d) * colGap, y: offset + i * rowGap };
      });
    });
    return pos;
  }

  // ---- elements off the SAME DagPayload buildMermaidSource consumes ------
  function buildElements(m) {
    var dag = m && m.dag;
    if (!dag || !dag.nodes || !dag.nodes.length) return null;
    var pos = layoutPositions(dag.nodes, dag.edges || []);
    var els = [];
    dag.nodes.forEach(function (n) {
      els.push({
        group: "nodes",
        // The element id is the stable node id (cute-dbt#155). It is only
        // ever resolved via cy.getElementById(...) — never string-concatenated
        // into a selector — so a selector-hostile id is inert by construction.
        data: { id: n.id, label: String(n.label || n.id), role: n.role || "transform" },
        position: pos[n.id] || { x: 0, y: 0 }
      });
    });
    (dag.edges || []).forEach(function (e, i) {
      els.push({
        group: "edges",
        data: { id: "e" + i, source: e.from, target: e.to, etype: e.edge_type || "from" }
      });
    });
    return els;
  }

  function styleSheet() {
    // The palette hook is REQUIRED (interaction.js defines it before this
    // file parses). It returns the active light/dark edge palette + the
    // role fills/strokes/shapes + nodeText + selected — keeping both DAG
    // engines on the one gated color source.
    var pal = window.cuteDagPalette();
    var EDGES = pal.edges, ROLES = pal.roles;
    var sheet = [
      { selector: "node", style: {
        "label": "data(label)",
        "text-valign": "center",
        "text-halign": "center",
        // Explicit non-webfont system stack — zero-egress, ADR-4 contract.
        "font-family": 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
        "font-size": 13,
        "color": pal.nodeText,
        "text-wrap": "wrap",
        "text-max-width": 150,
        "width": "label",
        "height": "label",
        "padding": "10px",
        "border-width": 1.6,
        "background-color": ROLES.transform.fill,
        "border-color": ROLES.transform.stroke,
        "shape": "rectangle",
        "transition-property": "opacity",
        "transition-duration": 120
      }},
      { selector: 'node[role = "import"]', style: {
        "background-color": ROLES.import.fill,
        "border-color": ROLES.import.stroke,
        "shape": "round-rectangle"
      }},
      { selector: 'node[role = "transform"]', style: {
        "background-color": ROLES.transform.fill,
        "border-color": ROLES.transform.stroke,
        "shape": "rectangle"
      }},
      { selector: 'node[role = "final"]', style: {
        "background-color": ROLES.final.fill,
        "border-color": ROLES.final.stroke,
        "shape": "hexagon",
        "border-width": 2,
        "padding": "14px"
      }},
      { selector: "edge", style: {
        "curve-style": "bezier",
        "width": 1.8,
        "target-arrow-shape": "triangle",
        "line-color": pal.fallbackEdge,
        "target-arrow-color": pal.fallbackEdge,
        "arrow-scale": 1.1,
        "transition-property": "opacity",
        "transition-duration": 120
      }}
    ];
    Object.keys(EDGES).forEach(function (k) {
      var dashed = (k === "union_all" || k === "union_distinct");
      var style = { "line-color": EDGES[k], "target-arrow-color": EDGES[k] };
      if (dashed) {
        style["line-style"] = "dashed";
        style["line-dash-pattern"] = [6, 3];
      }
      sheet.push({ selector: 'edge[etype = "' + k + '"]', style: style });
    });
    sheet.push({ selector: ".dim", style: { "opacity": 0.18 } });
    sheet.push({ selector: "node.sel", style: {
      "border-color": pal.selected, "border-width": 4, "opacity": 1
    }});
    sheet.push({ selector: "edge.trace", style: { "opacity": 1, "width": 2.6 } });
    sheet.push({ selector: "node.trace", style: { "opacity": 1 } });
    return sheet;
  }

  // ---- in-place interaction (never a re-render) ---------------------------
  function clearHighlight() {
    if (!cy) return;
    cy.elements().removeClass("dim sel trace");
  }

  function traceLineage(node) {
    clearHighlight();
    var lineage = node.union(node.predecessors()).union(node.successors());
    cy.elements().addClass("dim");
    lineage.removeClass("dim").addClass("trace");
    node.removeClass("dim").addClass("sel");
  }

  // Drive the report's shared node selection (the Inspect pane) through the
  // hook interaction.js exposes. The hook updates the panel ONLY — it must
  // never re-render the DAG (the no-renderDag-per-click rule).
  function selectReportNode(id) {
    if (typeof window.__cuteSelectNode === "function") window.__cuteSelectNode(id);
  }

  // One hover-card row: <div class="nc-row"><b>k</b> <span class="nc-v">v</span></div>.
  // Built with createElement + textContent ONLY — no innerHTML anywhere on
  // this DOM surface (the B-assertion: DOM stays escape-disciplined
  // regardless of the canvas-label safety).
  function cardRow(key, cls, value) {
    var row = document.createElement("div");
    row.className = "nc-row";
    var b = document.createElement("b");
    b.textContent = key;
    var span = document.createElement("span");
    span.className = cls;
    span.textContent = value;
    row.appendChild(b);
    row.appendChild(document.createTextNode(" "));
    row.appendChild(span);
    return row;
  }

  function showCard(node) {
    if (!card) return;
    var d = node.data();
    var pal = window.cuteDagPalette();
    while (card.firstChild) card.removeChild(card.firstChild);
    var id = document.createElement("span");
    id.className = "nc-id";
    id.textContent = d.id;
    card.appendChild(id);
    card.appendChild(cardRow("role", "nc-role", pal.roleLabels[d.role] || d.role));
    card.appendChild(cardRow("name", "nc-name", d.label));
    // Reveal BEFORE measuring (gemini review, PR #192): the chassis hides
    // the card via opacity (so offsetWidth stays real today), but ordering
    // the class first keeps the clamp correct even if the hide ever
    // becomes display-based.
    card.classList.add("is-visible");
    var rp = node.renderedPosition();
    var host = cy.container();
    var x = Math.min(rp.x + 14, host.clientWidth - card.offsetWidth - 8);
    var y = Math.min(rp.y + 12, host.clientHeight - card.offsetHeight - 8);
    card.style.left = Math.max(8, x) + "px";
    card.style.top = Math.max(8, y) + "px";
  }
  function hideCard() {
    if (card) card.classList.remove("is-visible");
  }

  // ---- render / destroy (model-switch, engine-switch, theme re-tint) -----
  function destroy() {
    if (cy) { cy.destroy(); cy = null; }
    hideCard();
  }

  function render(m, selectedNodeId) {
    var container = document.querySelector(".cte-dag-cyto .cyto-canvas");
    card = document.querySelector(".cte-dag-cyto .cyto-node-card");
    if (!container) return;
    destroy();
    if (typeof window.cytoscape !== "function") {
      container.textContent = "Cytoscape engine unavailable.";
      return;
    }
    var els = buildElements(m);
    if (!els) {
      container.textContent = "(no DAG available)";
      return;
    }
    container.textContent = "";

    cy = window.cytoscape({
      container: container,
      elements: els,
      style: styleSheet(),
      layout: { name: "preset", fit: true, padding: 28 },
      minZoom: 0.35,
      maxZoom: 2.2,
      boxSelectionEnabled: false,
      autoungrabify: false
    });

    // Handlers bound ONCE per build, from our JS, to rendered elements.
    // Tap = select + lineage highlight, all in-place class mutation.
    cy.on("tap", "node", function (evt) {
      traceLineage(evt.target);
      selectReportNode(evt.target.id());
    });
    cy.on("tap", function (evt) { if (evt.target === cy) clearHighlight(); });
    cy.on("mouseover", "node", function (evt) { showCard(evt.target); });
    cy.on("mouseout", "node", hideCard);
    cy.on("pan zoom drag", hideCard);

    // Restore the report's current selection (parity with the Mermaid
    // `selected` classDef) without waiting for a click.
    if (selectedNodeId) {
      var sel = cy.getElementById(String(selectedNodeId));
      if (sel && sel.length) traceLineage(sel);
    }

    // Re-fit after first paint — the host may have been hidden (0-sized)
    // when the engine flip revealed it this same frame. `fit(28)` is the
    // documented number-as-padding form (gemini review, PR #192).
    window.requestAnimationFrame(function () { if (cy) cy.fit(28); });
  }

  // Public seam: interaction.js's renderDag() dispatcher drives render/
  // destroy; the cyInstance getter is the headless-test seam (mirrors the
  // window.__cute* seam precedent).
  window.CuteCyto = {
    render: render,
    destroy: destroy,
    cyInstance: function () { return cy; }
  };
})();
/* end of cute-dbt cytoscape DAG engine v1 (cute-dbt#180) */
