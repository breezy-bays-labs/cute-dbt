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

   Model detail (cute-dbt#104 — epic #99 V5):
   - HIGHLIGHT opens the model-detail card (.model-detail-card):
     description, materialization, tags, meta, the resolved GRAIN (with
     its source + every detected signal; "unknown" rendered explicitly)
     and the declared columns. Clearing the highlight hides it.
   - hovering a node shows a TRANSIENT key-facts tooltip
     (.lineage-tooltip). The tooltip never changes the highlighted model
     and never writes document.body.dataset.selectedModel — commitFocus
     below stays the single write site.
   - both surfaces are DOM-built with createElement + textContent ONLY
     (no innerHTML, no string-interpolated markup): hostile
     manifest-derived values land as glyphs, the same posture as the
     canvas labels and the search rows. All display strings (badge,
     grain value/source, meta values) are composed server-side in Rust —
     this engine stays a pure renderer (the cute-dbt#138 posture).

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

   External-drive contract (cute-dbt#105 — the host-bridge respec):
   - window.cuteDbtContract mirrors the server-rendered
     data-cute-dbt-contract version attribute on <body> (the single
     source; this engine only reads it back).
   - window.focusModel(id) is the host's forward hook: highlight +
     center ONLY — no data-selected-model write and no bridge commit
     event (the NO-ECHO rule: a host pushing editor-sync focus must
     never hear its own push back as a commit).
   - the Space focus commit is DUAL-BOUND: it writes the
     data-selected-model attribute (the file:// / cmux browser-pane
     binding) AND, iff a host bridge registered at boot, posts the
     versioned commit event via postMessage. Registration is
     detection-based (acquireVsCodeApi presence, or an injected
     window.cuteDbtHostBridge) and INERT standalone — presence checks
     only, zero behavior change on plain file://, zero-egress
     unaffected (postMessage to a host is in-process message passing,
     never a network request).
   - window.setView(kind) is rebound by the CTE engine
     (explore-cte.js); the inert default here keeps the hook callable
     on every page (fail-open).

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
  var card = document.querySelector(".model-detail-card");
  var tooltip = document.querySelector(".lineage-tooltip");

  // cute-dbt#104 — payload nodes by id, for the detail card + tooltip.
  var nodeById = {};
  data.nodes.forEach(function (n) { nodeById[n.id] = n; });

  // ---- external-drive contract surface (cute-dbt#105) ----------------------
  // The version is server-rendered as <body data-cute-dbt-contract>
  // (readable by attribute-only observers without executing JS); this
  // global mirrors it for JS consumers — the attribute is the single
  // source, so the two surfaces cannot drift. Declared BEFORE the
  // empty-manifest return with inert hook defaults (rebound by the
  // booted engines below) so driving an empty page is a fail-open
  // no-op, never a TypeError.
  var CONTRACT_VERSION = String(document.body.dataset.cuteDbtContract || "");
  window.cuteDbtContract = {
    version: CONTRACT_VERSION,
    commitEventType: "cute-dbt/commit",
    attribute: "data-selected-model",
    hooks: ["focusModel", "setView"],
    views: ["lineage", "cte"]
  };
  window.focusModel = function () { return false; };
  window.setView = function () { return false; };

  // Host-bridge detection (cute-dbt#105): registration is
  // detection-based and INERT standalone — presence checks only, no
  // call that could touch the network. A VS Code webview exposes
  // acquireVsCodeApi (called exactly once per its contract); any other
  // embedding host injects window.cuteDbtHostBridge ({ postMessage })
  // before this script parses. On plain file:// neither exists and
  // hostBridge stays null — zero behavior change.
  var hostBridge = null;
  if (typeof window.acquireVsCodeApi === "function") {
    hostBridge = window.acquireVsCodeApi();
  } else if (
    window.cuteDbtHostBridge &&
    typeof window.cuteDbtHostBridge.postMessage === "function"
  ) {
    hostBridge = window.cuteDbtHostBridge;
  }

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
  // cute-dbt#253 — per-type horizontal padding: non-rectangular shapes
  // (ellipse / barrel / round-tag) inscribe less text than a rectangle,
  // so their labels need wider boxes to stay un-clipped.
  var TYPE_PAD = { model: 26, snapshot: 34, seed: 44, source: 44, exposure: 44 };
  function nodeWidth(name, badge, type) {
    var widest = Math.max(
      measureCtx.measureText(name).width,
      measureCtx.measureText(badge).width
    );
    var pad = TYPE_PAD[type] || 26;
    return Math.min(300, Math.ceil(widest) + pad);
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
    // cute-dbt#253 — the typed-node vocabulary (model / snapshot / seed
    // / source / exposure), the style/shape hook below. Pre-#253
    // payloads carried models only; default accordingly.
    var type = String(n.node_type || "model");
    elements.push({
      group: "nodes",
      // The element id is the full manifest node id. It is only ever
      // resolved via cy.getElementById(...) — never string-concatenated
      // into a selector — so a selector-hostile id is inert (the
      // cute-dbt#155 / #180 precedent).
      data: {
        id: n.id,
        label: badge ? name + "\n" + badge : name,
        w: nodeWidth(name, badge, type),
        type: type,
        notCompiled: n.not_compiled ? 1 : 0,
        // cute-dbt#106 — PR-diff change context. The payload omits the
        // key entirely on a no-context render, so `n.changed` is
        // undefined there and every node maps to 0.
        changed: n.changed ? 1 : 0,
        // cute-dbt#345 — the focused-macro-DAG role ("user" / "downstream").
        // Present ONLY on the macro.html carrier (the dag.html payload omits
        // it, so it is undefined there → "" → no macro-role class). Drives
        // the boot-time .macro-user / .macro-downstream class below.
        macroRole: String(n.macro_role || "")
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
    // cute-dbt#253 — the typed-node vocabulary. Redundant coding (shape
    // AND canvas-paired color) so the typing never rides color alone;
    // the dag.html legend chips mirror these exact fills/strokes (the
    // report's fixed-DAG-palette posture — identical in every theme).
    // The explicit model selector restates the base style so the
    // node-vocab completeness guard greps one selector per wire key.
    { selector: 'node[type = "model"]', style: {
      "shape": "round-rectangle",
      "background-color": "#e8f1f8",
      "border-color": "#0072B2"
    }},
    { selector: 'node[type = "snapshot"]', style: {
      "shape": "cut-rectangle",
      "background-color": "#f1ecf9",
      "border-color": "#7b5ea7"
    }},
    { selector: 'node[type = "seed"]', style: {
      "shape": "barrel",
      "background-color": "#e6f4f2",
      "border-color": "#00756d"
    }},
    { selector: 'node[type = "source"]', style: {
      "shape": "ellipse",
      "background-color": "#eef7ee",
      "border-color": "#1f8a5b"
    }},
    { selector: 'node[type = "exposure"]', style: {
      "shape": "round-tag",
      "background-color": "#f4f4f5",
      "border-color": "#6b6b76"
    }},
    { selector: "node[notCompiled = 1]", style: {
      "background-color": "#f4f4f5",
      "border-color": "#b07400",
      "border-style": "dashed"
    }},
    // cute-dbt#106 — the "changed in this diff" context treatment: an
    // amber UNDERLAY glow behind the node. A deliberately SEPARATE
    // visual channel from the highlight vocabulary (border color/width
    // = .sel, opacity = .dim/.trace) and from the not-compiled dashed
    // border, so all of them compose on one node without conflicting —
    // a changed node that is also highlighted shows both, and a changed
    // node outside a highlighted lineage dims with its glow (context
    // never fights emphasis).
    { selector: "node[changed = 1]", style: {
      "underlay-color": "#e69f00",
      "underlay-opacity": 0.45,
      "underlay-padding": 5,
      "underlay-shape": "round-rectangle"
    }},
    // cute-dbt#345 — the focused-macro-DAG roles (macro.html only). A
    // SEPARATE visual channel from the highlight vocabulary (.dim/.trace/
    // .sel) and the change-context underlay, so all compose: clicking a
    // node still dims the complement, and clearing returns to this static
    // macro-role dim (see clearHighlight). .macro-user EMPHASIZES the macro
    // callers (a stronger blue fill + thicker border that pops above the
    // base node style); .macro-downstream is a SOFT static dim (0.45,
    // softer than .dim's 0.15) so the context stays legible but recessive.
    // Applied once at boot from data(macroRole) — never per click.
    { selector: "node.macro-user", style: {
      "background-color": "#d4e7f5",
      "border-color": "#0072B2",
      "border-width": 3
    }},
    { selector: "node.macro-downstream", style: { "opacity": 0.45 } },
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

  // cute-dbt#345 — the focused-macro-DAG role classes, applied ONCE at
  // boot from data(macroRole). On dag.html the key is absent (macroRole
  // === "") so this is a no-op; on macro.html it emphasizes the macro
  // callers (.macro-user) and dims the downstream closure
  // (.macro-downstream). These static classes are NEVER removed by
  // clearHighlight (it touches only dim/sel/trace), so clearing an
  // interactive highlight returns to this macro-role dim — never to
  // all-visible (the interactive .dim/.trace composes on top, in place).
  cy.batch(function () {
    cy.nodes('[macroRole = "user"]').addClass("macro-user");
    cy.nodes('[macroRole = "downstream"]').addClass("macro-downstream");
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
    hideDetailCard();
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
    renderDetailCard(node.id());
    notifyHighlight(node.id());
  }

  // ---- model-detail card + hover tooltip (cute-dbt#104) -------------------
  // Both surfaces are DOM-built with createElement + textContent ONLY —
  // hostile manifest-derived values land as glyphs, never markup. All
  // display strings (badge, grain value/source/origins, meta values)
  // arrive pre-composed from Rust; this engine is a pure renderer.

  function el(tag, className, text) {
    var out = document.createElement(tag);
    if (className) out.className = className;
    if (text !== undefined && text !== null) out.textContent = String(text);
    return out;
  }

  function fact(dl, label, valueNode) {
    dl.appendChild(el("dt", null, label));
    var dd = document.createElement("dd");
    dd.appendChild(valueNode);
    dl.appendChild(dd);
  }

  function grainValueNode(grain) {
    var holder = document.createElement("span");
    if (!grain.known) {
      // The explicit-unknown rung — rendered as such, never guessed.
      holder.appendChild(el("span", "detail-grain-unknown", "unknown"));
      return holder;
    }
    holder.appendChild(el("code", null, grain.value));
    holder.appendChild(document.createTextNode(" "));
    holder.appendChild(el("span", "detail-grain-source", "(" + grain.source + ")"));
    if (grain.detected.length > 1) {
      var detected = el("ul", "detail-grain-detected", null);
      grain.detected.forEach(function (signal) {
        detected.appendChild(el(
          "li", null,
          signal.kind + ": " + signal.value + " — " + signal.origin
        ));
      });
      holder.appendChild(detected);
    }
    return holder;
  }

  function renderDetailCard(id) {
    var n = nodeById[id];
    if (!card || !n) return;
    var d = n.detail;
    // cute-dbt#253 — type-aware facts: code-bearing nodes-map types
    // carry files/grain; sources/exposures honestly omit what they
    // structurally cannot have.
    var type = String(n.node_type || "model");
    var codeBearing = type === "model" || type === "snapshot" || type === "seed";
    while (card.firstChild) card.removeChild(card.firstChild);

    card.appendChild(el("h2", null, n.name || n.id));
    if (type !== "model") {
      card.appendChild(el("span", "detail-type type-" + type, type));
    }
    if (n.not_compiled) {
      card.appendChild(el("span", "detail-notcompiled", "not compiled"));
    }
    // cute-dbt#106 — change context on the card (composes with the
    // not-compiled chip; both can show at once).
    if (n.changed) {
      card.appendChild(el("span", "detail-changed", "changed in this diff"));
    }
    card.appendChild(d.description
      ? el("p", "detail-description", d.description)
      : el("p", "detail-description detail-empty", "no description"));

    var facts = el("dl", "detail-facts", null);
    // The badge string is empty only on the cute-dbt#253 non-model
    // types (models always carry the explicit 0/0 line).
    fact(facts, "tests", n.badge
      ? document.createTextNode(n.badge)
      : el("span", "detail-empty", "none"));
    fact(facts, "materialized", d.materialized
      ? el("code", null, d.materialized)
      : el("span", "detail-empty", "not set"));
    // Grain is SQL-model semantics — sources/exposures have no grain
    // ladder to consult, so the row is omitted, not "unknown".
    if (codeBearing) fact(facts, "grain", grainValueNode(d.grain));
    var tagsNode = document.createElement("span");
    if (d.tags.length) {
      d.tags.forEach(function (tag) {
        tagsNode.appendChild(el("span", "detail-tag", tag));
      });
    } else {
      tagsNode.appendChild(el("span", "detail-empty", "none"));
    }
    fact(facts, "tags", tagsNode);
    card.appendChild(facts);

    if (d.meta.length) {
      card.appendChild(el("p", "detail-section-title", "meta"));
      var metaFacts = el("dl", "detail-facts", null);
      d.meta.forEach(function (entry) {
        fact(metaFacts, entry.key, el("span", "detail-meta-value", entry.value));
      });
      card.appendChild(metaFacts);
    }

    // cute-dbt#105 — read-only per-node file paths (the external-drive
    // contract's NodePathsPayload, surfaced for humans too). All values
    // are project-relative manifest facts; textContent only. Code-bearing
    // nodes-map types only (cute-dbt#253): a source/exposure carries no
    // per-node file paths in the payload, so an all-empty section would
    // be noise.
    if (codeBearing) {
      card.appendChild(el("p", "detail-section-title", "files"));
      var pathFacts = el("dl", "detail-facts detail-paths", null);
      fact(pathFacts, "sql", n.paths.sql
        ? el("code", null, n.paths.sql)
        : el("span", "detail-empty", "not in manifest"));
      fact(pathFacts, "schema yaml", n.paths.schema_yaml
        ? el("code", null, n.paths.schema_yaml)
        : el("span", "detail-empty", "none"));
      n.paths.unit_tests.forEach(function (t) {
        var holder = document.createElement("span");
        holder.appendChild(t.yaml
          ? el("code", null, t.yaml)
          : el("span", "detail-empty", "yaml not in manifest"));
        t.fixtures.forEach(function (fixture) {
          holder.appendChild(document.createTextNode(" "));
          holder.appendChild(el("code", "detail-path-fixtures", fixture));
        });
        fact(pathFacts, t.name, holder);
      });
      card.appendChild(pathFacts);
    }

    card.appendChild(el("p", "detail-section-title", "columns"));
    if (d.columns.length) {
      var table = el("table", "detail-columns", null);
      var head = document.createElement("tr");
      head.appendChild(el("th", null, "column"));
      head.appendChild(el("th", null, "type"));
      head.appendChild(el("th", null, "description"));
      table.appendChild(head);
      d.columns.forEach(function (column) {
        var row = document.createElement("tr");
        var name = document.createElement("td");
        name.appendChild(el("code", null, column.name));
        row.appendChild(name);
        row.appendChild(el("td", null, column.data_type || ""));
        row.appendChild(el("td", null, column.description || ""));
        table.appendChild(row);
      });
      card.appendChild(table);
    } else {
      card.appendChild(el("p", "detail-empty", "no declared columns"));
    }

    card.hidden = false;
  }

  function hideDetailCard() {
    if (!card) return;
    card.hidden = true;
    while (card.firstChild) card.removeChild(card.firstChild);
  }

  // The hover tooltip is TRANSIENT key facts only. It never touches the
  // highlight classes, the `highlighted` binding, or
  // document.body.dataset.selectedModel — commitFocus below stays the
  // single write site (pinned by the cute-dbt#101 headless test).
  function showTooltip(node) {
    var n = nodeById[node.id()];
    if (!tooltip || !n) return;
    var type = String(n.node_type || "model");
    while (tooltip.firstChild) tooltip.removeChild(tooltip.firstChild);
    tooltip.appendChild(el("span", "tooltip-name", n.name || n.id));
    // cute-dbt#253 — surface the non-model typing as a key fact; the
    // badge line is skipped when empty (non-model types without tests).
    if (type !== "model") {
      tooltip.appendChild(el("span", "tooltip-fact", "type: " + type));
    }
    if (n.badge) {
      tooltip.appendChild(el("span", "tooltip-fact", n.badge));
    }
    if (n.detail.materialized) {
      tooltip.appendChild(el("span", "tooltip-fact", "materialized: " + n.detail.materialized));
    }
    // Grain is SQL-model semantics (omitted for sources/exposures, the
    // detail-card rule).
    if (type === "model" || type === "snapshot" || type === "seed") {
      tooltip.appendChild(el("span", "tooltip-fact", "grain: " + n.detail.grain.value));
    }
    if (n.not_compiled) {
      tooltip.appendChild(el("span", "tooltip-fact", "not compiled"));
    }
    // cute-dbt#106 — change context is a key fact too (transient only;
    // the tooltip still never touches highlight or commit state).
    if (n.changed) {
      tooltip.appendChild(el("span", "tooltip-fact", "changed in this diff"));
    }
    var rp = node.renderedPosition();
    tooltip.style.left = rp.x + "px";
    tooltip.style.top = (rp.y - 32) + "px";
    tooltip.style.transform = "translate(-50%, -100%)";
    tooltip.hidden = false;
  }

  function hideTooltip() {
    if (!tooltip) return;
    tooltip.hidden = true;
    while (tooltip.firstChild) tooltip.removeChild(tooltip.firstChild);
  }

  // ---- FOCUS COMMIT (Space only — the one selectedModel write site) -------
  // Dual-bound since cute-dbt#105: the DOM attribute always writes (the
  // standalone file:// binding); the versioned bridge commit event
  // posts iff a host registered at boot. Both fire ONLY here — never on
  // hover/click/search/focusModel.
  function commitFocus() {
    if (!highlighted) return;
    cy.center(highlighted);
    document.body.dataset.selectedModel = highlighted.id();
    if (hostBridge) {
      var committed = nodeById[highlighted.id()];
      hostBridge.postMessage({
        type: "cute-dbt/commit",
        contractVersion: CONTRACT_VERSION,
        modelId: highlighted.id(),
        // The active view at commit time (the V3 toggle's state
        // vocabulary). Defensive fallback: the CTE engine parses after
        // this file, so it exists by any user-driven commit.
        view: window.CuteExploreCte ? window.CuteExploreCte.activeView() : "lineage",
        // The committed node's project-relative file paths — so a host
        // can open the files without re-parsing the payload carrier.
        paths: committed ? committed.paths : null
      });
    }
  }

  // ---- forward hook: window.focusModel(id) (cute-dbt#105) ------------------
  // Host-pushed editor sync — highlight + center, and NOTHING else: no
  // data-selected-model write, no bridge commit event (the no-echo
  // rule). The page-local cute-explore-highlight CustomEvent still
  // fires inside highlightNode — that is in-page wiring (the CTE arm's
  // gate/retarget), not the external-drive signal. An unknown id is a
  // fail-open no-op returning false.
  window.focusModel = function (id) {
    var node = cy.getElementById(String(id));
    if (!node || !node.length) return false;
    highlightNode(node);
    cy.center(node);
    return true;
  };

  cy.on("tap", "node", function (evt) { highlightNode(evt.target); });
  cy.on("tap", function (evt) { if (evt.target === cy) clearHighlight(); });

  // ---- tooltip wiring (hover = transient; never a highlight change) -------
  cy.on("mouseover", "node", function (evt) { showTooltip(evt.target); });
  cy.on("mouseout", "node", function () { hideTooltip(); });
  // Pan/zoom/drag move the anchor out from under the tooltip — hide it.
  cy.on("viewport", function () { hideTooltip(); });
  cy.on("drag", "node", function () { hideTooltip(); });
  cy.on("tap", function () { hideTooltip(); });

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
      none.textContent = "no matching node";
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
