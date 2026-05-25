# Zero egress — the privacy property

cute-dbt's load-bearing trait is that the generated `report.html`
opens via `file://` and makes **zero outbound network requests**.
Your manifest data — schema names, column names, fixture rows — never
leaves your machine through the report.

This page explains what that means, how it is mechanically enforced,
and how anyone (you, your security team, an auditor) can re-verify
it without taking our word for anything.

## What "zero egress" means concretely

When you open `report.html` in a browser via `file://`:

- No `<script src="https://…">` loads.
- No `<link rel="stylesheet" href="https://…">` loads.
- No `<img src="https://…">` loads.
- No CSS `@import url("https://…")`.
- No CSS `url("…")` resolving to a remote resource.
- No protocol-relative `//cdn.example.com/…` references.
- No fetch / XMLHttpRequest from inlined JavaScript.
- No Mermaid runtime fetching webfonts or remote layouts (Mermaid is
  embedded as a UMD bundle, initialized with `securityLevel: 'strict'`
  and an explicit system-font stack).

The report is, structurally, a self-contained text file. Open it on
an airplane; open it with `--disable-network` in your browser; open
it in a network-blocked sandbox. The behavior is identical.

## How this is enforced

Two mechanical checks, both re-runnable from a fresh clone:

### Primary: headless-browser network-block proof

A test in the repo opens the committed `examples/jaffle-shop-report.html`
in a real headless Chromium via `file://`, **with all network access
denied via Chrome DevTools Protocol**, and subscribes to every
`Network.requestWillBeSent` event. Any HTTP / HTTPS / WebSocket
request — even an inert pre-flight — fails the test.

```sh
# From a fresh clone
cargo test --test zero_egress  # the test name will vary; see the file
```

This is the **load-bearing artifact**. It runs on every PR in CI as
the `Headless zero-egress proof (file://, network blocked)` required
check.

### Secondary: structured resource-ref lint

A separate test parses the generated HTML with a real HTML parser
(`tl`) and rejects every "real loading construct" — the bullet list
above (`<script src>`, `<link href>`, `<img src>`, CSS `@import`,
CSS `url()`, protocol-relative `//`). This is **not** a `grep http`
— minified JavaScript bundles carry hundreds of inert URL string
literals that a naive grep would false-positive on. The structured
parser scopes the lint to **DOM positions that actually trigger
loads**.

This is the `Resource-ref lint (real loading constructs)` required
check.

## Why two checks

- The **structured lint** is fast and tells you *what* loading
  construct was introduced, with a precise line number.
- The **headless network-block proof** is the *truth* — it asks the
  browser, "did anything try to leave the file://?" That's the
  question the user actually cares about.

If a future change introduces a load via a path the structured lint
hasn't been taught about (e.g., a service worker, an iframe, a
dynamic `import()`), the headless proof still catches it. The two
checks are belt-and-braces.

## What is NOT bundled

cute-dbt is also **zero telemetry** — no analytics, no crash
reporting, no auto-update — but that's a property of the **CLI
binary**, not the report. This page is specifically about the
generated HTML.

## Vendored assets

Every embedded asset (Sakura CSS, jQuery, DataTables, Mermaid) is
listed in
[`assets/MANIFEST.toml`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/assets/MANIFEST.toml)
with its pinned version, SHA-256, and SPDX license. A CI guard refuses
to merge if `assets/` contains a file not listed in `MANIFEST.toml`.

The `AUDIT.md` file at the repo root indexes the full re-runnable
auditability package — the two checks above, plus the manifest, plus
`Cargo.lock`, plus the synthetic-only fixture manifest. A reviewer
running every artifact in `AUDIT.md` ends with end-to-end confidence
in the zero-egress property.
