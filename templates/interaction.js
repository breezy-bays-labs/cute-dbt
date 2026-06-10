/* cute-dbt report interaction engine v1 (cute-dbt#178)
   ----------------------------------------------------------------------------
   First-party, NOT vendored: this file lives at templates/interaction.js
   (beside the template it drives), embedded at compile time via
   asset_embed::INTERACTION_JS (include_str!) and interpolated inline by the
   askama renderer — never a runtime asset, never an external script tag
   (the zero-egress contract). Its integrity gates are the banner-pin +
   end-of-file-sentinel tests in src/adapters/asset_embed.rs (a truncated
   copy fails CI). */
// cute-dbt report.html — interactive layer.
//
// DOM + class + data-* attributes are the askama template contract.
// The Mermaid `<g id>` selector is runtime-constructed against the
// `{svgId}-flowchart-` prefix Mermaid 11.14+ stamps each node group
// with; never anchor on the bare `^flowchart-` form (it matches
// nothing on 11.14+ and produces a silent click-binding failure).
(function () {
  "use strict";

  // Edge / edge-type colors keyed by the snake_case wire form serialized
  // by the renderer. Okabe-Ito colorblind-safe palette. UNION variants
  // both render dashed (row concatenation, not row matching) but split
  // colors so the legend distinguishes them: orange for `union_all`
  // (duplicates preserved) and blue for `union_distinct` (deduplicated).
  // `right` reuses the same blue family — fine because joins render
  // solid, UNION renders dashed; the dash pattern is the visual cue.
  // cute-dbt#178 — FIXED categorical hue mapping (Okabe-Ito). The dark-bg
  // variant preserves hue IDENTITY (blue stays blue) but lifts lightness so
  // the page-drawn edges + the near-black `from` anchor stay legible on dark
  // themes. Node FILLS stay light in every theme (light fill + dark label
  // reads high-contrast on any bg), so only the edges/anchor flip.
  // One key per line — the edge-vocab-completeness CI gate and the BDD
  // legend steps line-anchor their greps on `<wire_key>:`.
  var JOIN_COLORS_LIGHT = {
    from:           "#1c1c1f",
    inner:          "#009E73",
    left:           "#0072B2",
    right:          "#56B4E9",
    cross:          "#CC79A7",
    full:           "#982c61",
    union_all:      "#E69F00",
    union_distinct: "#0072B2"
  };
  var JOIN_COLORS_DARK = {
    from:           "#cbd0da",
    inner:          "#34d399",
    left:           "#5aa6f0",
    right:          "#7cc4f2",
    cross:          "#e08fc0",
    full:           "#e472a3",
    union_all:      "#f0b020",
    union_distinct: "#5aa6f0"
  };
  function dagIsDark() { return document.documentElement.classList.contains("dark"); }
  function dagEdges() { return dagIsDark() ? JOIN_COLORS_DARK : JOIN_COLORS_LIGHT; }

  var ROLE_LABEL = {
    import:    "Import CTE",
    transform: "Transform CTE",
    final:     "Final select"
  };

  var DATA = (function () {
    var el = document.getElementById("cute-dbt-data");
    if (!el) return { baseline: "", models: [] };
    try {
      return JSON.parse(el.textContent);
    } catch (e) {
      return { baseline: "", models: [] };
    }
  })();

  var state = {
    selectedModel:  null,
    selectedTestId: null,
    selectedNodeId: null,
    leftPanelMode:  "node",
    // cute-dbt#91 — false: show only updated tests (default); true: show
    // all tests on the in-scope models. Global + persistent across model
    // switches. Auto-set to true at boot when the diff updated 0 tests.
    showAll:        false
  };

  // cute-dbt#139 — report-settings menu state. Both settings are PURE
  // presentation over the existing payload: `contextLines` re-folds the block
  // diffs (no recompute), `normalizeEquality` flips the per-cell change lens
  // (normalized: compare the canonical `key`; strict: compare the authored
  // `display` — both axes ship on the #138 wire, so neither needs a Rust
  // round-trip). State is in-memory; loadSettings/saveSettings persist it to
  // localStorage where available (a graceful in-memory fallback under file://,
  // where some browsers throw on localStorage access).
  var SETTINGS_KEY = "cute-dbt.settings.v1";
  var settings = {
    contextLines:      3,    // mirrors diffFoldPad's historical default (#132).
    normalizeEquality: true  // ON: hide format-only cell changes (default).
  };

  // Read persisted settings, clamping/coercing into the valid ranges. Any
  // storage error (file:// SecurityError, disabled storage) leaves the
  // in-memory defaults intact — never throws to the caller.
  function loadSettings() {
    var raw;
    try {
      raw = window.localStorage && window.localStorage.getItem(SETTINGS_KEY);
    } catch (e) {
      raw = null;
    }
    if (!raw) return;
    var parsed;
    try { parsed = JSON.parse(raw); } catch (e) { return; }
    if (!parsed || typeof parsed !== "object") return;
    if (typeof parsed.contextLines === "number" && isFinite(parsed.contextLines)) {
      settings.contextLines = Math.max(0, Math.min(20, Math.round(parsed.contextLines)));
    }
    if (typeof parsed.normalizeEquality === "boolean") {
      settings.normalizeEquality = parsed.normalizeEquality;
    }
  }

  // Persist the current settings. Swallows any storage error (file:// or
  // disabled storage) so the in-memory state still drives the live UI.
  function saveSettings() {
    try {
      if (window.localStorage) {
        window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
      }
    } catch (e) {
      // In-memory only — zero-egress fallback, no rethrow.
    }
  }

  $(function () {
    initExplainHash();
    // cute-dbt#139 — hydrate persisted settings BEFORE the first render so the
    // initial diffs fold at the saved context-lines and the cell lens reflects
    // the saved normalize-equality choice. diffFoldPad mirrors settings.
    loadSettings();
    diffFoldPad = settings.contextLines;
    renderBanner();
    if (!DATA.models.length) {
      $(".test-selection").hide();
      $(".model-sql").hide();
      $(".cte-dag").hide();
      $(".panel-row").hide();
      return;
    }
    // cute-dbt#91 — open in All-tests mode when the diff updated zero
    // tests (the common SQL-only PR), so the reviewer lands on content
    // rather than the empty 0-updated view.
    state.showAll = (countUpdated() === 0);
    // Default model foregrounds the diff's updates: first (deterministic
    // order) model with >=1 updated test, else the first model.
    state.selectedModel = defaultModelName();
    // First VISIBLE test under the resolved mode (so auto-All lands on a
    // real selected test with content, not the empty panel).
    state.selectedTestId = firstVisibleTestId(currentModel());
    // Mermaid config is static across model switches — initialize once
    // (cute-dbt#40 CR-4). renderDag() then only invokes mermaid.render.
    window.mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: "base",
      fontFamily: 'system-ui,-apple-system,"Segoe UI",sans-serif'
    });
    renderTestModeToggle();
    renderModelSelector();
    renderTestSelector();
    renderForSelectedModel();
    bindGlobalHandlers();
    bindSettingsMenu();
    // cute-dbt#178 — theme switches re-tint the DAG: re-render legend +
    // Mermaid so edge/anchor colours pick up the light/dark variant.
    // theme.js calls this hook after flipping [data-theme] / the .dark class.
    window.__cuteRerenderDag = function () { renderDagLegend(); renderDag(); };
  });

  // cute-dbt#91 — scope-toggle helpers --------------------------------

  // Count of updated (changed) tests across all in-scope models — the
  // banner's "X updated" and the auto-All-on-load trigger.
  function countUpdated() {
    var n = 0;
    DATA.models.forEach(function (m) {
      m.tests.forEach(function (t) { if (t.changed) n += 1; });
    });
    return n;
  }

  // Updated-test count for a single model.
  function updatedCount(m) {
    return m.tests.filter(function (t) { return t.changed; }).length;
  }

  // The tests visible for a model under the current toggle mode.
  function visibleTests(m) {
    if (!m) return [];
    return state.showAll ? m.tests : m.tests.filter(function (t) { return t.changed; });
  }

  // Default model on load: first model (deterministic order) with >=1
  // updated test, else the first model.
  function defaultModelName() {
    var withUpdated = DATA.models.filter(function (m) { return updatedCount(m) > 0; })[0];
    return (withUpdated || DATA.models[0]).name;
  }

  // First visible test id for a model under the resolved mode, or null.
  function firstVisibleTestId(m) {
    var vis = visibleTests(m);
    return vis.length ? vis[0].id : null;
  }

  function initExplainHash() {
    function sync() {
      var on = window.location.hash === "#explain";
      $(".error-card").toggleClass("is-visible", on);
    }
    sync();
    $(window).on("hashchange", sync);
  }

  function renderBanner() {
    // cute-dbt#91 — "K models in scope · X unit tests updated". K is the
    // number of in-scope models; X is the count of updated (changed)
    // tests across the RENDERED payload (not the domain's changed-set
    // length — a changed test whose model didn't resolve is never
    // rendered, so they can legitimately differ). The empty case keeps the
    // locked BANNER_EMPTY_SCOPE string the server-side contract pins. JS
    // rewrites the server text so swapping DATA in DevTools repaints
    // consistently without a round trip (banner wording is JS-only;
    // compose_banner_text stays untouched).
    var k = DATA.models.length;
    var x = countUpdated();
    var text;
    if (k === 0) {
      text = "0 unit tests in scope";
    } else {
      var modelNoun = k === 1 ? "model" : "models";
      var testNoun = x === 1 ? "unit test" : "unit tests";
      text = k + " " + modelNoun + " in scope · " + x + " " + testNoun + " updated";
    }
    $(".diff-scope-text").text(text);
    $(".diff-scope-baseline").text(DATA.baseline);
    // cute-dbt#96 — affirm block-precision ran when it narrowed every test
    // to context (k models in scope, 0 updated). The element exists only on
    // the PR-diff path (absent in baseline mode); jQuery no-ops if absent.
    // Copy is static in the template (LOCKED, CPO Finding A) — toggle only.
    $(".zero-updated-affirm").prop("hidden", !(k > 0 && x === 0));
  }

  function currentModel() {
    return DATA.models.filter(function (m) { return m.name === state.selectedModel; })[0];
  }

  function currentTest() {
    var m = currentModel();
    if (!m) return null;
    return m.tests.filter(function (t) { return t.id === state.selectedTestId; })[0];
  }

  function renderModelSelector() {
    var $sel = $("#model-select").empty();
    DATA.models.forEach(function (m) {
      // cute-dbt#91 — the per-model count is toggle-dependent: the updated
      // count in Updated-only mode, the total in All-tests mode.
      var n = state.showAll ? m.tests.length : updatedCount(m);
      $sel.append($("<option>").val(m.name).text(m.name + "  (" + n + ")"));
    });
    $sel.val(state.selectedModel);
    $sel.off("change.cuteDbt").on("change.cuteDbt", function () {
      state.selectedModel = $(this).val();
      // Land on the first VISIBLE test under the (persistent) toggle mode.
      state.selectedTestId = firstVisibleTestId(currentModel());
      state.selectedNodeId = null;
      state.leftPanelMode = "node";
      renderTestSelector();
      renderForSelectedModel();
    });
  }

  function renderTestSelector() {
    var $sel = $("#test-select").empty();
    var m = currentModel();
    var tests = visibleTests(m);
    tests.forEach(function (t) {
      $sel.append($("<option>").val(t.id).text(t.name));
    });
    $sel.val(state.selectedTestId);
    $sel.off("change.cuteDbt").on("change.cuteDbt", function () {
      state.selectedTestId = $(this).val();
      state.selectedNodeId = null;
      state.leftPanelMode = "node";
      renderForSelectedModel();
    });
    renderZeroUpdatedHint(m, tests);
  }

  // cute-dbt#91 — inline hint when a model has tests but none are visible
  // in Updated-only mode (the empty #test-select case). Hidden in
  // All-tests mode and whenever there are visible tests.
  function renderZeroUpdatedHint(m, tests) {
    var $hint = $(".zero-updated-hint");
    var show = m && !state.showAll && tests.length === 0 && m.tests.length > 0;
    if (show) {
      var total = m.tests.length;
      var noun = total === 1 ? "test" : "tests";
      $hint
        .text("0 updated — switch to All tests to see this model’s " + total + " " + noun + ".")
        .prop("hidden", false);
    } else {
      $hint.text("").prop("hidden", true);
    }
  }

  function renderForSelectedModel() {
    var m = currentModel();
    var t = currentTest();
    $(".recursive-banner").prop("hidden", !(m && m.is_recursive));
    // cute-dbt#145 / cute-dbt#178 — the model-level incremental badge now
    // lives in the Model-SQL code-card header, created per-render by
    // renderModelSql (the static template span is gone), so no hidden-toggle
    // is needed here.
    // cute-dbt#145 — the .expected-panel .panel-header is a PERSISTENT static
    // element. renderExpectedPanel (which clears + re-appends the mode badge /
    // expect-tooltip) runs ONLY in the `if (t)` arm below, so a switch to a
    // modified-but-untested model (currentTest() === null) would otherwise
    // leak a prior incremental-mode test's badge + tooltip. Clear them here
    // unconditionally too (targeted — never touches .expected-rowcount).
    // (gemini review, PR #146.)
    $(".expected-panel .panel-header .mode-badge, .expected-panel .panel-header .expect-tooltip").remove();
    renderTestDetails(t);
    renderModelSql(m);
    renderDagLegend();
    renderDag();
    renderSegmentedToggle();
    if (t) {
      var desc = t.description || "";
      $(".test-description").text(desc);
      // cute-dbt#74 — toggle the section's `.is-hidden` class so the
      // aria-labeled landmark drops from the accessibility tree when
      // the test carries no description (Gemini-disposition on PR#77:
      // class-based toggle is whitespace-robust where `:empty` /
      // `:has(:empty)` selectors are not).
      $(".test-description-section").toggleClass("is-hidden", !desc);
      renderLeftPanel();
      renderExpectedPanel(t);
    } else {
      $(".test-description").text("");
      $(".test-description-section").addClass("is-hidden");
      // cute-dbt#91 — distinguish "model has tests but none are updated
      // (Updated-only mode hides them)" from the genuine explorer-mode
      // "model is modified but has zero unit tests" case.
      var leftMsg = m && m.tests.length > 0
        ? "No updated tests on this model — switch to All tests to inspect its "
            + m.tests.length + (m.tests.length === 1 ? " test." : " tests.")
        : "This model is modified but has no unit tests — consider adding one.";
      $(".left-panel-body").empty().append(
        $("<div>").addClass("no-tests-empty").text(leftMsg)
      );
      // cute-dbt#178 — the row-count badge is a PERSISTENT static element a
      // prior render may have relocated into the fixture-view bar (inside
      // .expected-body); return it to the header BEFORE the wipe so the
      // .empty() below can never destroy it.
      var $erc0 = $(".expected-panel .expected-rowcount");
      $(".expected-panel .panel-header").append($erc0);
      $erc0.text("0 rows");
      $(".expected-body").empty().append(
        $("<div>").addClass("no-tests-empty")
          .text("No unit test selected.")
      );
    }
    // Fresh diffs render default-folded, so the global toggle resets to
    // "Expand all" (cute-dbt#132). Null-safe in baseline mode.
    resetExpandAllToggle();
  }

  function renderTestDetails(t) {
    var $body = $(".test-details-body").empty();
    if (!t) return;
    var $tagsLine = $("<div>").addClass("td-line td-tags-line");
    if (t.tags && t.tags.length) {
      $tagsLine.append($("<span>").addClass("td-label").text("Tagged "));
      t.tags.forEach(function (tag, i) {
        if (i > 0) $tagsLine.append(document.createTextNode(", "));
        $tagsLine.append($("<span>").addClass("td-tag").text(tag));
      });
    } else {
      $tagsLine.addClass("muted").text("Untagged");
    }
    $body.append($tagsLine);

    var $metaLine = $("<div>").addClass("td-line td-meta-line");
    var metaKeys = t.meta ? Object.keys(t.meta) : [];
    if (metaKeys.length === 0) {
      $metaLine.addClass("muted").text("No metadata");
    } else {
      metaKeys.forEach(function (k, i) {
        if (i > 0) $metaLine.append(document.createTextNode(" · "));
        $metaLine.append($("<span>").addClass("td-meta-key").text(k));
        $metaLine.append(document.createTextNode(": "));
        $metaLine.append($("<span>").addClass("td-meta-val").text(String(t.meta[k])));
      });
    }
    $body.append($metaLine);

    // cute-dbt#178 — the authoring-YAML drawer's code-card header now shows
    // this path (GitHub-style). Keep this row ONLY as a fallback for tests
    // with no authoring YAML (no drawer, no header), so the path is never
    // lost.
    if (t.defined_in && !t.authoring_yaml) {
      $body.append(
        $("<div>").addClass("td-line td-defined-in-line")
          .append($("<span>").addClass("td-label").text("Defined in "))
          .append($("<code>").addClass("td-defined-in").text(t.defined_in))
      );
    }
    // cute-dbt#69 — Authoring YAML drawer. Source-of-truth: the raw
    // unit_test block sliced by the domain layer (UnitTestYamlBlock),
    // populated by the gather_authoring_yaml run-loop stage when a
    // project root is resolvable AND the source file is readable.
    // Absent → undefined → drawer omitted entirely (no empty-state
    // copy: the panel above already surfaces tags/meta/defined-in).
    if (t.authoring_yaml) {
      // cute-dbt#96 concern 2 — when this test's own YAML block was edited
      // in the diff, the renderer attaches a reconstructed inline diff
      // (t.yaml_diff). The drawer then offers an Authored↔Diff toggle and
      // defaults to the Diff view; absent a diff it shows the plain
      // authored YAML exactly as cute-dbt#69 did.
      var hasDiff = t.yaml_diff && t.yaml_diff.lines && t.yaml_diff.lines.length;
      var $yamlDet = $("<details>").addClass("authoring-yaml").attr("open", "open");
      $yamlDet.append($("<summary>").text(hasDiff ? "Authoring YAML — diff" : "Authoring YAML"));
      var $yamlWrap = $("<div>").addClass("sql-block-wrap fixture-sql-block-wrap");
      // cute-dbt#178 — GitHub-style code-card header: the file path sits left
      // of the Diff/File toggle. `defined_in` is the test's authoring-YAML
      // path (the manifest original_file_path).
      var $yamlHeader = $("<div>").addClass("code-header");
      if (t.defined_in) {
        $yamlHeader.append($("<span>").addClass("code-filename")
          .attr("title", t.defined_in).text(t.defined_in));
      }
      $yamlHeader.append($("<span>").addClass("code-header-spacer"));
      if (hasDiff) {
        var $authPre = $("<pre>").addClass("sql-block yaml-authored-view").prop("hidden", true).append(
          $("<code>").html(highlightLinesYaml(t.authoring_yaml))
        );
        var $diffPre = $("<pre>").addClass("sql-block yaml-diff-view").html(
          diffViewsHtml(t.yaml_diff, tokenizeYaml)
        );
        var $toggle = $("<div>").addClass("yaml-diff-toggle");
        var $diffBtn = $("<button>").attr("type", "button")
          .addClass("yaml-view-btn active").attr("data-view", "diff").text("Diff");
        var $authBtn = $("<button>").attr("type", "button")
          .addClass("yaml-view-btn").attr("data-view", "authored").text("File");
        $toggle.append($diffBtn).append($authBtn);
        $toggle.on("click", ".yaml-view-btn", function () {
          var view = $(this).attr("data-view");
          $toggle.find(".yaml-view-btn").removeClass("active");
          $(this).addClass("active");
          $diffPre.prop("hidden", view !== "diff");
          $authPre.prop("hidden", view !== "authored");
        });
        $yamlHeader.append($toggle);
        $yamlWrap.append($yamlHeader).append($diffPre).append($authPre);
      } else {
        var $yamlPre = $("<pre>").addClass("sql-block").append(
          $("<code>").html(highlightLinesYaml(t.authoring_yaml))
        );
        if (t.defined_in) $yamlWrap.append($yamlHeader);
        $yamlWrap.append($yamlPre);
      }
      $yamlDet.append($yamlWrap);
      $body.append($yamlDet);
    }
  }

  // cute-dbt#47 / cute-dbt#111 — populate (or hide) the per-model Model SQL
  // section. The askama template emits one static <section class="model-sql">;
  // JS rebinds its body and Copy handler each time the user selects a
  // different model. When `raw_sql` is missing, hide the whole section
  // (defensive — dbt 1.8+ populates raw_code on every node).
  //
  // cute-dbt#111: when the selected model carries an inline SQL diff
  // (m.sql_diff — PR-diff mode, the model's .sql changed), the section shows
  // a Raw↔Diff toggle defaulting to Diff (rendered via renderBlockDiff with
  // the SQL highlighter on context lines only). Absent a diff it renders the
  // plain raw SQL exactly as cute-dbt#47 did. Copy always copies the raw SQL.
  function renderModelSql(m) {
    var $section = $(".model-sql");
    var raw = m && m.raw_sql;
    if (!raw) {
      $section.hide();
      return;
    }
    $section.show();
    var hasDiff = m.sql_diff && m.sql_diff.lines && m.sql_diff.lines.length;
    $section.find(".model-sql-summary-hint").text(hasDiff ? "diff" : "raw, with Jinja");
    var $wrap = $section.find(".model-sql-wrap");
    // Rebuild the wrap body each model switch so a prior model's toggle/diff
    // never leaks; the Copy button is re-created here too.
    $wrap.empty();
    var $copy = $("<button>").attr("type", "button")
      .addClass("sql-copy model-sql-copy").text("Copy");
    $copy.on("click.cuteDbtModelSql", function (e) {
      e.preventDefault();
      e.stopPropagation();
      copySql(raw, $(this));
    });
    $wrap.append($copy);
    // cute-dbt#178 — GitHub-style code-card header with the model's file
    // name. The payload carries no path, so synthesize `<name>.sql` (matches
    // the DAG terminal-node label, cute-dbt#155).
    var modelFile = (m && m.name ? m.name : "model") + ".sql";
    var $mHeader = $("<div>").addClass("code-header");
    $mHeader.append($("<span>").addClass("code-filename").attr("title", modelFile).text(modelFile));
    // cute-dbt#145 incremental badge — relocated here (cute-dbt#178), right
    // of the file name. Created only when the model is incremental (the
    // static template span is gone).
    if (m && m.is_incremental) {
      $mHeader.append($("<span>").addClass("incremental-badge").text("incremental"));
    }
    $mHeader.append($("<span>").addClass("code-header-spacer"));
    if (hasDiff) {
      var $diffPre = $("<pre>").addClass("sql-block model-sql-block sql-diff-view").html(
        diffViewsHtml(m.sql_diff, tokenizeSql, "model-sql-code")
      );
      var $rawPre = $("<pre>").addClass("sql-block model-sql-block sql-raw-view").prop("hidden", true).append(
        $("<code>").addClass("model-sql-code").html(highlightLinesSql(raw))
      );
      var $toggle = $("<div>").addClass("yaml-diff-toggle model-sql-toggle");
      var $diffBtn = $("<button>").attr("type", "button")
        .addClass("yaml-view-btn active").attr("data-view", "diff").text("Diff");
      var $rawBtn = $("<button>").attr("type", "button")
        .addClass("yaml-view-btn").attr("data-view", "raw").text("File");
      $toggle.append($diffBtn).append($rawBtn);
      $toggle.on("click", ".yaml-view-btn", function () {
        var view = $(this).attr("data-view");
        $toggle.find(".yaml-view-btn").removeClass("active");
        $(this).addClass("active");
        $diffPre.prop("hidden", view !== "diff");
        $rawPre.prop("hidden", view !== "raw");
      });
      $mHeader.append($toggle);
      $wrap.append($mHeader).append($diffPre).append($rawPre);
    } else {
      var $pre = $("<pre>").addClass("sql-block model-sql-block").append(
        $("<code>").addClass("model-sql-code").html(highlightLinesSql(raw))
      );
      $wrap.append($mHeader).append($pre);
    }
  }

  function renderDagLegend() {
    var $roles = $(".dag-legend-roles").empty();
    [
      { role: "import",    label: ROLE_LABEL.import },
      { role: "transform", label: ROLE_LABEL.transform },
      { role: "final",     label: ROLE_LABEL.final }
    ].forEach(function (r) {
      $roles.append(
        $("<span>").addClass("legend-item role-" + r.role)
          .append($("<span>").addClass("legend-swatch"))
          .append($("<span>").addClass("legend-label").text(r.label))
      );
    });

    var $joins = $(".dag-legend-joins").empty();
    ["from", "inner", "left", "right", "full", "cross", "union_all", "union_distinct"].forEach(function (j) {
      var $edge = $("<span>").addClass("legend-edge");
      if (j === "union_all" || j === "union_distinct") {
        $edge.addClass("is-dashed").css({
          background: "transparent",
          "border-top": "3px dashed " + dagEdges()[j],
          height: "0"
        });
      } else {
        $edge.css("background", dagEdges()[j]);
      }
      $joins.append(
        $("<span>").addClass("legend-item join-" + j)
          .append($edge)
          .append($("<span>").addClass("legend-label").text(j))
      );
    });
  }

  function buildMermaidSource(m) {
    var lines = ["graph LR"];
    var dag = m.dag;
    if (!dag || !dag.nodes.length) {
      lines.push("    empty[\"(no DAG available)\"]");
      return lines.join("\n");
    }
    var safeIds = {};
    dag.nodes.forEach(function (n, i) { safeIds[n.id] = "n" + i; });

    dag.nodes.forEach(function (n) {
      // Display the label (e.g. `orders.sql` for the terminal) when present;
      // fall back to the stable id for CTE nodes. Node identity stays keyed
      // by id everywhere else (cute-dbt#155).
      var safe = String(n.label || n.id).replace(/"/g, "&quot;");
      var shape;
      if (n.role === "import") {
        shape = "([\"" + safe + "\"])";
      } else if (n.role === "final") {
        shape = "{{\"" + safe + "\"}}";
      } else {
        shape = "[\"" + safe + "\"]";
      }
      lines.push("    " + safeIds[n.id] + shape + ":::role_" + n.role);
    });
    dag.edges.forEach(function (e) {
      lines.push("    " + safeIds[e.from] + " --> " + safeIds[e.to]);
    });

    lines.push("    classDef role_import fill:#e8f1f8,stroke:#0072B2,stroke-width:1.5px,color:#1c1c1f;");
    lines.push("    classDef role_transform fill:#f4f4f5,stroke:#8a8a90,stroke-width:1.5px,color:#1c1c1f;");
    lines.push("    classDef role_final fill:#fdf2dc,stroke:#E69F00,stroke-width:2px,color:#1c1c1f;");
    lines.push("    classDef selected stroke:#E91E63,stroke-width:4px;");

    dag.edges.forEach(function (e, i) {
      var color = dagEdges()[e.edge_type] || "#8a8a90";
      var dash = (e.edge_type === "union_all" || e.edge_type === "union_distinct") ? ",stroke-dasharray:5 3" : "";
      lines.push("    linkStyle " + i + " stroke:" + color + ",stroke-width:1.8px,fill:none" + dash + ";");
    });

    if (state.selectedNodeId) {
      var safeSel = safeIds[state.selectedNodeId];
      if (safeSel) lines.push("    class " + safeSel + " selected;");
    }
    return lines.join("\n");
  }

  function renderDag() {
    var m = currentModel();
    var $host = $(".cte-dag-mermaid").empty();
    if (!m) return;
    var src = buildMermaidSource(m);

    var id = "mermaid-" + Math.random().toString(36).slice(2, 9);
    window.mermaid.render(id, src).then(function (out) {
      $host.html(out.svg);
      if (out.bindFunctions) out.bindFunctions($host[0]);
      bindDagNodeClicks($host[0], m, id);
    }).catch(function (err) {
      $host.empty().append(
        $("<pre>").addClass("mermaid-error").text("Mermaid render failed: " + String(err))
      );
    });
  }

  function bindDagNodeClicks(svgHost, m, svgId) {
    // Mermaid 11.14+ stamps each node group with the id
    // "{svgElementId}-flowchart-{nodeName}-{counter}". Use the runtime
    // svgId to anchor a precise selector — broken selectors are visible
    // in DevTools (fail-loud), not silent.
    var prefix = svgId + "-flowchart-";
    var groups = svgHost.querySelectorAll('g.node[id^="' + prefix + '"]');
    if (!groups.length) {
      // Fallback substring selector — version-agnostic across Mermaid
      // 11.14+ point releases. (The precise prefix above misses if
      // Mermaid changes its internal id structure; the substring catches
      // any "flowchart-" stamped id.)
      groups = svgHost.querySelectorAll('g.node[id*="-flowchart-"]');
    }
    var safeToOriginal = {};
    var safeToLabel = {};
    m.dag.nodes.forEach(function (n, i) {
      safeToOriginal["n" + i] = n.id;
      safeToLabel["n" + i] = n.label || n.id;
    });
    groups.forEach(function (g) {
      var raw = g.getAttribute("id") || "";
      var stripped = raw.replace(prefix, "");
      // Trailing "-{counter}" — find last hyphen-digit boundary.
      var m2 = stripped.match(/^(.+?)-\d+$/);
      if (!m2) return;
      var safeId = m2[1];
      var originalId = safeToOriginal[safeId];
      if (!originalId) return;
      g.classList.add("dag-node");
      g.setAttribute("data-node-id", originalId);
      // Keyboard accessibility (cute-dbt#41): tabindex makes the SVG
      // group focusable; role="button" announces it to screen readers;
      // Enter/Space activate via the same selection path as click.
      g.setAttribute("tabindex", "0");
      g.setAttribute("role", "button");
      // Announce the visible label (e.g. `orders.sql`), not the internal id
      // — they diverge for the terminal node (cute-dbt#155).
      g.setAttribute("aria-label", "Inspect DAG node: " + (safeToLabel[safeId] || originalId));
      g.style.cursor = "pointer";
      function activate() {
        state.selectedNodeId = originalId;
        if (state.leftPanelMode !== "node") {
          state.leftPanelMode = "node";
          renderSegmentedToggle();
        }
        renderLeftPanel();
        renderDag();
      }
      g.addEventListener("click", activate);
      g.addEventListener("keydown", function (ev) {
        if (ev.key === "Enter" || ev.key === " ") {
          ev.preventDefault();
          activate();
        }
      });
    });
    // Keyboard a11y (cute-dbt#41): activate() triggers a full SVG
    // re-render, which detaches the previously focused <g>. Restore
    // focus to the selected node so Tab+Enter chains feel continuous.
    // `:focus-visible` reflects the originating input heuristic, so
    // mouse activation does not produce a stuck outline here.
    if (state.selectedNodeId) {
      var escaped = String(state.selectedNodeId).replace(/[\\"]/g, "\\$&");
      var sel = svgHost.querySelector('g.dag-node[data-node-id="' + escaped + '"]');
      if (sel) sel.focus({ preventScroll: true });
    }
  }

  function renderSegmentedToggle() {
    $(".panel-toggle [data-mode]").each(function () {
      var mode = $(this).attr("data-mode");
      $(this).attr("aria-pressed", mode === state.leftPanelMode ? "true" : "false");
      $(this).toggleClass("is-active", mode === state.leftPanelMode);
    });
  }

  // cute-dbt#91 — sync the Updated-only ↔ All-tests toggle's active state
  // to state.showAll (mirrors renderSegmentedToggle's idiom).
  function renderTestModeToggle() {
    var active = state.showAll ? "all" : "updated";
    $(".test-mode-toggle [data-test-mode]").each(function () {
      var mode = $(this).attr("data-test-mode");
      $(this).attr("aria-pressed", mode === active ? "true" : "false");
      $(this).toggleClass("is-active", mode === active);
    });
  }

  function bindGlobalHandlers() {
    $(".panel-toggle").on("click", "[data-mode]", function () {
      state.leftPanelMode = $(this).attr("data-mode");
      renderSegmentedToggle();
      renderLeftPanel();
    });
    // cute-dbt#91 — global Updated-only ↔ All-tests toggle. state.showAll
    // is global and persists across model switches (the model-change
    // handler never resets it). On flip, keep the selection valid: if the
    // currently selected test is no longer visible, fall back to the first
    // visible test (or null).
    $(".test-mode-toggle").on("click", "[data-test-mode]", function () {
      var nextShowAll = $(this).attr("data-test-mode") === "all";
      if (nextShowAll === state.showAll) return;
      state.showAll = nextShowAll;
      var vis = visibleTests(currentModel());
      var stillVisible = vis.some(function (t) { return t.id === state.selectedTestId; });
      if (!stillVisible) state.selectedTestId = vis.length ? vis[0].id : null;
      state.selectedNodeId = null;
      state.leftPanelMode = "node";
      renderTestModeToggle();
      renderModelSelector();
      renderTestSelector();
      renderForSelectedModel();
    });
    // cute-dbt#132 — hunk-fold reveal. ONE delegated listener (diff blocks are
    // .empty()'d and rebuilt on model/test switch, so delegate on `document`
    // to survive re-renders). Reveal is PARENT-SCOPED: `$(this).parent()`
    // isolates `.fold-<id>` to the SAME <code> block, so the call-local fold
    // ids never collide across the YAML drawer vs the model-SQL block.
    // Bidirectional toggle (#136): clicking a fold expands the hidden run AND
    // keeps the control visible, relabeled to a "Hide N" collapse affordance;
    // clicking again re-collapses just that hunk. Reveal stays PARENT-SCOPED to
    // the enclosing <code> so duplicate call-local fold ids never cross-talk.
    function toggleFold($ctrl) {
      var id = $ctrl.attr("data-fold");
      var willExpand = $ctrl.attr("aria-expanded") !== "true";
      $ctrl.parent().find(".fold-" + id).prop("hidden", !willExpand);
      $ctrl.attr("aria-expanded", willExpand ? "true" : "false");
      $ctrl.find(".diff-fold-label").text(foldLabel(willExpand, $ctrl.attr("data-fold-count")));
    }
    $(document).on("click", ".diff-fold", function () {
      toggleFold($(this));
    });
    $(document).on("keydown", ".diff-fold", function (e) {
      // Enter or Space activates (both directions); preventDefault on Space
      // stops page scroll.
      if (e.key === "Enter" || e.key === " " || e.key === "Spacebar") {
        e.preventDefault();
        toggleFold($(this));
      }
    });
    bindDiffViewControls();
  }

  function renderLeftPanel() {
    var t = currentTest();
    if (!t) return;
    if (state.leftPanelMode === "inputs") {
      renderAllInputs(t);
    } else {
      renderNodeDetail(t);
    }
  }

  function renderNodeDetail(t) {
    var $wrap = $(".left-panel-body").empty().attr("data-mode", "node");
    var m = currentModel();
    if (!state.selectedNodeId) {
      $wrap.append($("<div>").addClass("empty-hint").text("Click a node above to inspect."));
      return;
    }
    var node = m && m.dag.nodes.filter(function (n) { return n.id === state.selectedNodeId; })[0];
    if (!node) {
      $wrap.append($("<div>").addClass("empty-hint").text("Node not found."));
      return;
    }
    var $detail = $("<div>").addClass("node-detail")
      .attr("data-node-id", node.id)
      .attr("data-node-role", node.role);

    var $hdr = $("<div>").addClass("node-detail-header");
    $hdr.append($("<h3>").addClass("node-detail-title").text(node.label || node.id));
    $hdr.append($("<span>").addClass("node-role-badge role-" + node.role).text(node.role));
    $detail.append($hdr);

    var sql = (m.compiled_sql && m.compiled_sql[node.id]) || "-- compiled SQL not available";
    var $det = $("<details>").addClass("compiled-sql").attr("open", "open");
    $det.append($("<summary>").text("Compiled SQL"));
    var $sqlWrap = $("<div>").addClass("sql-block-wrap");
    var $copy = $("<button>").attr("type", "button").addClass("sql-copy").text("Copy")
      .on("click", function (e) { e.preventDefault(); e.stopPropagation(); copySql(sql, $(this)); });
    var $pre = $("<pre>").addClass("sql-block").append($("<code>").html(highlightLinesSql(sql)));
    $sqlWrap.append($copy).append($pre);
    $det.append($sqlWrap);
    $detail.append($det);

    // Fixture binding surface — cute-dbt#34 messy-import-CTE medium scope.
    // A single CTE body may reference multiple ref() targets (UNION ALL,
    // JOIN, derived subqueries), in which case the renderer binds EVERY
    // matching given to that one node; we stack them vertically here so
    // each fixture card is independently scannable. Pass-1 import-CTEs
    // with no bound given retain the historic "no fixture provided"
    // empty-state copy; transform leaves without a binding render
    // nothing extra (most non-import CTEs carry no fixture by design).
    var bound = t.given.filter(function (g) { return g.bound_to_node === node.id; });
    if (bound.length > 0) {
      // cute-dbt#131 — `bound` is a FILTERED view of `t.given`, so its own
      // index is not the source ordinal; recover the true position via
      // `indexOf` (filter preserves object references) so the cell-diff binds
      // to the right given even when two givens share a `ref(...)`.
      bound.forEach(function (g) { $detail.append(renderGivenSection(g, t.data_diff, t.given.indexOf(g))); });
    } else if (node.role === "import") {
      $detail.append(
        $("<div>").addClass("given-empty")
          .text('no fixture provided — dbt treats unspecified inputs as empty')
      );
    }
    $wrap.append($detail);
    initDataTablesIn($wrap);
  }

  // cute-dbt#98 — the test's cell-level data diff for THIS given input, or
  // undefined. `data_diff` is present only in PR-diff mode and only when the
  // test's own block was edited; `given[]` carries an entry only for inputs
  // whose fixture data carried a real cell change (the domain already gated on
  // `has_real_change()`), so presence === "default this table to Diff".
  // cute-dbt#131 — resolve a given's cell diff by its SOURCE ORDINAL (its
  // position in the test's `given:` list), NOT its `input` text. Two givens
  // can share the same `ref(...)` input, and `dataDiff.given` is filtered to
  // changed givens only — so neither `input` nor a dense index into
  // `dataDiff.given` identifies a given. The ordinal (carried on each
  // NamedTableDiff) is the stable key; the caller passes the render loop index,
  // which is the same source position since `t.given` is the full in-order list.
  function givenDataDiff(dataDiff, ordinal) {
    if (!dataDiff || !dataDiff.given) return undefined;
    var hit = dataDiff.given.filter(function (g) { return g.ordinal === ordinal; })[0];
    return hit ? hit.diff : undefined;
  }

  function renderGivenSection(given, dataDiff, ordinal) {
    var $sec = $("<section>").addClass("given-section").attr("data-input-name", given.input);
    var $hdr = $("<div>").addClass("table-header");
    $hdr.append($("<h4>").addClass("table-title").text("given · " + given.input));
    // cute-dbt#145 — mark a `given: - input: this` as the model's prior
    // state. $hdr is freshly built per call (renderAllInputs .empty()s the
    // wrap), so no idempotent clear is needed here.
    if (given.is_this) {
      $hdr.append($("<span>").addClass("this-badge").text("prior model state"));
    }
    // cute-dbt#126 — provenance chip when the data was LOADED from an external
    // fixture file (`fixture:` set AND rows present, i.e. the reader inlined
    // it). Shown in BOTH the grid and the sql-code-block paths below; the
    // UNREADABLE case (rows still null) keeps the buildExternalFixtureAffordance
    // instead. Inline fixtures omit `fixture`, so the chip never fires for them
    // and existing reports stay byte-identical.
    if (isLoadedExternalFixture(given)) {
      $hdr.append(buildFixtureProvenanceChip(given.fixture));
    }
    if (isSqlCodeBlockFixture(given)) {
      $hdr.append($("<span>").addClass("format-badge").text("format: sql"));
      $sec.append($hdr);
      $sec.append(buildSqlCodeBlock(given.rows));
      return $sec;
    }
    // cute-dbt#98 / #126 — external-fixture guard. When the data lives in an
    // external fixture file (`fixture:` set AND `rows == null`, so the rows are
    // NOT in the manifest), render an affordance pointing at the file + a
    // pointer to the Authoring YAML drawer rather than a silently-empty grid.
    if (isExternalFixture(given)) {
      if (given.format) {
        $hdr.append($("<span>").addClass("format-badge").text("format: " + given.format));
      }
      $sec.append($hdr);
      $sec.append(buildExternalFixtureAffordance(given.fixture));
      return $sec;
    }
    // cute-dbt#138 — the Current view renders the Rust-computed POD directly.
    var table = given.table || { columns: [], rows: [] };
    var rowCount = table.rows.length;
    if (given.format && given.format !== "dict") {
      $hdr.append($("<span>").addClass("format-badge").text("format: " + given.format));
    }
    $hdr.append($("<span>").addClass("row-count-badge")
      .text(rowCount + " row" + (rowCount === 1 ? "" : "s")));
    $sec.append($hdr);
    var diff = givenDataDiff(dataDiff, ordinal);
    $sec.append(buildFixtureView(table, "given-table", diff, given.column_meta));
    return $sec;
  }

  function renderAllInputs(t) {
    var $wrap = $(".left-panel-body").empty().attr("data-mode", "inputs");
    if (!t.given.length) {
      $wrap.append($("<div>").addClass("empty-hint").text("No fixtures defined for this test."));
      return;
    }
    t.given.forEach(function (given, idx) { $wrap.append(renderGivenSection(given, t.data_diff, idx)); });
    initDataTablesIn($wrap);
  }

  function renderExpectedPanel(t) {
    var $panel = $(".expected-panel");
    var $exHdr = $panel.find(".panel-header");
    // cute-dbt#178 — the row-count badge is a PERSISTENT static element. A
    // prior render may have relocated it into the fixture-view bar (below);
    // return it to the header BEFORE the body wipe so $body.empty() can
    // never destroy it.
    var $rowcount = $panel.find(".expected-rowcount");
    $exHdr.append($rowcount);
    // cute-dbt#145 — per-test mode badge + expect-semantics tooltip.
    // The .panel-header is a PERSISTENT static element reused across
    // renders, so first clear any prior mode badge / tooltip (targeted —
    // must NOT touch the static .expected-rowcount). The mode badge shows
    // only when the enclosing model is incremental; the tooltip rides the
    // AUTHORITATIVE bool (is_incremental_mode === true), NEVER the
    // `this`-given proxy, and NEVER on the full-refresh branch (there
    // `expect` IS the final table). Emitted BEFORE the sql / external
    // early-returns: the mode is about the test, not the fixture format.
    $exHdr.find(".mode-badge, .expect-tooltip").remove();
    var $body = $panel.find(".expected-body").empty();
    var em = currentModel();
    if (em && em.is_incremental) {
      var incrementalMode = t.is_incremental_mode === true;
      $exHdr.append($("<span>")
        .addClass("mode-badge " + (incrementalMode ? "mode-incremental" : "mode-full-refresh"))
        .text(incrementalMode ? "incremental branch" : "full-refresh branch"));
      if (incrementalMode) {
        var tip = "Expected is the output of the model's compiled SELECT on the "
          + "incremental branch — the rows the configured incremental strategy "
          + "will apply to the table — not the table's final state after the run.";
        // A focusable <button> carrying a CSS-rendered bubble shown on hover
        // AND keyboard focus — a native `title` is hover-delayed, keyboard-
        // unreachable, and frequently never paints (cute-dbt#146 review). The
        // bubble is the visual surface; `aria-label` carries the same text for
        // assistive tech (bubble is aria-hidden so it is not announced twice).
        // Pure CSS — no asset, no JS tooltip lib — so the zero-egress gate holds.
        var $tip = $("<button>")
          .attr("type", "button")
          .addClass("expect-tooltip")
          .attr("aria-label", tip);
        $tip.append(document.createTextNode("ⓘ"));
        $tip.append($("<span>")
          .addClass("expect-tooltip-bubble")
          .attr("aria-hidden", "true")
          .text(tip));
        $exHdr.append($tip);
      }
    }
    // cute-dbt#126 — external-fixture provenance for the expect side (loaded
    // from a file). `$body` is emptied each render, so no cleanup is needed.
    if (isLoadedExternalFixture(t.expected)) {
      $body.append(buildFixtureProvenanceChip(t.expected.fixture));
    }
    if (isSqlCodeBlockFixture(t.expected)) {
      $panel.find(".expected-rowcount").text("format: sql");
      $body.append(buildSqlCodeBlock(t.expected.rows));
      return;
    }
    // cute-dbt#98 / #126 — external-fixture guard for the expect side.
    if (isExternalFixture(t.expected)) {
      $panel.find(".expected-rowcount").text("external fixture");
      $body.append(buildExternalFixtureAffordance(t.expected.fixture));
      return;
    }
    // cute-dbt#138 — the Current view renders the Rust-computed POD directly.
    var table = t.expected.table || { columns: [], rows: [] };
    var rowCount = table.rows.length;
    $rowcount.text(rowCount + " row" + (rowCount === 1 ? "" : "s"));
    // The expect side's cell diff, when the test's expect fixture carried a
    // real change (cute-dbt#98). `data_diff.expect` already passed
    // `has_real_change()`, so its presence === "default this table to Diff".
    var diff = (t.data_diff && t.data_diff.expect) || undefined;
    $body.append(buildFixtureView(table, "expected-table", diff, t.expected.column_meta));
    // cute-dbt#178 — when a Diff/File toggle bar exists, relocate the header
    // meta onto that bar, to its right, in reading order: [mode badge]
    // [N rows] [expect-tooltip]. .append() MOVES the nodes (no copy), so
    // nothing is duplicated. No bar (sql/external/no-diff) => the meta stays
    // in the header as the fallback (the pre-#178 layout).
    var $bar = $body.find(".fixture-view-bar").first();
    if ($bar.length) {
      $bar.append($exHdr.find(".mode-badge")); // empty set when not incremental
      $bar.append($rowcount);
      $bar.append($exHdr.find(".expect-tooltip"));
    }
    initDataTablesIn($body);
  }

  // dbt's unit-test fixture format determines what shape `rows` arrives
  // in (cute-dbt#66, 2026-05-26 — engine divergence finding):
  //
  //   format | dbt-core 1.11+    | dbt-fusion 2.0-preview
  //   -------|-------------------|-----------------------
  //   dict   | array of dicts    | array of dicts
  //   csv    | array of dicts    | raw CSV string
  //   sql    | raw SELECT string | raw SELECT string
  //
  // The two engines diverge on csv normalization — core pre-parses the
  // CSV body at compile time, fusion passes it through. cute-dbt must
  // tabulate both csv paths so reports look identical regardless of
  // which engine compiled the manifest. `format: sql` arrives as a raw
  // SELECT string; cute-dbt#137 tabulates the LITERAL-ROW subset in Rust
  // (so `obj.table` is populated), and only a NON-literal sql (a real
  // FROM/operator/cast/function) falls back to the syntax-highlighted
  // code block here.

  // Whether `obj` is a raw `format: sql` fixture string.
  function isSqlStringFixture(obj) {
    return Boolean(obj && obj.format === "sql" && typeof obj.rows === "string");
  }

  // Whether to render `obj` as the sql CODE BLOCK fallback: a raw sql
  // string AND the domain produced no tabulated `table` POD (cute-dbt#137 —
  // a literal-row SELECT tabulates, so `obj.table` is present and we render
  // the data grid instead). An accepted literal-sql always yields ≥1 row, so
  // a truthy `obj.table` reliably distinguishes "tabulated" from "fell back".
  function isSqlCodeBlockFixture(obj) {
    return isSqlStringFixture(obj) && !obj.table;
  }

  // Render a sql-format fixture as a syntax-highlighted code block.
  // Reuses the compiled-SQL drawer's `.sql-block-wrap` shell so visual
  // style stays consistent. No copy button: the source of truth is the
  // fixture in the manifest, not the rendered DOM. This is the ONE
  // remaining non-tabulatable fallback path: a `format: sql` fixture has no
  // cells, so the domain returns no `table` POD (cute-dbt#138) and the JS
  // renders the raw SELECT here.
  function buildSqlCodeBlock(sql) {
    var $wrap = $("<div>").addClass("sql-block-wrap fixture-sql-block-wrap");
    var $pre = $("<pre>").addClass("sql-block").append(
      $("<code>").html(highlightLinesSql(String(sql)))
    );
    $wrap.append($pre);
    return $wrap;
  }

  // cute-dbt#138 — the JS csv/dict table parsers (`parseCsvRows`,
  // `extractRowsFromFixture`, `collectColumns`) are RETIRED. The Current view
  // now renders the Rust-computed `FixtureTable` POD that the domain ships on
  // each given/expect payload (`table`), so csv/dict tabulation — and the
  // engine-divergence handling above — happens once, in Rust
  // (`table_from_manifest_rows`), and the template is a pure renderer. CSV
  // RFC 4180 correctness is now owned by the Rust `parse_csv_rows` unit tests
  // (the JS `__cuteParseCsv` headless seam retired with the function).

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function escapeHtmlForBody(s) {
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }
  // cute-dbt#132 — token-stream syntax highlighters.
  //
  // Replaces the old single-`String.replace` highlighters (which matched a
  // whole `{{ ref('stg_orders') }}` as one opaque jinja blob, so the inner
  // `'stg_orders'` never read as a string). The new approach is a
  // position-advancing, mode-stack scanner that returns a GAP-FREE token
  // stream `[{text, cls}, ...]` over the RAW input (cls "" = plaintext). Two
  // hard invariants hold by construction:
  //   1. Gap-free: tokenizeSql(s).map(t=>t.text).join("") === s for all s.
  //   2. Raw-in / escape-at-emit: tokens carry RAW slices; HTML escaping
  //      happens only at `emitTokens` time, so renderBlockDiff's codepoint
  //      `emphasis` offsets (Rust `chars()` indices) never drift.
  // Every branch advances pos by >=1 (single-char fallthrough is the
  // backstop), so unterminated fragments — renderBlockDiff feeds per-line
  // pieces of multi-line constructs — flush-to-end instead of hanging.

  // SQL keyword set (lowercase). `from_date` does NOT highlight `from`
  // because identifiers are scanned WHOLE then looked up (cute-dbt#31/#40).
  var SQL_KEYWORDS = {};
  ("select from where with recursive as join inner left right full outer cross " +
   "lateral natural on using group by order having limit offset fetch union " +
   "intersect except all distinct case when then else end and or not in is null " +
   "like ilike between exists any some asc desc nulls first last over partition " +
   "qualify window cast try_cast coalesce sum count avg min max row_number true " +
   "false").split(" ").forEach(function (k) { SQL_KEYWORDS[k] = true; });
  // Jinja control words stay violet inside `{{ }}` / `{% %}`.
  var JINJA_KEYWORDS = {};
  ("if elif else endif for in endfor set endset macro endmacro call endcall " +
   "filter endfilter block endblock do with without as import from include " +
   "extends is not and or loop recursive raw endraw").split(" ")
    .forEach(function (k) { JINJA_KEYWORDS[k] = true; });
  // Known dbt functions: their name reads violet even without a trailing `(`.
  var JINJA_FUNCS = {};
  ("ref source config var env_var is_incremental this target model graph " +
   "builtins flags modules doc run_query statement log print adapter return " +
   "dispatch").split(" ").forEach(function (k) { JINJA_FUNCS[k] = true; });

  function isWordChar(ch) {
    return ch !== undefined && /[A-Za-z0-9_]/.test(ch);
  }

  // Scan the inside of a `{{ }}` (expr) or `{% %}` (stmt) construct, pushing
  // tokens onto `out`. `src` is the whole string, `i` the index just past the
  // open delimiter. `expr` selects the close delimiter. Returns the index past
  // the close (or src.length if the fragment is unterminated). Strings inside
  // jinja are `sql-string` (the headline fix: `ref('stg_orders')` reads teal).
  function scanJinjaBody(src, i, out, expr) {
    var n = src.length;
    while (i < n) {
      var ch = src[i];
      // Close delim: `-}}`/`}}` for expr, `-%}`/`%}` for stmt.
      if (expr) {
        if (ch === "-" && src[i + 1] === "}" && src[i + 2] === "}") {
          out.push({ text: "-}}", cls: "sql-jinja" }); return i + 3;
        }
        if (ch === "}" && src[i + 1] === "}") {
          out.push({ text: "}}", cls: "sql-jinja" }); return i + 2;
        }
      } else {
        if ((ch === "-" || ch === "+") && src[i + 1] === "%" && src[i + 2] === "}") {
          out.push({ text: ch + "%}", cls: "sql-jinja" }); return i + 3;
        }
        if (ch === "%" && src[i + 1] === "}") {
          out.push({ text: "%}", cls: "sql-jinja" }); return i + 2;
        }
      }
      // String inside jinja -> sql-string.
      if (ch === "'" || ch === '"') {
        var q = ch, j = i + 1;
        while (j < n) {
          if (src[j] === "\\") { j += 2; continue; }
          if (src[j] === q) { j += 1; break; }
          j += 1;
        }
        out.push({ text: src.slice(i, Math.min(j, n)), cls: "sql-string" });
        i = Math.min(j, n);
        continue;
      }
      // Identifier: jinja keyword / dbt function / function-call -> jinja; else plain.
      if (/[A-Za-z_]/.test(ch)) {
        var k = i + 1;
        while (k < n && isWordChar(src[k])) k += 1;
        var word = src.slice(i, k);
        var lower = word.toLowerCase();
        var rest = k;
        while (rest < n && (src[rest] === " " || src[rest] === "\t")) rest += 1;
        var isCall = src[rest] === "(";
        if (JINJA_KEYWORDS[lower] || JINJA_FUNCS[lower] || isCall) {
          out.push({ text: word, cls: "sql-jinja" });
        } else {
          out.push({ text: word, cls: "" });
        }
        i = k;
        continue;
      }
      // Number / operator / other -> plaintext (coalesce a run of non-word,
      // non-quote, non-close-delim chars into one plaintext token).
      var s = i;
      i += 1;
      out.push({ text: src.slice(s, i), cls: "" });
    }
    return i; // unterminated fragment: flushed to end, no hang.
  }

  // Tokenize SQL (with embedded jinja). Gap-free over RAW `src`.
  function tokenizeSql(src) {
    src = String(src);
    var out = [];
    var n = src.length;
    var i = 0;
    var plainStart = -1;
    function flushPlain(upTo) {
      if (plainStart !== -1 && upTo > plainStart) {
        out.push({ text: src.slice(plainStart, upTo), cls: "" });
      }
      plainStart = -1;
    }
    while (i < n) {
      var c0 = src[i], c1 = src[i + 1];
      // 1. jinja comment {# ... #} (opaque; unterminated -> flush rest).
      if (c0 === "{" && c1 === "#") {
        flushPlain(i);
        var end = src.indexOf("#}", i + 2);
        var stop = end === -1 ? n : end + 2;
        out.push({ text: src.slice(i, stop), cls: "sql-comment" });
        i = stop;
        continue;
      }
      // 2/3. jinja expr `{{` or stmt `{%` (optional -/+ handled by scanner).
      if (c0 === "{" && (c1 === "{" || c1 === "%")) {
        flushPlain(i);
        var expr = c1 === "{";
        // Emit open delim (with optional whitespace-control sign).
        var sign = (src[i + 2] === "-" || src[i + 2] === "+") ? src[i + 2] : "";
        var open = src.slice(i, i + 2 + sign.length);
        out.push({ text: open, cls: "sql-jinja" });
        i = scanJinjaBody(src, i + 2 + sign.length, out, expr);
        continue;
      }
      // 4. block comment /* ... */ (unterminated -> flush rest).
      if (c0 === "/" && c1 === "*") {
        flushPlain(i);
        var be = src.indexOf("*/", i + 2);
        var bstop = be === -1 ? n : be + 2;
        out.push({ text: src.slice(i, bstop), cls: "sql-comment" });
        i = bstop;
        continue;
      }
      // 5. line comment -- ... to EOL.
      if (c0 === "-" && c1 === "-") {
        flushPlain(i);
        var nl = src.indexOf("\n", i);
        var lstop = nl === -1 ? n : nl;
        out.push({ text: src.slice(i, lstop), cls: "sql-comment" });
        i = lstop;
        continue;
      }
      // 6. single-quoted string '...' with '' doubling and \' tolerance. If a
      //    jinja open appears inside, jinja wins: emit string-so-far, scan the
      //    jinja, then resume the string.
      if (c0 === "'") {
        flushPlain(i);
        var sBegin = i;
        var j = i + 1;
        while (j < n) {
          var cj = src[j];
          if (cj === "\\") { j += 2; continue; }
          if (cj === "'" && src[j + 1] === "'") { j += 2; continue; } // doubled
          if (cj === "{" && (src[j + 1] === "{" || src[j + 1] === "%" || src[j + 1] === "#")) {
            // jinja wins inside the string body.
            if (j > sBegin) out.push({ text: src.slice(sBegin, j), cls: "sql-string" });
            // Re-enter the main loop at the jinja by handling it inline.
            var jc1 = src[j + 1];
            if (jc1 === "#") {
              var je = src.indexOf("#}", j + 2);
              var jstop = je === -1 ? n : je + 2;
              out.push({ text: src.slice(j, jstop), cls: "sql-comment" });
              j = jstop;
            } else {
              var jexpr = jc1 === "{";
              var jsign = (src[j + 2] === "-" || src[j + 2] === "+") ? src[j + 2] : "";
              out.push({ text: src.slice(j, j + 2 + jsign.length), cls: "sql-jinja" });
              j = scanJinjaBody(src, j + 2 + jsign.length, out, jexpr);
            }
            sBegin = j;
            continue;
          }
          if (cj === "'") { j += 1; break; } // closing quote
          j += 1;
        }
        if (j > sBegin) out.push({ text: src.slice(sBegin, Math.min(j, n)), cls: "sql-string" });
        i = Math.min(j, n);
        if (i === sBegin) i += 1; // safety: never stall
        continue;
      }
      // 7. double-quoted identifier "..." -> plaintext (NOT a string).
      if (c0 === '"') {
        flushPlain(i);
        var dj = i + 1;
        while (dj < n) {
          if (src[dj] === "\\") { dj += 2; continue; }
          if (src[dj] === '"') { dj += 1; break; }
          dj += 1;
        }
        out.push({ text: src.slice(i, Math.min(dj, n)), cls: "" });
        i = Math.min(dj, n);
        continue;
      }
      // 10. identifier -> keyword lookup (case-insensitive) or plaintext.
      if (/[A-Za-z_]/.test(c0)) {
        flushPlain(i);
        var k2 = i + 1;
        while (k2 < n && isWordChar(src[k2])) k2 += 1;
        var w = src.slice(i, k2);
        out.push({ text: w, cls: SQL_KEYWORDS[w.toLowerCase()] ? "sql-keyword" : "" });
        i = k2;
        continue;
      }
      // 8/9/11. number, multi-char op, single char -> plaintext (coalesced).
      if (plainStart === -1) plainStart = i;
      i += 1;
    }
    flushPlain(n);
    return out;
  }

  // Tokenize YAML. Gap-free over RAW `src`. No jinja nesting (not needed
  // for the authoring-YAML drawer in v0.x). Strings are matched before the
  // `#` comment arm so a `#` inside a quoted scalar never opens a comment.
  function tokenizeYaml(src) {
    src = String(src);
    var out = [];
    var n = src.length;
    var i = 0;
    var plainStart = -1;
    function flushPlain(upTo) {
      if (plainStart !== -1 && upTo > plainStart) {
        out.push({ text: src.slice(plainStart, upTo), cls: "" });
      }
      plainStart = -1;
    }
    // Track whether we are at logical start-of-line content (after optional
    // indent + a single `- ` list sigil) so the key arm only fires there.
    var atLineStart = true;     // true right after a newline / at index 0
    var sawListSigil = false;
    while (i < n) {
      var c = src[i];
      if (c === "\n") {
        flushPlain(i);
        out.push({ text: "\n", cls: "" });
        i += 1;
        atLineStart = true;
        sawListSigil = false;
        continue;
      }
      // String scalars (single/double) — matched before the comment arm.
      if (c === "'" || c === '"') {
        flushPlain(i);
        var q = c, j = i + 1;
        while (j < n) {
          if (src[j] === "\\") { j += 2; continue; }
          if (q === "'" && src[j] === "'" && src[j + 1] === "'") { j += 2; continue; }
          if (src[j] === q) { j += 1; break; }
          if (src[j] === "\n") break; // YAML scalars don't span our per-line fragments
          j += 1;
        }
        out.push({ text: src.slice(i, Math.min(j, n)), cls: "yaml-string" });
        i = Math.min(j, n);
        atLineStart = false;
        continue;
      }
      // # line comment (to EOL).
      if (c === "#") {
        flushPlain(i);
        var nl = src.indexOf("\n", i);
        var stop = nl === -1 ? n : nl;
        out.push({ text: src.slice(i, stop), cls: "yaml-comment" });
        i = stop;
        atLineStart = false;
        continue;
      }
      // At start-of-line: consume indent, then an optional `- ` list sigil
      // (which keeps us at start-of-line so `- name:` highlights `name`).
      if (atLineStart) {
        if (c === " " || c === "\t") {
          if (plainStart === -1) plainStart = i;
          i += 1;
          continue;
        }
        if (!sawListSigil && c === "-" && (src[i + 1] === " " || src[i + 1] === "\t")) {
          // Emit indent-so-far + the `- ` as plaintext.
          if (plainStart === -1) plainStart = i;
          i += 2;
          // consume following spaces of the sigil run
          while (i < n && (src[i] === " " || src[i] === "\t")) i += 1;
          flushPlain(i);
          sawListSigil = true;
          continue; // still at line start (key may follow the sigil)
        }
        // Identifier followed by `:` -> yaml-key.
        if (/[A-Za-z_]/.test(c)) {
          var k = i + 1;
          while (k < n && /[A-Za-z0-9_\-]/.test(src[k])) k += 1;
          // peek past optional trailing spaces is NOT done: YAML keys abut `:`.
          if (src[k] === ":") {
            flushPlain(i);
            out.push({ text: src.slice(i, k), cls: "yaml-key" });
            i = k;
            atLineStart = false;
            continue;
          }
        }
        atLineStart = false; // first non-indent, non-key char ends the key zone
      }
      // Default: coalesce into a plaintext run.
      if (plainStart === -1) plainStart = i;
      i += 1;
    }
    flushPlain(n);
    return out;
  }

  // Emit a token stream as HTML. RAW slices are escaped here (and ONLY here),
  // wrapped in a class span unless the token is plaintext (cls === "").
  function emitTokens(tokens) {
    var html = "";
    for (var i = 0; i < tokens.length; i++) {
      var t = tokens[i];
      if (t.cls) {
        html += '<span class="' + t.cls + '">' + escapeHtmlForBody(t.text) + "</span>";
      } else {
        html += escapeHtmlForBody(t.text);
      }
    }
    return html;
  }

  // Thin wrappers: tokenize -> emit. Same observable contract as the old
  // regex highlighters for context lines, PLUS nested-jinja correctness.
  function highlightSql(sql) { return emitTokens(tokenizeSql(sql)); }
  function highlightYaml(yaml) { return emitTokens(tokenizeYaml(yaml)); }

  // cute-dbt#178 — plain (non-diff) code block with a GitHub-style single
  // line-number gutter. Splits on newlines and tokenizes each line
  // independently (same per-line contract the diff path already relies on),
  // wrapping each in a .code-line so a robust numbered gutter renders without
  // depending on <pre> newline counting. The trailing blank line is trimmed
  // so we don't show a stray empty numbered row.
  function highlightLines(text, tokenizeFn) {
    var src = String(text == null ? "" : text).replace(/\n+$/, "");
    var rows = src.split("\n");
    var out = "";
    for (var i = 0; i < rows.length; i++) {
      out += '<span class="code-line">'
           + '<span class="code-gutter" aria-hidden="true">' + (i + 1) + '</span>'
           + '<span class="diff-code">' + emitTokens(tokenizeFn(rows[i])) + '</span>'
           + '</span>';
    }
    return out;
  }
  function highlightLinesSql(sql) { return highlightLines(sql, tokenizeSql); }
  function highlightLinesYaml(yaml) { return highlightLines(yaml, tokenizeYaml); }

  // Expose for headless JS unit tests (cute-dbt#69 / cute-dbt#132 — parallels
  // the `__cuteRenderYamlDiff` headless seam).
  window.__cuteHighlightYaml = highlightYaml;
  window.__cuteTokenizeSql = tokenizeSql;
  window.__cuteTokenizeYaml = tokenizeYaml;

  // Overlay the codepoint `emphasis` range [a,b) onto a token stream. Walks
  // the tokens tracking a running CODEPOINT offset; each token is intersected
  // with [a,b) and split at the boundary so the in-range pieces sit inside a
  // <strong> while every piece keeps its syntax class. The range can straddle
  // token boundaries (e.g. a changed run crossing a keyword into the next
  // token), so this is a true split — NOT a single-token lookup. Slicing is by
  // CODEPOINT (Array.from), matching Rust `chars()` offsets.
  function emitTokensWithEmphasis(tokens, a, b) {
    var html = "";
    var pos = 0; // running codepoint offset across the whole line
    for (var t = 0; t < tokens.length; t++) {
      var cps = Array.from(tokens[t].text);
      var cls = tokens[t].cls;
      var len = cps.length;
      var start = pos, endp = pos + len;
      pos = endp;
      // Within this token, codepoint indices [0,len) map to line offsets
      // [start, endp). Emphasis covers token-local [lo, hi).
      var lo = Math.max(a, start) - start;
      var hi = Math.min(b, endp) - start;
      function wrap(text, emph) {
        if (text === "") return "";
        var esc = escapeHtmlForBody(text);
        var inner = emph ? "<strong>" + esc + "</strong>" : esc;
        return cls ? '<span class="' + cls + '">' + inner + "</span>" : inner;
      }
      if (hi <= 0 || lo >= len || hi <= lo) {
        // No overlap with this token.
        html += wrap(cps.join(""), false);
      } else {
        // Split this token at [lo, hi): pre | emphasized | post. Each piece
        // keeps the token's class; the middle piece is wrapped in <strong>.
        // To keep CSS-descendant nesting valid and the class on every piece,
        // emit one span per non-empty piece.
        html += wrap(cps.slice(0, lo).join(""), false);
        html += wrap(cps.slice(lo, hi).join(""), true);
        html += wrap(cps.slice(hi).join(""), false);
      }
    }
    return html;
  }

  // cute-dbt#96 concern 2 / cute-dbt#111 / cute-dbt#132 — render a
  // reconstructed inline block diff (domain::pr_diff::BlockDiff) as HTML for a
  // <pre><code>. Content-agnostic: `tokenizeFn` is a TOKENIZER (tokenizeYaml
  // for the test drawer, tokenizeSql for the Model SQL section). Each line is a
  // block-level `.diff-line` span (so a full-width add/remove row tint fills
  // the line) — block layout provides the breaks, so the spans are joined with
  // "" (a literal "\n" would render as a blank row between blocks in a <pre>);
  // each is prefixed by a +/-/space sigil.
  //
  // cute-dbt#132: CHANGED lines are now ALSO syntax-highlighted. We tokenize
  // every line (context and changed), then overlay the intra-line `emphasis`
  // range as <strong> by SPLITTING the token stream at the codepoint range —
  // so a changed line shows syntax colors AND the emphasis marker. (Previously
  // changed lines bypassed highlighting entirely and rendered plain escaped
  // text.) The offsets are CODEPOINT indices (Rust `chars()`), sliced via
  // Array.from — UTF-16 `.slice` would drift on astral chars.
  // cute-dbt#132 — hunk contraction (GitHub-style fold). Long unchanged
  // stretches collapse behind a click/keyboard "Show N unchanged line(s)"
  // control. Frontend-only over the existing DiffLine[] payload.
  //   diffFoldPad     — context lines kept adjacent to a change (head/tail).
  //                     User-configurable live via the .diff-context-input
  //                     control (PR-diff mode only); defaults to 3.
  //                     renderBlockDiff reads it, or an explicit per-call
  //                     override (the headless tests pass one directly).
  //   FOLD_MIN_HIDDEN — only fold when >=2 lines would actually hide, so a
  //                     short YAML test block (a change + a couple context
  //                     lines) NEVER folds.
  //   diffAllExpanded — global expand-all/collapse-all state. The toggle is a
  //                     symmetric DOM mirror (setAllFolds), NOT a re-render, so
  //                     it never disturbs the SQL File<->Diff view or mermaid.
  var diffFoldPad = 3;
  var FOLD_MIN_HIDDEN = 2;
  var diffAllExpanded = false;

  // The per-hunk fold control's label, shared by the per-hunk toggle and the
  // global expand/collapse mirror so they never drift (#136). `count` is the
  // `data-fold-count` string; expanded => a "Hide" (collapse) affordance.
  function foldLabel(expanded, count) {
    return (expanded ? "Hide " : "Show ") + count
         + " unchanged line" + (count === "1" ? "" : "s");
  }

  // Render a single DiffLine to its `.diff-line` span via the shared
  // tokenize + emphasis-overlay path. `extraClass` appends to the kind class
  // (the folded middle passes `diff-folded fold-<id>`); `hidden` toggles the
  // `hidden` attribute (relies on the #122 `[hidden]{display:none!important}`
  // rule, since Sakura's `.diff-line{display:block}` defeats a bare [hidden]).
  // cute-dbt#178 — GitHub-style two-column gutter (old line no | new line
  // no). `null` => blank cell (added lines have no old number; removed have
  // no new). aria-hidden so screen readers read the code, not the numbering
  // scaffold.
  function diffGutter(o, n) {
    return '<span class="diff-gutter" aria-hidden="true">'
         + '<span class="dln dln-o">' + (o == null ? "" : o) + '</span>'
         + '<span class="dln dln-n">' + (n == null ? "" : n) + '</span></span>';
  }

  // Shared: the inner code HTML for a diff line (syntax + optional word-level
  // emphasis overlay). Used by both the unified and split renderers.
  function diffBody(ln, tokenizeFn) {
    var tokens = tokenizeFn(ln.text);
    if (ln.kind !== "context" && ln.emphasis) {
      return emitTokensWithEmphasis(tokens, ln.emphasis[0], ln.emphasis[1]);
    }
    return emitTokens(tokens);
  }

  // Old/new line-number pair for every line, in document order (unified-diff
  // numbering): added has no old number, removed has no new number.
  function diffNumbers(lines) {
    var nums = new Array(lines.length), o = 1, nw = 1;
    for (var p = 0; p < lines.length; p++) {
      var kk = lines[p].kind;
      if (kk === "added")        { nums[p] = { o: null, n: nw++ }; }
      else if (kk === "removed") { nums[p] = { o: o++,  n: null }; }
      else                       { nums[p] = { o: o++,  n: nw++ }; }
    }
    return nums;
  }

  function renderOneDiffLine(ln, tokenizeFn, extraClass, hidden, num) {
    var cls = ln.kind === "added" ? "diff-added"
            : ln.kind === "removed" ? "diff-removed"
            : "diff-context";
    var sigil = ln.kind === "added" ? "+" : ln.kind === "removed" ? "-" : " ";
    return '<span class="diff-line ' + cls + (extraClass ? " " + extraClass : "") + '"'
         + (hidden ? " hidden" : "") + '>'
         + diffGutter(num ? num.o : null, num ? num.n : null)
         + '<span class="diff-sigil">' + sigil + '</span>'
         + '<span class="diff-code">' + diffBody(ln, tokenizeFn) + '</span>'
         + '</span>';
  }

  function renderBlockDiff(diff, tokenizeFn, padOverride) {
    // `padOverride` (>= 0) wins over the module-level diffFoldPad — the
    // headless tests pin specific pads without touching shared state; the
    // production callers omit it and ride the user-configurable value.
    var foldPad = (typeof padOverride === "number" && padOverride >= 0)
      ? padOverride : diffFoldPad;
    var lines = diff.lines;
    var n = lines.length;
    // Precompute the old/new line-number pair for every line, in document
    // order, so the gutters read like a unified diff regardless of folding.
    var nums = diffNumbers(lines);
    var html = "";
    var foldId = 0; // counter LOCAL to this call — parent-scoping makes it safe.
    var i = 0;
    while (i < n) {
      if (lines[i].kind !== "context") {
        html += renderOneDiffLine(lines[i], tokenizeFn, "", false, nums[i]);
        i += 1;
        continue;
      }
      // Maximal run of consecutive context lines: [runStart, j).
      var runStart = i;
      var j = i;
      while (j < n && lines[j].kind === "context") j += 1;
      var runLen = j - runStart;
      var head = runStart > 0 ? foldPad : 0;      // change before -> keep PAD on top
      var tail = j < n ? foldPad : 0;             // change after  -> keep PAD on bottom
      var hidden = runLen - head - tail;
      if (hidden < FOLD_MIN_HIDDEN) {
        // Short run (or short hidden middle): render the whole run normally.
        for (var k = runStart; k < j; k++) {
          html += renderOneDiffLine(lines[k], tokenizeFn, "", false, nums[k]);
        }
      } else {
        var id = foldId++;
        // head context lines (adjacent to the preceding change).
        for (var h = runStart; h < runStart + head; h++) {
          html += renderOneDiffLine(lines[h], tokenizeFn, "", false, nums[h]);
        }
        // The fold control. Bidirectional (#136): it stays in the DOM and
        // relabels Show<->Hide on toggle, so `data-fold-count` + a dedicated
        // label span let toggleFold/setAllFolds rebuild the text without
        // re-rendering. `aria-expanded` tracks the collapsed/expanded state.
        html += '<span class="diff-line diff-fold" data-fold="' + id + '"'
              + ' data-fold-count="' + hidden + '"'
              + ' role="button" tabindex="0" aria-expanded="false">'
              + '<span class="diff-gutter diff-gutter-hunk" aria-hidden="true"></span>'
              + '<span class="diff-sigil">⋯</span>'
              + '<span class="diff-fold-label">'
              + 'Show ' + hidden + ' unchanged line' + (hidden === 1 ? "" : "s")
              + '</span>'
              + '</span>';
        // The folded (hidden) middle lines.
        for (var m2 = runStart + head; m2 < j - tail; m2++) {
          html += renderOneDiffLine(lines[m2], tokenizeFn, "diff-folded fold-" + id, true, nums[m2]);
        }
        // tail context lines (adjacent to the following change).
        for (var tIdx = j - tail; tIdx < j; tIdx++) {
          html += renderOneDiffLine(lines[tIdx], tokenizeFn, "", false, nums[tIdx]);
        }
      }
      i = j;
    }
    return html;
  }
  // Expose for headless JS unit tests (parallels __cuteHighlightYaml). The
  // #96 name is kept as a thin alias so existing headless assertions and the
  // YAML call site stay stable; SQL callers pass `tokenizeSql`.
  function renderYamlDiff(diff) { return renderBlockDiff(diff, tokenizeYaml); }
  window.__cuteRenderBlockDiff = renderBlockDiff;
  window.__cuteRenderYamlDiff = renderYamlDiff;

  // cute-dbt#178 — SPLIT (side-by-side) diff. Old lines on the left, new on
  // the right, paired index-wise within each change block; context appears on
  // both sides. No folding in split mode (these blocks are short — showing
  // every context line reads cleaner than wiring a second fold mechanism).
  // Rendered as a fixed-layout table so the two halves stay aligned; each
  // code cell soft-wraps. The CSS picks unified vs split per
  // html[data-difflayout] (+ a responsive fallback), so BOTH are emitted and
  // exactly one is shown. Consumes the same BlockDiff lines verbatim — the
  // Rust diff engine is unchanged.
  function splitCell(side) {
    if (!side) {
      return '<td class="ds-num ds-empty"></td><td class="ds-code ds-empty"></td>';
    }
    return '<td class="ds-num ds-' + side.cls + '">' + (side.num == null ? "" : side.num) + '</td>'
         + '<td class="ds-code ds-' + side.cls + '"><div class="ds-codewrap">'
         + '<span class="diff-sigil">' + side.sigil + '</span>'
         + '<span class="diff-code">' + side.body + '</span></div></td>';
  }
  function renderSplitDiff(diff, tokenizeFn) {
    var lines = diff.lines, n = lines.length, nums = diffNumbers(lines);
    var rows = "", i = 0;
    while (i < n) {
      if (lines[i].kind === "context") {
        var body = diffBody(lines[i], tokenizeFn);
        rows += '<tr class="ds-row">'
              + splitCell({ cls: "context", num: nums[i].o, sigil: " ", body: body })
              + splitCell({ cls: "context", num: nums[i].n, sigil: " ", body: body })
              + '</tr>';
        i += 1;
        continue;
      }
      // Maximal change block; pair removed (left) with added (right).
      var rem = [], add = [];
      while (i < n && lines[i].kind !== "context") {
        if (lines[i].kind === "removed") rem.push(i); else add.push(i);
        i += 1;
      }
      var maxL = Math.max(rem.length, add.length);
      for (var x = 0; x < maxL; x++) {
        var L = x < rem.length
          ? { cls: "removed", num: nums[rem[x]].o, sigil: "-", body: diffBody(lines[rem[x]], tokenizeFn) } : null;
        var R = x < add.length
          ? { cls: "added", num: nums[add[x]].n, sigil: "+", body: diffBody(lines[add[x]], tokenizeFn) } : null;
        rows += '<tr class="ds-row">' + splitCell(L) + splitCell(R) + '</tr>';
      }
    }
    return '<table class="diff-split"><tbody>' + rows + '</tbody></table>';
  }

  // Emit BOTH layouts into a diff <pre>: the unified <code> + the split
  // table. CSS shows exactly one (html[data-difflayout] + the responsive
  // fallback). `codeClass` lets the Model-SQL caller keep its hook class.
  function diffViewsHtml(diff, tokenizeFn, codeClass) {
    return '<code class="diff-unified' + (codeClass ? " " + codeClass : "") + '">'
         + renderBlockDiff(diff, tokenizeFn) + '</code>'
         + renderSplitDiff(diff, tokenizeFn);
  }
  window.__cuteRenderSplitDiff = renderSplitDiff;

  // cute-dbt#132 — global fold controls (configurable context lines +
  // expand-all/collapse-all). setAllFolds is a SYMMETRIC DOM MIRROR over the
  // already-rendered diff: expand reveals every folded middle line; collapse
  // re-hides them — exactly reproducing renderBlockDiff's default-folded output
  // WITHOUT a re-render, so the SQL File<->Diff toggle, mermaid, and scroll
  // position are never disturbed. The per-hunk controls stay VISIBLE and
  // relabel Show<->Hide (#136 bidirectional), so an individual hunk can still
  // be toggled after a global op. `root` scopes the op (the report passes
  // `document`; the headless tests pass a mounted node).
  function setAllFolds(expanded, root) {
    var scope = root || document;
    var folded = scope.querySelectorAll(".diff-folded");
    for (var i = 0; i < folded.length; i++) folded[i].hidden = !expanded;
    var controls = scope.querySelectorAll(".diff-fold");
    for (var k = 0; k < controls.length; k++) {
      controls[k].setAttribute("aria-expanded", expanded ? "true" : "false");
      var label = controls[k].querySelector(".diff-fold-label");
      if (label) label.textContent = foldLabel(expanded, controls[k].getAttribute("data-fold-count"));
    }
  }
  window.__cuteExpandAllFolds = function (root) { setAllFolds(true, root); };
  window.__cuteCollapseAllFolds = function (root) { setAllFolds(false, root); };

  // Sync the global Expand/Collapse-all button label + aria-pressed to
  // diffAllExpanded. Null-safe: the .diff-view-controls strip is is_pr_diff-
  // gated and absent in baseline mode.
  function renderExpandAllToggle() {
    var $btn = $(".diff-expand-all");
    if (!$btn.length) return;
    $btn.text(diffAllExpanded ? "Collapse all" : "Expand all")
        .attr("aria-pressed", diffAllExpanded ? "true" : "false");
  }
  // Any re-render emits freshly default-folded diffs, so the global toggle
  // returns to "Expand all" (called at the tail of renderForSelectedModel).
  function resetExpandAllToggle() {
    diffAllExpanded = false;
    renderExpandAllToggle();
  }

  // Wire the PR-diff-only diff-view controls strip. Idempotent-safe binding;
  // returns early in baseline mode where the strip is not emitted. The
  // configurable fold-context input now lives in the #139 settings panel; this
  // strip retains only the global expand-all/collapse-all action.
  function bindDiffViewControls() {
    var $strip = $(".diff-view-controls");
    if (!$strip.length) return;
    // Global expand-all/collapse-all over every currently-mounted fold (the
    // model SQL diff + the test YAML diff), per-hunk controls still work too.
    $strip.find(".diff-expand-all").on("click", function () {
      diffAllExpanded = !diffAllExpanded;
      setAllFolds(diffAllExpanded, document);
      renderExpandAllToggle();
    });
  }

  // cute-dbt#139 — wire the settings cog + panel. PR-diff mode only; returns
  // early in baseline mode where the cog is not emitted. The cog toggles a
  // non-blocking panel (Escape + outside-click close, focus returns to the
  // cog). The two settings drive PURE presentation:
  //   contextLines  -> diffFoldPad, then renderForSelectedModel() re-folds the
  //                    live diffs (the same re-render path #132's input used).
  //   normalize     -> flips the cell lens (cellChanged); a re-render re-tints
  //                    the cell-diff grids under the new lens.
  // Both persist via saveSettings (localStorage where available; in-memory
  // under file://). The controls hydrate to the loaded settings at bind time.
  function bindSettingsMenu() {
    var $cog = $(".settings-cog");
    if (!$cog.length) return;
    var $panel = $("#settings-panel");
    var $ctx = $("#settings-context-input");
    var $norm = $("#settings-normalize-input");

    // Hydrate the controls from the (possibly persisted) settings.
    $ctx.val(settings.contextLines);
    $norm.prop("checked", settings.normalizeEquality);

    function setOpen(open) {
      $panel.prop("hidden", !open);
      $cog.attr("aria-expanded", open ? "true" : "false");
    }
    $cog.on("click", function () {
      setOpen($cog.attr("aria-expanded") !== "true");
    });
    // Escape closes (from anywhere) and returns focus to the cog. Outside-click
    // closes too; clicks inside the panel/cog are ignored.
    $(document).on("keydown", function (e) {
      if ((e.key === "Escape" || e.key === "Esc") && $cog.attr("aria-expanded") === "true") {
        setOpen(false);
        $cog.trigger("focus");
      }
    });
    $(document).on("click", function (e) {
      if ($cog.attr("aria-expanded") !== "true") return;
      if ($(e.target).closest(".settings-cog-wrap").length) return;
      setOpen(false);
    });

    // Context-lines: clamp to [0,20], re-fold the live diffs, persist.
    $ctx.on("change", function () {
      var v = parseInt($(this).val(), 10);
      if (isNaN(v)) { $(this).val(settings.contextLines); return; }
      settings.contextLines = Math.max(0, Math.min(20, v));
      diffFoldPad = settings.contextLines;
      $(this).val(settings.contextLines);
      saveSettings();
      renderForSelectedModel();
    });
    // Normalize-equality: flip the cell lens, re-render to re-tint, persist.
    $norm.on("change", function () {
      settings.normalizeEquality = $(this).prop("checked");
      saveSettings();
      renderForSelectedModel();
    });
  }

  function copySql(text, $btn) {
    function done(ok) {
      $btn.addClass(ok ? "copied" : "copy-failed").text(ok ? "Copied" : "Failed");
      setTimeout(function () {
        $btn.removeClass("copied copy-failed").text("Copy");
      }, 1500);
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(function () { done(true); }, function () {
        fallbackCopy(text, done);
      });
    } else {
      fallbackCopy(text, done);
    }
  }
  function fallbackCopy(text, done) {
    var ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    var ok = false;
    try { ok = document.execCommand("copy"); } catch (e) { ok = false; }
    document.body.removeChild(ta);
    done(ok);
  }

  // The Current-view grid, rendered PURELY from the Rust-computed
  // `FixtureTable` POD (cute-dbt#138). Each cell is a `{display, key}` Cell:
  // the text is the authored `display` (so a csv `1.00` shows `1.00`, not the
  // canonicalized `1`), while the numeric / NULL styling + sort order come
  // from the canonical `key` (a `Number` key sorts numerically even when its
  // display is `1.00`). The JS no longer parses csv/dict — the domain's
  // `table_from_manifest_rows` owns that, and ships this POD on the wire.
  // cute-dbt#165 — the column-header tooltip button for one column's
  // Rust-computed metadata POD ({description, tests}). Mirrors the #146
  // expect-tooltip contract: a focusable <button> with a CSS bubble shown on
  // hover AND keyboard focus (a native `title` is hover-delayed, keyboard-
  // unreachable, touch-invisible); `aria-label` carries the same text for
  // assistive tech (the bubble is aria-hidden so it is not announced twice).
  // DOM is built from text nodes only — payload strings never parse as HTML.
  function buildColTooltip(colName, meta) {
    var tests = meta.tests || [];
    var parts = [];
    if (meta.description) parts.push(meta.description);
    if (tests.length) parts.push("column tests: " + tests.join("; "));
    var $btn = $("<button>")
      .attr("type", "button")
      .addClass("col-tooltip")
      .attr("aria-label", "Column " + colName + " — " + parts.join(" — "));
    $btn.append(document.createTextNode("ⓘ"));
    var $bubble = $("<span>").addClass("col-tooltip-bubble").attr("aria-hidden", "true");
    if (meta.description) {
      $bubble.append($("<span>").addClass("col-tooltip-desc").text(meta.description));
    }
    if (tests.length) {
      $bubble.append($("<span>").addClass("col-tooltip-tests-label").text("column tests"));
      var $list = $("<ul>").addClass("col-tooltip-tests");
      tests.forEach(function (t) { $list.append($("<li>").text(t)); });
      $bubble.append($list);
    }
    $btn.append($bubble);
    return $btn;
  }

  // cute-dbt#165 — the bubble is position:fixed so it escapes the .table-fit
  // overflow scroller (which clips absolutely-positioned descendants); the
  // coordinates are set here on hover/focus while VISIBILITY stays pure CSS
  // (the #146 :hover/:focus contract). Horizontally clamped so a first/last-
  // column bubble stays on-viewport. A hidden bubble still has layout
  // geometry (visibility:hidden, not display:none), so offsetWidth is real.
  function positionColTooltip(btn) {
    var bubble = btn.querySelector(".col-tooltip-bubble");
    if (!bubble) return;
    var r = btn.getBoundingClientRect();
    var half = bubble.offsetWidth / 2;
    var margin = 8;
    var cx = r.left + r.width / 2;
    cx = Math.max(margin + half, Math.min(cx, window.innerWidth - margin - half));
    bubble.style.left = cx + "px";
    bubble.style.top = (r.bottom + 6) + "px";
  }
  $(document).on("mouseenter focusin", ".col-tooltip", function () {
    positionColTooltip(this);
  });
  // A th click sorts the DataTable; clicking/tapping the info button must
  // only reveal the bubble (via focus), never also re-sort the column.
  $(document).on("click", ".col-tooltip", function (e) {
    e.preventDefault();
    e.stopPropagation();
  });

  // `colMeta` (cute-dbt#165) is the per-table column-metadata map keyed by
  // column name (absent key/entry => no affordance on that th).
  function buildTable(table, cls, colMeta) {
    var columns = (table && table.columns) || [];
    var rows = (table && table.rows) || [];
    var $wrap = $("<div>").addClass("table-fit");
    var $tbl = $("<table>").addClass(cls);
    var $thead = $("<thead>").appendTo($tbl);
    var $hr = $("<tr>").appendTo($thead);
    columns.forEach(function (c) {
      var html = escapeHtml(c).replace(/_/g, "_<wbr>");
      var $th = $("<th>").attr("title", c).html(html);
      var meta = colMeta && colMeta[c];
      if (meta) $th.append(buildColTooltip(c, meta));
      $hr.append($th);
    });
    var $tbody = $("<tbody>").appendTo($tbl);
    rows.forEach(function (r) {
      var $tr = $("<tr>").appendTo($tbody);
      (r.cells || []).forEach(function (cell) {
        $tr.append(decorateCurrentCell($("<td>"), cell));
      });
    });
    $wrap.append($tbl);
    return $wrap;
  }

  // Render one Current-view Cell (`{display, key}`) into a <td>: the text is
  // the authored `display`; the styling/sort affordance is read from `key.t`
  // (the canonical type), never from the display token (cute-dbt#138). Shares
  // the null/number styling with `decorateValueCell` so Current and Diff read
  // identically.
  function decorateCurrentCell($td, cell) {
    var key = cell && cell.key;
    if (!key || key.t === "null") {
      return $td.addClass("cell-null").text("NULL");
    }
    if (key.t === "absent") {
      return $td.addClass("cell-absent").text("");
    }
    if (key.t === "number") {
      // Sort numerically on the canonical value; show the authored display.
      return $td.addClass("cell-num").attr("data-order", key.v).text(cell.display);
    }
    return $td.text(cell.display);
  }

  // cute-dbt#98 ----------------------------------------------------------
  //
  // The structured cell-diff renderer + the per-table Current↔Diff toggle.
  // Mirrors the #96 Authored↔Diff drawer idiom (a `.cell-diff-toggle` of two
  // `.yaml-view-btn`s, `data-view`, `.active`, and `prop("hidden", …)` show/
  // hide reusing the #121 `[hidden]{display:none!important}` rule). When the
  // test carries no cell diff for this table the view is the plain Current
  // grid — byte-identical to the historic `buildTable` output, so baseline-
  // mode reports never drift.

  // True when the fixture's data lives in an external file: dbt's `fixture:`
  // key is set AND `rows` is null (the manifest does NOT inline the data).
  function isExternalFixture(obj) {
    return Boolean(obj && obj.fixture && (obj.rows === null || obj.rows === undefined));
  }

  // cute-dbt#126 — the fixture data lives in an external file AND was LOADED
  // (the reader inlined it, so `rows` is the file body). The complement of
  // isExternalFixture over a `fixture`-bearing obj: loaded → render the grid /
  // sql code block + a provenance chip; unloaded (unreadable) → the affordance.
  function isLoadedExternalFixture(obj) {
    return Boolean(obj && obj.fixture && obj.rows !== null && obj.rows !== undefined);
  }

  // A small "from <path>" chip noting the fixture's external source, shown
  // alongside the loaded grid / sql code block. Inline fixtures omit `fixture`
  // so this never renders for them (byte-identity of existing reports holds).
  function buildFixtureProvenanceChip(name) {
    return $("<span>").addClass("fixture-provenance")
      .append(document.createTextNode("from "))
      .append($("<code>").text(String(name)));
  }

  // The external-fixture affordance: never a silently-empty grid. Names the
  // file and points the reader at the Authoring YAML drawer (the #96 view),
  // which surfaces the authored block including the `fixture:` line.
  function buildExternalFixtureAffordance(name) {
    var $box = $("<div>").addClass("given-empty external-fixture-note");
    $box.append(document.createTextNode("data in external fixture file: "));
    $box.append($("<code>").addClass("td-defined-in").text(String(name)));
    $box.append(document.createTextNode(" — see the Authoring YAML drawer above for this test's block."));
    return $box;
  }

  // A FixtureTable's cell view: build the Current grid, and — when a cell diff
  // is present — also build the Diff grid and a Current↔Diff toggle that flips
  // exactly one view visible at a time (the inactive view carries `hidden`).
  // Default-Diff when a diff exists, else Current with no toggle.
  function buildFixtureView(table, cls, diff, colMeta) {
    if (!diff) {
      // No cell diff for this table → the plain Current grid from the POD.
      return buildTable(table, cls, colMeta);
    }
    var $wrap = $("<div>").addClass("fixture-view");
    var $diffView = buildDiffTable(diff, cls, colMeta);
    var $currentView = buildTable(table, cls, colMeta).prop("hidden", true);

    var $toggle = $("<div>").addClass("cell-diff-toggle");
    var $diffBtn = $("<button>").attr("type", "button")
      .addClass("yaml-view-btn active").attr("data-view", "diff").text("Diff");
    // cute-dbt#178 — labelled "File" for consistency with the other
    // Diff/File toggles (the data-view stays "current": it is the test's
    // current fixture data as authored in the file).
    var $curBtn = $("<button>").attr("type", "button")
      .addClass("yaml-view-btn").attr("data-view", "current").text("File");
    $toggle.append($diffBtn).append($curBtn);
    // Bind to the LOCAL $toggle (not a global `.yaml-view-btn` delegate) so it
    // can never catch the #96/#111 drawer toggles elsewhere in the document.
    $toggle.on("click", ".yaml-view-btn", function () {
      var view = $(this).attr("data-view");
      $toggle.find(".yaml-view-btn").removeClass("active");
      $(this).addClass("active");
      $diffView.prop("hidden", view !== "diff");
      $currentView.prop("hidden", view !== "current");
      // Lazy-init + reflow the table just revealed: a DataTable initialized
      // while `display:none` measures column widths against a 0-width box, so
      // the Current grid is initialized only on first reveal and any already-
      // initialized table gets its widths recomputed once it is on-screen.
      var $shown = view === "diff" ? $diffView : $currentView;
      initDataTablesIn($shown);
      adjustDataTablesIn($shown);
    });

    // cute-dbt#178 — the toggle rides a .fixture-view-bar strip so
    // renderExpectedPanel can relocate the header meta (mode badge, row
    // count, expect-tooltip) onto the same row, GitHub-toolbar style.
    $wrap.append($("<div>").addClass("fixture-view-bar").append($toggle))
         .append($diffView).append($currentView);
    return $wrap;
  }

  // cute-dbt#139 — the per-cell change lens. The #138 wire ships BOTH axes per
  // cell (`old`/`new` each a `{display, key}` Cell) plus the Rust-precomputed
  // `changed = old.key != new.key` (the NORMALIZED verdict). This re-derives
  // the verdict client-side from the active lens WITHOUT a Rust round-trip:
  //   normalized (default): the canonical `key` axis — Rust's `cell.changed`.
  //   strict:               the authored `display` axis — a reformat-but-equal
  //                         cell (`1.00` -> `1`, both keying Number("1")) flags.
  // Row alignment is NEVER re-paired here (it stays Rust-canonical, #138): only
  // a Modified row's cells carry both displays, so the strict lens reveals
  // format-only changes exclusively WITHIN already-Modified rows. Added/Removed
  // cells never participate — their `changed` is intrinsic to the row kind.
  // DOCUMENTED ASYMMETRY (the literal #139 spec, not a bug): the strict lens
  // HIDES a same-display/different-key real change (e.g. Number `1` vs Str
  // `1`), since the displays match. A pure-reformat row stays canonically
  // Unchanged (its OLD display is discarded by the Rust `unchanged_row`), so
  // strict cannot reveal it — closing that gap needs a Rust change and is out
  // of #139's scope.
  function cellChanged(cell) {
    if (!cell) return false;
    if (settings.normalizeEquality) {
      return !!cell.changed;
    }
    return cellText(cell.old) !== cellText(cell.new);
  }

  // The row-modified rollup under the active lens: a Modified row counts as
  // modified iff >=1 of its cells flags under cellChanged. The normalize toggle
  // never re-pairs rows; it only re-rolls this presentation flag (the row stays
  // a Rust-`Modified` row even when the lens flags no cell, e.g. a row whose
  // only delta is format-only and the lens is normalized).
  function rowFlagged(rc) {
    return rc.kind === "modified" && rc.cells.some(cellChanged);
  }

  // Build the Diff grid from a FixtureTableDiff: a unified column axis (with
  // added/removed column badges from DiffColumn.status) and rows tinted by
  // RowChangeKind. A `Modified` cell flagged by the active lens (cellChanged)
  // renders inline `old → new`; otherwise the cell shows its current value.
  function buildDiffTable(diff, cls, colMeta) {
    var $wrap = $("<div>").addClass("table-fit");
    var $tbl = $("<table>").addClass(cls).addClass("cell-diff-table");
    var $thead = $("<thead>").appendTo($tbl);
    var $hr = $("<tr>").appendTo($thead);
    diff.columns.forEach(function (c) {
      var html = escapeHtml(c.name).replace(/_/g, "_<wbr>");
      var $th = $("<th>").attr("title", c.name).html(html);
      if (c.status === "added") {
        $th.addClass("col-added").append($("<span>").addClass("col-status").text("added"));
      } else if (c.status === "removed") {
        $th.addClass("col-removed").append($("<span>").addClass("col-status").text("removed"));
      }
      // cute-dbt#165 — the Diff grid's unified column axis shares the same
      // metadata map (a removed column simply has no entry — the map is
      // filtered to the CURRENT table's columns in Rust).
      var meta = colMeta && colMeta[c.name];
      if (meta) $th.append(buildColTooltip(c.name, meta));
      $hr.append($th);
    });
    var $tbody = $("<tbody>").appendTo($tbl);
    diff.rows.forEach(function (rc) {
      var $tr = $("<tr>").appendTo($tbody);
      if (rc.kind === "added")   $tr.addClass("row-added");
      if (rc.kind === "removed") $tr.addClass("row-removed");
      // cute-dbt#139 — the row-modified tint is the lens rollup, not the raw
      // Rust kind: a Modified row whose only delta is format-only sheds its
      // tint under the normalized lens (no cell flags), and regains it under
      // strict. Added/Removed tints are intrinsic and never lens-driven.
      if (rowFlagged(rc)) $tr.addClass("row-modified");
      rc.cells.forEach(function (cell) {
        $tr.append(buildDiffCell(cell, rc.kind));
      });
    });
    $wrap.append($tbl);
    return $wrap;
  }

  // One diff cell. For a Removed row we show the old value; for an Added row
  // the new value; for an Unchanged row the (shared) value; for a Modified
  // row, a changed cell renders `old → new` inline, an unchanged cell its
  // value. `data-order` carries a sort key so DataTables orders the column
  // sensibly even on the composite old→new rendering.
  function buildDiffCell(cell, rowKind) {
    var $td = $("<td>");
    if (rowKind === "removed") {
      return decorateValueCell($td, cell.old);
    }
    if (rowKind === "added" || rowKind === "unchanged" || !cellChanged(cell)) {
      // Not flagged (added/unchanged, or — under the active lens — a cell the
      // normalize toggle treats as unchanged): show the NEW authored value, no
      // red/green (cute-dbt#138/#139).
      return decorateValueCell($td, cell.new);
    }
    // Modified + changed → inline old → new, each side showing its authored
    // display. Each side is type-aware via diffToken: a real null/absent keeps
    // its own neutral styling and never borrows the value-change red/green
    // (cute-dbt#132). The sort key is the NEW cell's authored display.
    $td.addClass("cell-changed").attr("data-order", cellText(cell.new));
    $td.append(diffToken(cell.old, "cell-old"));
    $td.append($("<span>").addClass("cell-arrow").html("&rarr;"));
    $td.append(diffToken(cell.new, "cell-new"));
    return $td;
  }

  // One side of an inline `old → new` changed cell. `cell` is a `{display,
  // key}` Cell (cute-dbt#138): the text is the authored `display`; the
  // neutral-vs-value styling is read from `key.t` so a real null or absent
  // cell reads in its own neutral styling rather than the brilliant red/green
  // reserved for a real value change: null → italic muted-gray "NULL"; absent
  // → blank; any concrete value (including the string 'null') → the side's
  // color via `sideClass` (.cell-old = red, .cell-new = green). This keeps a
  // real NULL visually distinct from a string 'null' even mid-diff (#132).
  function diffToken(cell, sideClass) {
    var key = cell && cell.key;
    if (!key || key.t === "null") {
      return $("<span>").addClass("cell-null").text("NULL");
    }
    if (key.t === "absent") {
      return $("<span>").addClass("cell-absent").text("");
    }
    return $("<span>").addClass(sideClass).text(cell.display);
  }

  // Render a single `{display, key}` Cell into a plain value cell, matching
  // `buildTable`'s null/number styling so the Current and Diff views read
  // identically. Text is the authored `display`; styling/sort from `key`.
  function decorateValueCell($td, cell) {
    return decorateCurrentCell($td, cell);
  }

  // The display text of a `{display, key}` Cell (cute-dbt#138): a `null` key
  // renders the word NULL, an `absent` key renders empty, otherwise the
  // authored `display` token verbatim. Used for the changed-cell sort key.
  function cellText(cell) {
    var key = cell && cell.key;
    if (!key) return "";
    if (key.t === "null") return "NULL";
    if (key.t === "absent") return "";
    return cell.display == null ? "" : String(cell.display);
  }

  // Re-measure DataTables column widths for any (now-visible) table under
  // `$scope`. A no-op for tables not yet wrapped by DataTables. cute-dbt#98 —
  // a table initialized while `display:none` must be re-adjusted on reveal.
  function adjustDataTablesIn($scope) {
    if (!($.fn && $.fn.DataTable)) return;
    $scope.find("table.given-table, table.expected-table").each(function () {
      if ($.fn.DataTable.isDataTable(this)) {
        $(this).DataTable().columns.adjust();
      }
    });
  }
  // Exposed for headless JS unit tests (cute-dbt#98 — parallels the
  // `__cuteRenderYamlDiff` seam). Takes the Rust-computed `FixtureTable` POD
  // (cute-dbt#138) + an optional cell diff and returns the outer wrapper
  // element so a test can assert the authored-display rendering, the toggle,
  // and the tinted markup.
  if (typeof window !== "undefined") {
    window.__cuteBuildFixtureView = function (table, cls, diff, colMeta) {
      return buildFixtureView(table, cls, diff, colMeta)[0];
    };
    window.__cuteCellText = cellText;
    // cute-dbt#139 — the cell-change lens + the settings object, exposed so the
    // headless tests can pin the normalized/strict verdict on a `{old,new,
    // changed}` cell without driving the full panel, and flip the lens directly.
    window.__cuteCellChanged = cellChanged;
    window.__cuteSettings = settings;
  }

  function initDataTablesIn($scope) {
    $scope.find("table.given-table, table.expected-table").each(function () {
      // cute-dbt#98 — never initialize a DataTable while its host is hidden:
      // column widths measured against a 0-width box render wrong on reveal.
      // The Current↔Diff toggle handler lazy-inits the hidden view when first
      // shown (see buildFixtureView). `offsetParent === null` ⇒ not rendered.
      if (this.offsetParent === null) return;
      if ($.fn && $.fn.DataTable) {
        if ($.fn.DataTable.isDataTable(this)) return;
        $(this).DataTable({
          paging: false,
          info: false,
          searching: false,
          scrollX: false,
          ordering: true,
          order: []
        });
      }
    });
    reflowPanelsForOverflow();
  }

  var reflowScheduled = false;
  function reflowPanelsForOverflow() {
    if (reflowScheduled) return;
    reflowScheduled = true;
    requestAnimationFrame(function () {
      reflowScheduled = false;
      var $row = $(".panel-row");
      if (!$row.length) return;
      $row.removeClass("is-stacked");
      void $row[0].offsetWidth;
      var needsStack = false;
      $row.find(".table-fit").each(function () {
        if (this.scrollWidth > this.clientWidth + 1) { needsStack = true; }
      });
      if (needsStack) $row.addClass("is-stacked");
    });
  }

  $(window).on("resize", reflowPanelsForOverflow);

})();
/* end of cute-dbt report interaction engine v1 (cute-dbt#178) */
