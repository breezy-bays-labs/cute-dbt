/* cute-dbt explore lineage engine v1 (cute-dbt#101)
   ----------------------------------------------------------------------------
   The interactive model-lineage engine for the explore page's dag.html —
   Cytoscape UMD core + the cytoscape-dagre left-to-right rank layout over
   the server-built `explore-dag-data` LineagePayload carrier (nodes =
   models, edges = FORWARD dependency edges only; this file traverses both
   directions itself).

   Interaction model (epic cute-dbt#99 — HIGHLIGHT vs FOCUS):
   - click a node OR select a fuzzy-search match  -> HIGHLIGHT: emphasize
     the node + its full transitive upstream+downstream lineage, dim the
     complement. In-page and exploratory — it never writes the
     external-drive signal.
   - Space on the highlighted model -> FOCUS COMMIT: center the node AND
     write `document.body.dataset.selectedModel`. The commit fires ONLY on
     this deliberate keypress — never on hover/click/search.
   Keyboard-focus wiring (the cute-dbt#101 hard AC): a search-select blurs
   the input and moves focus to the canvas host, and the Space handler is
   gated on "no typing context focused" + calls preventDefault() — so Space
   commits when the canvas owns focus and never types into the search box
   or scrolls the page.

   Fuzzy search is HAND-ROLLED and dependency-free (no new JS asset):
   case-insensitive subsequence match, scored toward word boundaries and
   contiguous runs, top-ranked matches in a listbox (ArrowUp/ArrowDown +
   Enter + Escape + pointer).

   Init hygiene (the ADR-4-amendment contract, explore-page variant):
   - node labels are CANVAS TEXT (Cytoscape's default canvas renderer) —
     XSS-safe by construction: hostile manifest-derived names draw as
     glyphs, never parse as HTML. No cytoscape-node-html-label, ever.
   - explicit non-webfont system fontFamily (zero-egress: no font fetch).
   - the dagre layout runs IN-THREAD (cytoscape-dagre UMD, dagre bundled
     internally; never the EPL-licensed cytoscape-elk) and NO workers.
   - no custom wheelSensitivity and no `width: label` style (both emit
     Cytoscape console warnings); node widths are measured up front via
     canvas measureText and carried as data(w).
   - handlers are bound from THIS file to rendered elements — never click
     directives executing payload data.
   - interaction mutates classes IN PLACE and never re-calls a render
     entry point: a rebuild would reset pan/zoom and wipe the highlight.
   - search-result rows are DOM built with createElement + textContent
     ONLY — no innerHTML anywhere on this surface.

   First-party, NOT vendored: lives at templates/explore-lineage.js,
   embedded at compile time via asset_embed::EXPLORE_LINEAGE_JS
   (include_str!) and interpolated inline by the askama renderer.
   Banner-pin + end-of-file-sentinel tests in src/adapters/asset_embed.rs
   guard the include. */
