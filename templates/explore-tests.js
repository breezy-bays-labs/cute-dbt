/* cute-dbt explore tests viewer v1 (cute-dbt#102)
   ----------------------------------------------------------------------------
   The unit-test viewer on explore's tests.html: renders the selected
   test's description, badges, given fixtures and expected table into the
   SHARED test-card partial (templates/partials/test-card.html — the same
   markup report.html renders), reading the embedded `cute-dbt-data`
   ReportPayload (the build_payload reuse seam).

   Deliberately a LIGHT sibling of the report's interaction engine, not a
   copy: no jQuery, no DataTables, no diff toggles (explore is
   full-manifest — there is no changed-set and no PR diff to tint), no
   Mermaid/Cytoscape. The Current-view fixture grids render the SAME
   Rust-computed FixtureTable POD ({display, key} cells, cute-dbt#138)
   the report renders, so the two surfaces cannot drift on cell
   semantics.

   Affordances:
   - the test selector (the partial's #test-select) lists every unit
     test, optgrouped by model;
   - each index row's test-name button (data-test-id) selects that test
     in the viewer;
   - NULL / absent cells keep the report's vocabulary (.cell-null /
     .cell-absent); sql-format fixtures render as a code block; external
     fixture files surface the provenance note instead of a silently
     empty grid (the cute-dbt#126 contract).

   DOM discipline: every dynamic string lands via createElement +
   textContent — never an HTML-string sink (the asset_embed banner test
   pins this). First-party, NOT vendored: lives at templates/explore-tests.js,
   embedded via asset_embed::EXPLORE_TESTS_JS (include_str!). Banner-pin
   + end-of-file-sentinel tests guard the include. */