(function () {
  "use strict";

  var data = JSON.parse(document.getElementById("explore-dag-data").textContent);
  var host = document.querySelector(".lineage-canvas");
  var empty = document.querySelector(".lineage-empty");
  var input = document.querySelector(".lineage-search-input");
  var list = document.querySelector(".lineage-search-results");

  if (!data.nodes.length) {
    if (host) host.hidden = true;
    var toolbar = document.querySelector(".lineage-toolbar");
    if (toolbar) toolbar.hidden = true;
    empty.hidden = false;
    return;
  }

  // ---- measured node widths (no deprecated `width: label`) ---------------
  var FONT_STACK = 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace';
  var MAX_TEXT_WIDTH = 274;
  var measureCtx = document.createElement("canvas").getContext("2d");
  measureCtx.font = "13px " + FONT_STACK;
  function nodeWidth(name, badge) {
    var widest = Math.max(
      measureCtx.measureText(name).width,
      measureCtx.measureText(badge).width
    );
    return Math.min(300, Math.ceil(widest) + 26);
  }
  // Manual single-line ellipsis (the label is two lines under
  // `text-wrap: wrap` for the cute-dbt#103 badge, so Cytoscape's
  // one-line `ellipsis` mode no longer applies — truncate the NAME line
  // ourselves the same canvas-measured way).
  function ellipsize(text) {
    if (measureCtx.measureText(text).width <= MAX_TEXT_WIDTH) return text;
    var out = text;
    while (out.length > 1 && measureCtx.measureText(out + "…").width > MAX_TEXT_WIDTH) {
      out = out.slice(0, -1);
    }
    return out + "…";
  }

  // ---- elements off the LineagePayload carrier ----------------------------
  var elements = [];
  data.nodes.forEach(function (n) {
    // cute-dbt#103 — the per-node test-count badge rides the label's
    // second line as CANVAS TEXT (same XSS-by-construction posture as
    // the name: glyphs, never HTML). The badge string is composed
    // server-side ("N data-tests · M unit-tests", 0/0 explicit) — these
    // are manifest test-count facts, not check-engine output.
    var name = ellipsize(String(n.name || n.id));
    var badge = String(n.badge || "");
    elements.push({
      group: "nodes",
      // The element id is the full manifest node id. It is only ever
      // resolved via cy.getElementById(...) — never string-concatenated
      // into a selector — so a selector-hostile id is inert (the
      // cute-dbt#155 / #180 precedent).
      data: {
        id: n.id,
        label: badge ? name + "\n" + badge : name,
        w: nodeWidth(name, badge),
        notCompiled: n.not_compiled ? 1 : 0
      }
    });
  });
  data.edges.forEach(function (e, i) {
    elements.push({
      group: "edges",
      data: { id: "e" + i, source: e.from, target: e.to }
    });
  });

  // ---- style (canvas text, system fonts, V1 palette parity) --------------
  var styleSheet = [
    { selector: "node", style: {
      "label": "data(label)",
      "text-valign": "center",
      "text-halign": "center",
      "font-family": FONT_STACK,
      "font-size": 13,
      "color": "#1c1c1f",
      // cute-dbt#103 — two-line labels (name + test-count badge): wrap
      // honors the explicit \n; the name line is pre-ellipsized above
      // (canvas-measured), so the max-width is only a backstop.
      "text-wrap": "wrap",
      "text-max-width": 280,
      "width": "data(w)",
      "height": 46,
      "shape": "round-rectangle",
      "background-color": "#e8f1f8",
      "border-width": 1.6,
      "border-color": "#0072B2",
      "transition-property": "opacity",
      "transition-duration": 120
    }},
    { selector: "node[notCompiled = 1]", style: {
      "background-color": "#f4f4f5",
      "border-color": "#b07400",
      "border-style": "dashed"
    }},
    { selector: "edge", style: {
      "curve-style": "bezier",
      "width": 1.8,
      "line-color": "#8b8b96",
      "target-arrow-shape": "triangle",
      "target-arrow-color": "#8b8b96",
      "arrow-scale": 1.1,
      "transition-property": "opacity",
      "transition-duration": 120
    }},
    { selector: ".dim", style: { "opacity": 0.15 } },
    { selector: "node.trace", style: { "opacity": 1 } },
    { selector: "edge.trace", style: {
      "opacity": 1, "width": 2.6,
      "line-color": "#0072B2", "target-arrow-color": "#0072B2"
    }},
    { selector: "node.sel", style: {
      "border-color": "#c2186b", "border-width": 4, "opacity": 1
    }}
  ];

  var cy = window.cytoscape({
    container: host,
    elements: elements,
    style: styleSheet,
    // Left-to-right ranks via the vendored cytoscape-dagre extension
    // (auto-registered against window.cytoscape at parse time).
    layout: { name: "dagre", rankDir: "LR", nodeSep: 18, rankSep: 90, edgeSep: 12, fit: true, padding: 24 },
    minZoom: 0.2,
    maxZoom: 2.5,
    boxSelectionEnabled: false
  });

  // ---- HIGHLIGHT (click / search-select; in-place, never a commit) --------
  var highlighted = null;

  // cute-dbt#102 — page-local highlight observer hook: the CTE-view
  // engine (explore-cte.js) gates + retargets its view off this event.
  // It is NOT the external-drive signal: data-selected-model stays
  // Space-commit-only (the one write site below).
  function notifyHighlight(id) {
    document.dispatchEvent(
      new CustomEvent("cute-explore-highlight", { detail: { id: id } })
    );
  }

  function clearHighlight() {
    cy.elements().removeClass("dim sel trace");
    highlighted = null;
    notifyHighlight(null);
  }

  function highlightNode(node) {
    cy.batch(function () {
      cy.elements().removeClass("dim sel trace");
      var lineage = node.union(node.predecessors()).union(node.successors());
      cy.elements().addClass("dim");
      lineage.removeClass("dim").addClass("trace");
      node.removeClass("dim").addClass("sel");
    });
    highlighted = node;
    notifyHighlight(node.id());
  }

  // ---- FOCUS COMMIT (Space only — the one selectedModel write site) -------
  function commitFocus() {
    if (!highlighted) return;
    cy.center(highlighted);
    document.body.dataset.selectedModel = highlighted.id();
  }

  cy.on("tap", "node", function (evt) { highlightNode(evt.target); });
  cy.on("tap", function (evt) { if (evt.target === cy) clearHighlight(); });

  // ---- Space commit gating (the hard AC's half b) -------------------------
  function inTypingContext(el) {
    return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
  }
  document.addEventListener("keydown", function (ev) {
    if (ev.key !== " " && ev.key !== "Spacebar") return;
    // The search input owns Space while focused — typing must win.
    if (inTypingContext(document.activeElement)) return;
    // Never scroll the page; commit (a no-op without a highlight).
    ev.preventDefault();
    commitFocus();
  });

  // ---- hand-rolled fuzzy search (dependency-free) --------------------------
  // Case-insensitive subsequence match. Score rewards word-boundary hits
  // (start of name or after `_`) and contiguous runs; a light length
  // penalty prefers tighter names on ties. Returns null on no match.
  function fuzzyScore(query, name) {
    var q = query.toLowerCase();
    var n = name.toLowerCase();
    if (!q) return null;
    var qi = 0, score = 0, run = 0;
    for (var i = 0; i < n.length && qi < q.length; i++) {
      if (n.charAt(i) === q.charAt(qi)) {
        run += 1;
        score += run;
        if (i === 0 || n.charAt(i - 1) === "_") score += 3;
        qi += 1;
      } else {
        run = 0;
      }
    }
    if (qi < q.length) return null;
    return score - n.length * 0.01;
  }

  function rankedMatches(query) {
    var out = [];
    data.nodes.forEach(function (n) {
      var s = fuzzyScore(query, n.name);
      if (s !== null) out.push({ id: n.id, name: n.name, score: s });
    });
    out.sort(function (a, b) { return b.score - a.score || (a.name < b.name ? -1 : 1); });
    return out.slice(0, 8);
  }

  var matches = [];
  var activeIndex = 0;

  function closeList() {
    list.hidden = true;
    input.setAttribute("aria-expanded", "false");
    matches = [];
    activeIndex = 0;
    while (list.firstChild) list.removeChild(list.firstChild);
  }

  function paintActive() {
    for (var i = 0; i < list.children.length; i++) {
      list.children[i].classList.toggle("is-active", i === activeIndex);
    }
  }

  function renderList() {
    while (list.firstChild) list.removeChild(list.firstChild);
    if (!matches.length) {
      var none = document.createElement("li");
      none.className = "lineage-search-none";
      none.textContent = "no matching model";
      list.appendChild(none);
    }
    matches.forEach(function (m, i) {
      var li = document.createElement("li");
      li.setAttribute("role", "option");
      li.dataset.modelId = m.id;
      // textContent ONLY — a hostile model name lands as glyphs here too.
      li.textContent = m.name;
      // mousedown (not click) so selection wins the race against the
      // input's blur.
      li.addEventListener("mousedown", function (ev) {
        ev.preventDefault();
        selectMatch(i);
      });
      list.appendChild(li);
    });
    activeIndex = 0;
    paintActive();
    list.hidden = false;
    input.setAttribute("aria-expanded", "true");
  }

  // A search-select HIGHLIGHTS (never commits) and hands focus to the
  // canvas (the hard AC's half a) so the follow-up Space lands on the
  // document handler, not the input.
  function selectMatch(index) {
    var m = matches[index];
    if (!m) return;
    var node = cy.getElementById(m.id);
    if (!node || !node.length) return;
    closeList();
    highlightNode(node);
    input.blur();
    host.focus();
  }

  input.addEventListener("input", function () {
    var q = input.value.trim();
    if (!q) { closeList(); return; }
    matches = rankedMatches(q);
    renderList();
  });

  input.addEventListener("keydown", function (ev) {
    if (ev.key === "ArrowDown") {
      ev.preventDefault();
      if (matches.length) { activeIndex = (activeIndex + 1) % matches.length; paintActive(); }
    } else if (ev.key === "ArrowUp") {
      ev.preventDefault();
      if (matches.length) { activeIndex = (activeIndex + matches.length - 1) % matches.length; paintActive(); }
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      if (matches.length) selectMatch(activeIndex);
    } else if (ev.key === "Escape") {
      closeList();
    }
  });

  input.addEventListener("blur", function () {
    // Let an in-flight option mousedown finish first (it preventDefaults,
    // so blur only fires here for true focus-leaves).
    closeList();
  });

  // Public seam: the headless suites read the live instance + the
  // highlight through it (mirrors the report's window.CuteCyto precedent).
  window.CuteExploreLineage = {
    cyInstance: function () { return cy; },
    highlightedId: function () { return highlighted ? highlighted.id() : null; }
  };
})();
/* end of cute-dbt explore lineage engine v1 (cute-dbt#101) */