(function () {
  "use strict";

  var carrier = document.getElementById("cute-dbt-data");
  var viewer = document.querySelector(".explore-viewer");
  var select = document.getElementById("test-select");
  if (!carrier || !viewer || !select) return;
  var data = JSON.parse(carrier.textContent);

  // Flat id -> {model, test} index over the payload (prototype-less so a
  // hostile test id can never collide with a built-in property).
  var byId = Object.create(null);
  var total = 0;
  (data.models || []).forEach(function (m) {
    (m.tests || []).forEach(function (t) {
      byId[t.id] = { model: m, test: t };
      total += 1;
    });
  });
  if (!total) {
    viewer.hidden = true;
    return;
  }

  // ---- tiny DOM helpers (textContent only) --------------------------------
  function el(tag, cls, text) {
    var node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  function rowsText(n) {
    return n + " row" + (n === 1 ? "" : "s");
  }

  // ---- fixture-shape predicates (the interaction.js vocabulary) -----------
  function isSqlCodeBlock(obj) {
    return Boolean(obj && obj.format === "sql" && typeof obj.rows === "string");
  }

  function isExternalFixture(obj) {
    return Boolean(obj && obj.fixture && (obj.rows === null || obj.rows === undefined));
  }

  function isLoadedExternalFixture(obj) {
    return Boolean(obj && obj.fixture && obj.rows !== null && obj.rows !== undefined);
  }

  function provenanceChip(name) {
    var chip = el("span", "fixture-provenance", "from ");
    chip.appendChild(el("code", "", String(name)));
    return chip;
  }

  function externalNote(name) {
    var box = el("div", "given-empty external-fixture-note",
      "data in external fixture file: ");
    box.appendChild(el("code", "", String(name)));
    return box;
  }

  function sqlBlock(text) {
    var pre = el("pre", "sql-block");
    pre.appendChild(el("code", "", String(text)));
    return pre;
  }

  // ---- the Current-view grid off the Rust FixtureTable POD ----------------
  function cellTd(cell) {
    var td = document.createElement("td");
    var key = cell && cell.key;
    if (!key || key.t === "null") {
      td.className = "cell-null";
      td.textContent = "NULL";
      return td;
    }
    if (key.t === "absent") {
      td.className = "cell-absent";
      return td;
    }
    if (key.t === "number") td.className = "cell-num";
    td.textContent = cell.display;
    return td;
  }

  function buildTable(table, cls) {
    var tbl = el("table", cls);
    var thead = document.createElement("thead");
    var hr = document.createElement("tr");
    (table.columns || []).forEach(function (c) {
      hr.appendChild(el("th", "", c));
    });
    thead.appendChild(hr);
    tbl.appendChild(thead);
    var tbody = document.createElement("tbody");
    (table.rows || []).forEach(function (r) {
      var tr = document.createElement("tr");
      (r.cells || []).forEach(function (cell) {
        tr.appendChild(cellTd(cell));
      });
      tbody.appendChild(tr);
    });
    tbl.appendChild(tbody);
    return tbl;
  }

  // ---- per-test render -----------------------------------------------------
  function renderGiven(g) {
    var sec = el("section", "given-section");
    var hdr = el("div", "table-header");
    hdr.appendChild(el("h4", "table-title", "given · " + g.input));
    if (g.is_this) hdr.appendChild(el("span", "this-badge", "prior model state"));
    if (isLoadedExternalFixture(g)) hdr.appendChild(provenanceChip(g.fixture));
    if (isSqlCodeBlock(g)) {
      hdr.appendChild(el("span", "format-badge", "format: sql"));
      sec.appendChild(hdr);
      sec.appendChild(sqlBlock(g.rows));
      return sec;
    }
    if (isExternalFixture(g)) {
      if (g.format) hdr.appendChild(el("span", "format-badge", "format: " + g.format));
      sec.appendChild(hdr);
      sec.appendChild(externalNote(g.fixture));
      return sec;
    }
    var table = g.table || { columns: [], rows: [] };
    if (g.format && g.format !== "dict") {
      hdr.appendChild(el("span", "format-badge", "format: " + g.format));
    }
    hdr.appendChild(el("span", "row-count-badge", rowsText(table.rows.length)));
    sec.appendChild(hdr);
    sec.appendChild(buildTable(table, "given-table"));
    return sec;
  }

  function renderExpected(t) {
    var body = document.querySelector(".expected-body");
    var rowcount = document.querySelector(".expected-rowcount");
    clear(body);
    if (isLoadedExternalFixture(t.expected)) {
      body.appendChild(provenanceChip(t.expected.fixture));
    }
    if (isSqlCodeBlock(t.expected)) {
      rowcount.textContent = "format: sql";
      body.appendChild(sqlBlock(t.expected.rows));
      return;
    }
    if (isExternalFixture(t.expected)) {
      rowcount.textContent = "external fixture";
      body.appendChild(externalNote(t.expected.fixture));
      return;
    }
    var table = (t.expected && t.expected.table) || { columns: [], rows: [] };
    rowcount.textContent = rowsText(table.rows.length);
    body.appendChild(buildTable(table, "expected-table"));
  }

  function renderTest(t) {
    // Description (the partial's hidden-when-absent contract).
    var desc = document.querySelector(".test-description");
    if (t.description) {
      desc.textContent = t.description;
      desc.hidden = false;
    } else {
      desc.textContent = "";
      desc.hidden = true;
    }
    // Badges: tag chips (kept light — explore has no diff badges).
    var badges = document.querySelector(".test-badges");
    clear(badges);
    (t.tags || []).forEach(function (tag) {
      badges.appendChild(el("span", "tag-badge", tag));
    });
    // Details body: where the test was authored.
    var details = document.querySelector(".test-details-body");
    clear(details);
    if (t.defined_in) {
      var p = el("p", "test-defined-in", "defined in ");
      p.appendChild(el("code", "", t.defined_in));
      details.appendChild(p);
    }
    // Given panel.
    var givenWrap = document.querySelector(".left-panel-body");
    clear(givenWrap);
    if (!t.given || !t.given.length) {
      givenWrap.appendChild(el("div", "empty-hint", "No fixtures defined for this test."));
    } else {
      t.given.forEach(function (g) { givenWrap.appendChild(renderGiven(g)); });
    }
    // Expected panel.
    renderExpected(t);
  }

  function selectTest(id) {
    var entry = byId[id];
    if (!entry) return;
    if (select.value !== id) select.value = id;
    renderTest(entry.test);
  }

  // ---- selector population + wiring ----------------------------------------
  (data.models || []).forEach(function (m) {
    if (!m.tests || !m.tests.length) return;
    var og = document.createElement("optgroup");
    og.label = m.name;
    m.tests.forEach(function (t) {
      var opt = document.createElement("option");
      opt.value = t.id;
      opt.textContent = t.name;
      og.appendChild(opt);
    });
    select.appendChild(og);
  });

  select.addEventListener("change", function () { selectTest(select.value); });

  // Index rows: each test-name button jumps the viewer to its test.
  Array.prototype.forEach.call(
    document.querySelectorAll(".test-jump[data-test-id]"),
    function (btn) {
      btn.addEventListener("click", function () {
        selectTest(btn.getAttribute("data-test-id"));
        if (viewer.scrollIntoView) viewer.scrollIntoView({ block: "start" });
      });
    }
  );

  // Boot: the first test in payload order.
  selectTest(select.value);
})();
/* end of cute-dbt explore tests viewer v1 (cute-dbt#102) */
