//! Vars change attribution at honest tiers (cute-dbt#268, epic #262 C3).
//!
//! A `vars:` edit in `dbt_project.yml` is invisible to the engines' own
//! `state:modified` (fusion compares `unrendered_config` strings, which
//! keep the literal `{{ var(…) }}` text unchanged — `nodes.rs:1005-1035`
//! @ `9977b6cb…`), and manifest v12 records **no** per-node var usage
//! anywhere (`NodeDependsOn` = `{macros, nodes, nodes_with_ref_location}`
//! only). So attribution here is a **static scan** over what the
//! manifest already carries, tiered by evidence strength:
//!
//! - **DIRECT** — `var('x')` found in a Jinja-evaluated region of the
//!   model's `raw_code` (call forms exhaustive per fusion
//!   `VarFunction::parse_args`, `dbt-jinja-vars/src/var.rs:25-92,111-126`
//!   @ `9977b6cb…`: `var('x')` / `var("x")`, positional or `default=`
//!   kwarg defaults incl. map defaults, and the `var.has_var('x')`
//!   method form).
//! - **CONFIG** — `var('x')` found in a string value of the node's
//!   `unrendered_config` (recursive walk; all three config sources are
//!   captured Jinja-preserving — `resolve_utils.rs:42-80`,
//!   `utils.rs:620-729` — and hooks merge into the same map).
//! - **MACRO** — `var('x')` found in the `macro_sql` of the transitive
//!   closure of the model's `depends_on.macros` (the cute-dbt#271 wire
//!   family; both engines populate it on real manifests, dispatch
//!   indirection included — verified 2026-06-12 on core 1.11 and fusion
//!   2.0-preview.177).
//! - **UNKNOWN residual** — what no static scan can rule out: dynamic
//!   `var(expr)` names, var-to-var value indirection, CLI `--vars`
//!   masking, Python models. Never silent: the render layer states the
//!   enumerated causes in-row with "at least N" framing.
//!
//! Resolution mirrors fusion's precedence EXACTLY
//! (`configured_var.rs:55-129` @ `9977b6cb…`): CLI `--vars` (highest,
//! unobservable here — an explicit caveat, never a guess) > the
//! package-scoped project var > the global project var > the inline
//! `var()` default (LOWEST — a call-site default never overrides a
//! project value, so carrying a default never subtracts a model from
//! the blast radius). Root-project vars extend into every installed
//! package's namespace (`load_vars.rs:28-42`), so a global edit reaches
//! package models too — unless the package pins the same name in its
//! own scope, which masks the edit for exactly that package.
//!
//! **Contextualize, don't widen** (the founder's v3 frame): nothing
//! here touches scope selection. The attribution rides the panel's vars
//! rows ([`VarChangeFacts`] on [`ProjectChange::vars`]) and the
//! in-scope models' reference chips ([`VarReference`]); the in-scope
//! set is computed elsewhere and never grows because of a var edit.
//!
//! Unit tests that pin an edited var in `overrides.vars` are
//! **insulated** from the project edit (fusion re-binds `var` to
//! `Var::with_overrides` at unit-test compile,
//! `unit_test.rs:603-609` — the override always wins) and are reported
//! as a precision subtraction on the row.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::manifest::Manifest;
use crate::domain::project_def::{ProjectChange, ProjectChangeCategory, ProjectDefinition};

// ---------------------------------------------------------------------
// Payload PODs (additive, ADR-5)
// ---------------------------------------------------------------------

/// The evidence tier of one var reference — the honesty label on every
/// attribution claim. Declaration order is confidence order (strongest
/// first); the UNKNOWN residual is deliberately NOT a variant — it is
/// the in-row copy about what no tier can claim, not a per-model fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarTier {
    /// `var('x')` in a Jinja-evaluated region of the model's own
    /// `raw_code`.
    Direct,
    /// `var('x')` in a string value of the node's `unrendered_config`
    /// (covers project-file, schema-YAML, and inline `config()` sources
    /// plus hooks).
    Config,
    /// `var('x')` in the `macro_sql` of the model's `depends_on.macros`
    /// transitive closure.
    Macro,
}

/// One var-reference chip on an in-scope model (cute-dbt#268): the model
/// references an edited var at `tier`. Rendered by the report JS beside
/// the cute-dbt#267 config-attribution chips — context, never scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VarReference {
    /// The edited var's bare name.
    pub name: String,
    /// The evidence tier of the reference.
    pub tier: VarTier,
    /// The mediating macro's full id — `Some` exactly on
    /// [`VarTier::Macro`] references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// One MACRO-tier hit: `model` reads the var through `via` (the first
/// macro in its dependency closure whose body carries the literal call).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroVarHit {
    /// The reading model's full node id.
    pub model: String,
    /// The mediating macro's full id (`macro.{package}.{name}`).
    pub via: String,
}

/// What the static scan actually covered — the "state what WAS checked"
/// half of the honest-UNKNOWN copy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarScanFootprint {
    /// Model nodes whose `raw_code` + `unrendered_config` were scanned.
    pub models_scanned: usize,
    /// Distinct macro bodies scanned across all dependency closures.
    pub macros_scanned: usize,
    /// Models whose source is Python (`.py`) — vars there flow through
    /// the Python `dbt` object, not `var(` Jinja syntax, so the SQL scan
    /// cannot see them (their configs are still scanned).
    pub python_models: usize,
}

/// The tiered attribution of ONE effective var edit — the panel row's
/// per-var facts. All model/test lists are sorted full ids; "at least"
/// framing belongs to the render copy, the lists are the evidence.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VarAttribution {
    /// The edited var's bare name.
    pub name: String,
    /// `Some(pkg)` when the edit is the package-scoped entry
    /// `vars.{pkg}.{name}`; `None` for a global (root-namespace) entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The entry's old value (`None` ⇒ added).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Value>,
    /// The entry's new value (`None` ⇒ removed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Value>,
    /// DIRECT-tier models (node ids, sorted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub direct: Vec<String>,
    /// CONFIG-tier models (node ids, sorted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<String>,
    /// MACRO-tier hits (sorted by model id).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via_macros: Vec<MacroVarHit>,
    /// Models in reached namespaces whose scan saw a `var(` call with a
    /// non-literal name (and no literal hit on this var) — the degrade
    /// bucket: they cannot be ruled out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamic: Vec<String>,
    /// Packages whose own package-scoped pin keeps this GLOBAL edit's
    /// resolved value unchanged for their models (fusion precedence:
    /// package-scoped > global). Always empty on package-scoped edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masked_packages: Vec<String>,
    /// Unit tests pinning this var in `overrides.vars` (sorted ids) —
    /// insulated from the project edit (the override always wins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub insulated_tests: Vec<String>,
}

/// Vars-row enrichment attached to a `Vars` [`ProjectChange`]
/// (cute-dbt#268). One panel row maps to one top-level `vars:` key; a
/// package-scope key expands into one entry per changed inner var, a
/// global key carries exactly one entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VarChangeFacts {
    /// The effective var edits under this row's key, precedence-aware.
    pub entries: Vec<VarAttribution>,
    /// What the scan covered (shared across the report's rows).
    pub footprint: VarScanFootprint,
}

/// The full analysis output (transient — consumed by the run loop):
/// per-row facts plus the per-model reference chips.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VarAnalysis {
    /// Row facts keyed by the top-level `vars:` key (the `Vars`
    /// [`ProjectChange::label`] join key).
    pub facts_by_label: BTreeMap<String, VarChangeFacts>,
    /// node id → the edited vars it references (sorted, deduped) — the
    /// in-scope model chips.
    pub references: BTreeMap<String, Vec<VarReference>>,
}

// ---------------------------------------------------------------------
// Precedence (fusion's, mirrored exactly)
// ---------------------------------------------------------------------

/// fusion's var-resolution precedence (`ConfiguredVar::call_as_function`,
/// `configured_var.rs:55-129` @ `9977b6cb…`), highest first — the
/// declaration order IS the priority order (`Ord` derives from it, and
/// the property tests pin the total order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VarPrecedence {
    /// CLI `--vars` — always wins; unobservable from a manifest + a
    /// project file, so cute-dbt's copy carries the caveat instead of a
    /// guess. Never returned by [`resolve_project_var`].
    CliVars,
    /// `vars.{package}.{name}` — the package-scoped project entry.
    PackageScoped,
    /// `vars.{name}` — the root/global project entry (extends into
    /// every package's namespace, `load_vars.rs:28-42`).
    Global,
    /// The call-site `var('x', default)` — LOWEST: a default never
    /// overrides a project value. Never returned by
    /// [`resolve_project_var`] (the default lives at the call site, not
    /// in the project file).
    InlineDefault,
}

/// Resolve var `name` for a model in `package` against a parsed project
/// definition, mirroring the observable half of fusion's precedence:
/// the package-scoped entry wins over the global entry; CLI `--vars`
/// (above both) and the inline default (below both) live outside the
/// file and are represented by `None` plus the render-layer caveats.
///
/// `packages` is the manifest-known package-name set: a top-level
/// `vars:` key in it is a **scope map**, not a var — so it never
/// resolves as a global var of that name, and its inner entries resolve
/// only for that package's models.
#[must_use]
pub fn resolve_project_var<'a>(
    def: &'a ProjectDefinition,
    packages: &BTreeSet<String>,
    package: &str,
    name: &str,
) -> Option<(&'a Value, VarPrecedence)> {
    if packages.contains(package)
        && let Some(Value::Object(scoped)) = def.vars.get(package)
        && let Some(value) = scoped.get(name)
    {
        return Some((value, VarPrecedence::PackageScoped));
    }
    if !packages.contains(name)
        && let Some(value) = def.vars.get(name)
    {
        return Some((value, VarPrecedence::Global));
    }
    None
}

/// The value-only view of [`resolve_project_var`] — what a model in
/// `package` actually reads (source irrelevant for change detection).
fn resolved_value<'a>(
    def: &'a ProjectDefinition,
    packages: &BTreeSet<String>,
    package: &str,
    name: &str,
) -> Option<&'a Value> {
    resolve_project_var(def, packages, package, name).map(|(value, _)| value)
}

// ---------------------------------------------------------------------
// Changed-var extraction
// ---------------------------------------------------------------------

/// One effective var edit between two project definitions.
#[derive(Debug, Clone, PartialEq)]
pub struct VarEdit {
    /// The var's bare name.
    pub name: String,
    /// `Some(pkg)` for a package-scoped entry; `None` for global.
    pub package: Option<String>,
    /// Old value (`None` ⇒ added).
    pub old: Option<Value>,
    /// New value (`None` ⇒ removed).
    pub new: Option<Value>,
}

impl VarEdit {
    /// The top-level `vars:` key this edit surfaced under — the panel
    /// row's label (the package name for scoped entries, else the var
    /// name itself).
    #[must_use]
    pub fn label(&self) -> &str {
        self.package.as_deref().unwrap_or(&self.name)
    }
}

/// Extract the effective var edits between two parsed definitions,
/// precedence-aware: a top-level key that names a manifest-known
/// package (and carries a mapping) is fusion's package scope
/// (`load_vars.rs:28-42`) and expands per inner var; every other key is
/// one global var. Union semantics per level — a key present on either
/// side is compared; equal values emit nothing.
///
/// Tolerant ingestion: a package-named key whose value is NOT a mapping
/// on either side cannot be a scope there — the non-mapping side simply
/// contributes no inner entries (and when neither side is a mapping the
/// key degrades to a single opaque global-style edit, so the change is
/// never silently dropped).
#[must_use]
pub fn changed_vars(
    old: &ProjectDefinition,
    new: &ProjectDefinition,
    packages: &BTreeSet<String>,
) -> Vec<VarEdit> {
    let mut out = Vec::new();
    for key in union_keys(&old.vars, &new.vars) {
        let (o, n) = (old.vars.get(&key), new.vars.get(&key));
        if o == n {
            continue;
        }
        let is_scope = packages.contains(&key)
            && (matches!(o, Some(Value::Object(_))) || matches!(n, Some(Value::Object(_))));
        if is_scope {
            push_scope_edits(&mut out, &key, o, n);
        } else {
            out.push(VarEdit {
                name: key,
                package: None,
                old: o.cloned(),
                new: n.cloned(),
            });
        }
    }
    out
}

/// Expand one package-scope key's differing inner vars into edits.
fn push_scope_edits(
    out: &mut Vec<VarEdit>,
    package: &str,
    old: Option<&Value>,
    new: Option<&Value>,
) {
    let empty = serde_json::Map::new();
    let as_map = |side: Option<&Value>| match side {
        Some(Value::Object(map)) => map.clone(),
        _ => empty.clone(),
    };
    let (o_map, n_map) = (as_map(old), as_map(new));
    let mut names: Vec<&String> = o_map.keys().chain(n_map.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let (o, n) = (o_map.get(name), n_map.get(name));
        if o != n {
            out.push(VarEdit {
                name: name.clone(),
                package: Some(package.to_owned()),
                old: o.cloned(),
                new: n.cloned(),
            });
        }
    }
}

/// The sorted union of two `BTreeMap`s' keys.
fn union_keys<V>(a: &BTreeMap<String, V>, b: &BTreeMap<String, V>) -> Vec<String> {
    let mut keys: Vec<String> = a.keys().chain(b.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    keys
}

// ---------------------------------------------------------------------
// The var() call scanner (fusion's call grammar, Jinja-region guarded)
// ---------------------------------------------------------------------

/// What one text scan found: the literal var names called, plus whether
/// any call computes its name (`var(expr)` — detectable as a read,
/// name not statically extractable).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarRefs {
    /// Literal first-argument names, deduped.
    pub names: BTreeSet<String>,
    /// `true` when at least one `var(` call has a non-literal name.
    pub dynamic: bool,
}

impl VarRefs {
    /// Fold another scan into this one.
    fn merge(&mut self, other: &VarRefs) {
        self.names.extend(other.names.iter().cloned());
        self.dynamic |= other.dynamic;
    }
}

/// Scan the Jinja-evaluated regions of `text` for var calls — the sound
/// under-approximation: only `{{ … }}` expressions and `{% … %}`
/// statements count; `{# … #}` comments and `{% raw %} … {% endraw %}`
/// blocks are skipped (calls there never evaluate), and plain SQL text
/// outside any region never matches. A SQL `--` comment INSIDE a Jinja
/// region still counts — Jinja evaluates it (a true positive, per the
/// research evidence).
#[must_use]
pub fn scan_jinja_text(text: &str) -> VarRefs {
    let mut refs = VarRefs::default();
    for region in jinja_regions(text) {
        scan_expression(region, &mut refs);
    }
    refs
}

/// Scan a bare Jinja **expression** string (no `{{ }}` frame) — the
/// shape fusion's inline-`config()` static parse preserves into
/// `unrendered_config` (`utils.rs:713-721` @ `9977b6cb…`: non-constant
/// kwargs keep their raw expression source text).
fn scan_expression(text: &str, refs: &mut VarRefs) {
    scan_calls(text, "var", refs);
    scan_calls(text, "var.has_var", refs);
}

/// Find `{token}(` call sites in `text` (word-boundary guarded — never
/// `myvar(` / `varchar(`, and a bare `var` reading also rejects `.var(`
/// method calls on other objects) and classify each first argument:
/// quoted literal → a name; anything else → dynamic.
fn scan_calls(text: &str, token: &str, refs: &mut VarRefs) {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(found) = text[from..].find(token) {
        let start = from + found;
        from = start + token.len();
        if !boundary_before(bytes, start) {
            continue;
        }
        let mut i = start + token.len();
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) != Some(&b'(') {
            continue;
        }
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match bytes.get(i) {
            Some(&quote @ (b'\'' | b'"')) => {
                let name_start = i + 1;
                match text[name_start..].find(quote as char) {
                    Some(len) => {
                        refs.names
                            .insert(text[name_start..name_start + len].to_owned());
                    }
                    None => refs.dynamic = true, // unterminated literal
                }
            }
            _ => refs.dynamic = true,
        }
    }
}

/// Whether the char before `start` permits a call-token match: start of
/// text, or a non-word, non-`.` byte (`.` rejects method calls on other
/// objects — `graph.var(` is not the project var global).
fn boundary_before(bytes: &[u8], start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let prev = bytes[start - 1];
    !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.')
}

/// Extract the Jinja-evaluated regions of `text`: `{{ … }}` expression
/// bodies and `{% … %}` statement bodies, skipping `{# … #}` comments
/// and `{% raw %} … {% endraw %}` blocks. Expression ends are
/// string-and-brace aware so a map default (`var('x', {'k': 1})`)
/// doesn't truncate the region at the inner `}}`. An unterminated
/// construct conservatively yields the remainder (under-approximation
/// preserving: a real call near the open still counts).
fn jinja_regions(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        match &bytes[i..=i + 1] {
            b"{#" => i = find_past(text, i + 2, "#}"),
            b"{{" => {
                let end = expression_end(text, i + 2);
                out.push(&text[i + 2..end]);
                i = (end + 2).min(text.len());
            }
            b"{%" => {
                let end = text[i + 2..].find("%}").map_or(text.len(), |o| i + 2 + o);
                let body = &text[i + 2..end];
                if statement_keyword(body) == "raw" {
                    i = skip_raw_block(text, end);
                } else {
                    out.push(body);
                    i = (end + 2).min(text.len());
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Index just past the next occurrence of `close` from `from` (or the
/// text end when unterminated).
fn find_past(text: &str, from: usize, close: &str) -> usize {
    text[from..]
        .find(close)
        .map_or(text.len(), |o| from + o + close.len())
}

/// The first word of a `{% … %}` body, `-` whitespace-control trim
/// included — `raw` / `endraw` / `if` / ….
fn statement_keyword(body: &str) -> &str {
    body.trim_start_matches('-')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('-')
}

/// Skip from just past a `{% raw %}` tag's body end to just past the
/// matching `{% endraw %}` tag (raw content is never evaluated).
fn skip_raw_block(text: &str, mut from: usize) -> usize {
    from = (from + 2).min(text.len());
    while let Some(open) = text[from..].find("{%") {
        let tag_start = from + open + 2;
        let tag_end = text[tag_start..]
            .find("%}")
            .map_or(text.len(), |o| tag_start + o);
        if statement_keyword(&text[tag_start..tag_end]) == "endraw" {
            return (tag_end + 2).min(text.len());
        }
        from = (tag_end + 2).min(text.len());
    }
    text.len()
}

/// The end index of a `{{ … }}` expression body starting at `from`:
/// the first `}}` at brace depth 0 outside string literals (Jinja's
/// lexer balances braces the same way, so a dict default never ends
/// the expression early).
fn expression_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = from;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'{' => depth += 1,
                b'}' => {
                    if depth == 0 && bytes.get(i + 1) == Some(&b'}') {
                        return i;
                    }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            },
        }
        i += 1;
    }
    text.len()
}

/// Recursively scan every string value of an `unrendered_config`-style
/// JSON tree. A string carrying Jinja delimiters scans region-guarded;
/// a bare string scans as raw expression text (the inline-`config()`
/// preservation shape). Keys are config vocabulary, never Jinja — only
/// values scan.
fn scan_config_value(value: &Value, refs: &mut VarRefs) {
    match value {
        Value::String(s) => {
            if s.contains("{{") || s.contains("{%") {
                refs.merge(&scan_jinja_text(s));
            } else {
                scan_expression(s, refs);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| scan_config_value(v, refs)),
        Value::Object(map) => map.values().for_each(|v| scan_config_value(v, refs)),
        _ => {}
    }
}

// ---------------------------------------------------------------------
// Per-model scans + the macro closure
// ---------------------------------------------------------------------

/// One model's reference scan across the three checkable surfaces
/// (transient).
#[derive(Debug, Default)]
struct ModelScan {
    direct: VarRefs,
    config: VarRefs,
    /// var name → the first closure macro whose body carries the call.
    macro_hits: BTreeMap<String, String>,
    macro_dynamic: bool,
    python: bool,
}

impl ModelScan {
    /// Whether any surface saw a non-literal `var(` call AND none of
    /// them carries a literal hit on `name` — the cannot-rule-out
    /// bucket for that var.
    fn dynamic_for(&self, name: &str) -> bool {
        (self.direct.dynamic || self.config.dynamic || self.macro_dynamic) && !self.references(name)
    }

    /// Whether any tier carries a literal hit on `name`.
    fn references(&self, name: &str) -> bool {
        self.direct.names.contains(name)
            || self.config.names.contains(name)
            || self.macro_hits.contains_key(name)
    }
}

/// Walk one model's `depends_on.macros` transitive closure
/// (breadth-first, wire order), scanning each macro body once via the
/// shared memo, and record per var name the first macro that carries
/// the literal call.
fn scan_macro_closure(
    manifest: &Manifest,
    roots: &[String],
    memo: &mut HashMap<String, VarRefs>,
    scan: &mut ModelScan,
) {
    let mut queue: Vec<&str> = roots.iter().map(String::as_str).collect();
    let mut seen: BTreeSet<&str> = queue.iter().copied().collect();
    let mut at = 0;
    while at < queue.len() {
        let macro_id = queue[at];
        at += 1;
        if !memo.contains_key(macro_id) {
            let refs = manifest
                .macros()
                .get(macro_id)
                .map(|sql| scan_jinja_text(sql))
                .unwrap_or_default();
            memo.insert(macro_id.to_owned(), refs);
        }
        let refs = &memo[macro_id];
        for name in &refs.names {
            scan.macro_hits
                .entry(name.clone())
                .or_insert_with(|| macro_id.to_owned());
        }
        scan.macro_dynamic |= refs.dynamic;
        for next in manifest.macro_refs(macro_id) {
            if seen.insert(next) {
                queue.push(next);
            }
        }
    }
}

/// Scan every model node once (all tiers), returning the per-model
/// scans plus the shared footprint.
fn scan_models(manifest: &Manifest) -> (BTreeMap<String, ModelScan>, VarScanFootprint) {
    let mut scans = BTreeMap::new();
    let mut memo: HashMap<String, VarRefs> = HashMap::new();
    let mut footprint = VarScanFootprint::default();
    for (id, node) in manifest.nodes() {
        if node.resource_type() != "model" {
            continue;
        }
        let mut scan = ModelScan {
            python: node
                .original_file_path()
                .is_some_and(|p| p.ends_with(".py")),
            ..ModelScan::default()
        };
        if !scan.python
            && let Some(raw) = node.raw_code()
        {
            scan.direct = scan_jinja_text(raw);
        }
        for value in node.unrendered_config().values() {
            scan_config_value(value, &mut scan.config);
        }
        scan_macro_closure(manifest, node.depends_on().macros(), &mut memo, &mut scan);
        footprint.models_scanned += 1;
        footprint.python_models += usize::from(scan.python);
        scans.insert(id.as_str().to_owned(), scan);
    }
    footprint.macros_scanned = memo.len();
    (scans, footprint)
}

// ---------------------------------------------------------------------
// Attribution
// ---------------------------------------------------------------------

/// The manifest-known package names: every node's `package_name` plus
/// the root project's `metadata.project_name`.
fn package_names(manifest: &Manifest) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = manifest
        .nodes()
        .values()
        .filter_map(|n| n.package_name().map(str::to_owned))
        .collect();
    if let Some(root) = manifest.metadata().project_name() {
        out.insert(root.to_owned());
    }
    out
}

/// Whether `edit` reaches a model in `model_pkg`: a package-scoped edit
/// reaches exactly its package; a global edit reaches the packages
/// whose resolution changed (`changed_pkgs` — masking already applied),
/// and a model with NO package information (pre-#256 wire) is included
/// for global edits (no pin can be proven for it — under-claiming there
/// would be a guess, not evidence).
fn edit_reaches(edit: &VarEdit, model_pkg: Option<&str>, changed_pkgs: &BTreeSet<String>) -> bool {
    match (&edit.package, model_pkg) {
        (Some(p), Some(mp)) => p == mp,
        (Some(_), None) => false,
        (None, Some(mp)) => changed_pkgs.contains(mp),
        (None, None) => true,
    }
}

/// The packages a global edit actually changes resolution for, plus the
/// packages it is masked in (a pin on both sides keeps the resolved
/// value identical). Package-scoped edits change exactly their package.
fn changed_and_masked(
    edit: &VarEdit,
    old: &ProjectDefinition,
    new: &ProjectDefinition,
    packages: &BTreeSet<String>,
) -> (BTreeSet<String>, Vec<String>) {
    if let Some(p) = &edit.package {
        return (BTreeSet::from([p.clone()]), Vec::new());
    }
    let mut changed = BTreeSet::new();
    let mut masked = Vec::new();
    for pkg in packages {
        if resolved_value(old, packages, pkg, &edit.name)
            == resolved_value(new, packages, pkg, &edit.name)
        {
            masked.push(pkg.clone());
        } else {
            changed.insert(pkg.clone());
        }
    }
    (changed, masked)
}

/// Unit tests pinning `name` in `overrides.vars` (sorted ids) — the
/// insulated set (fusion re-binds `var` with the override at unit-test
/// compile, so the project edit never reaches them).
fn insulated_tests(manifest: &Manifest, name: &str) -> Vec<String> {
    let mut out: Vec<String> = manifest
        .unit_tests()
        .iter()
        .filter(|(_, ut)| {
            ut.overrides()
                .and_then(|o| o.get("vars"))
                .is_some_and(|vars| vars.contains_key(name))
        })
        .map(|(id, _)| id.clone())
        .collect();
    out.sort();
    out
}

/// Build one edit's [`VarAttribution`] over the per-model scans.
fn attribute_edit(
    manifest: &Manifest,
    edit: &VarEdit,
    scans: &BTreeMap<String, ModelScan>,
    old: &ProjectDefinition,
    new: &ProjectDefinition,
    packages: &BTreeSet<String>,
) -> VarAttribution {
    let (changed_pkgs, masked_packages) = changed_and_masked(edit, old, new, packages);
    let root = manifest.metadata().project_name();
    let mut attribution = VarAttribution {
        name: edit.name.clone(),
        package: edit.package.clone(),
        old: edit.old.clone(),
        new: edit.new.clone(),
        masked_packages,
        insulated_tests: insulated_tests(manifest, &edit.name),
        ..VarAttribution::default()
    };
    for (id, scan) in scans {
        let model_pkg = manifest
            .nodes()
            .get(&crate::domain::manifest::NodeId::new(id.clone()))
            .and_then(|n| n.package_name())
            .or(root);
        if !edit_reaches(edit, model_pkg, &changed_pkgs) {
            continue;
        }
        if scan.direct.names.contains(&edit.name) {
            attribution.direct.push(id.clone());
        }
        if scan.config.names.contains(&edit.name) {
            attribution.config.push(id.clone());
        }
        if let Some(via) = scan.macro_hits.get(&edit.name) {
            attribution.via_macros.push(MacroVarHit {
                model: id.clone(),
                via: via.clone(),
            });
        }
        if scan.dynamic_for(&edit.name) {
            attribution.dynamic.push(id.clone());
        }
    }
    attribution
}

/// The per-model chips for one attributed edit, appended into the
/// shared reference map.
fn collect_references(
    attribution: &VarAttribution,
    references: &mut BTreeMap<String, Vec<VarReference>>,
) {
    let mut push = |model: &str, tier: VarTier, via: Option<&str>| {
        references
            .entry(model.to_owned())
            .or_default()
            .push(VarReference {
                name: attribution.name.clone(),
                tier,
                via: via.map(str::to_owned),
            });
    };
    for model in &attribution.direct {
        push(model, VarTier::Direct, None);
    }
    for model in &attribution.config {
        push(model, VarTier::Config, None);
    }
    for hit in &attribution.via_macros {
        push(&hit.model, VarTier::Macro, Some(&hit.via));
    }
}

/// Attribute every effective var edit between two parsed project
/// definitions against the current manifest (cute-dbt#268 — the C3
/// slice of epic #262).
///
/// Pure computation over data already in hand (zero-compute: no Jinja
/// rendering, no filesystem, no engine). Returns the per-row facts
/// (keyed by the panel row's label) plus the per-model reference chips.
/// **Never widens scope** — the founder's contextualize-don't-widen
/// frame; callers thread the result into the panel rows and the render
/// payload only.
#[must_use]
pub fn attribute_var_changes(
    current: &Manifest,
    old: &ProjectDefinition,
    new: &ProjectDefinition,
) -> VarAnalysis {
    let packages = package_names(current);
    let edits = changed_vars(old, new, &packages);
    if edits.is_empty() {
        return VarAnalysis::default();
    }
    let (scans, footprint) = scan_models(current);
    let mut analysis = VarAnalysis::default();
    for edit in &edits {
        let attribution = attribute_edit(current, edit, &scans, old, new, &packages);
        collect_references(&attribution, &mut analysis.references);
        analysis
            .facts_by_label
            .entry(edit.label().to_owned())
            .or_insert_with(|| VarChangeFacts {
                entries: Vec::new(),
                footprint,
            })
            .entries
            .push(attribution);
    }
    for refs in analysis.references.values_mut() {
        refs.sort();
        refs.dedup();
    }
    analysis
}

/// Attach [`VarChangeFacts`] to every `Vars` row of a categorized
/// change list (cute-dbt#268) — the cute-dbt#269 `attach_hook_facts`
/// pattern. Rows whose label the analysis carries no facts for keep
/// `None` (defensive: the structural diff and `changed_vars` share
/// union semantics, so in practice every Vars row gets facts).
pub fn attach_var_facts(changes: &mut [ProjectChange], analysis: &VarAnalysis) {
    for change in changes
        .iter_mut()
        .filter(|c| c.category == ProjectChangeCategory::Vars)
    {
        if let Some(facts) = analysis.facts_by_label.get(&change.label) {
            change.vars = Some(facts.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, Node, NodeConfig, NodeId,
    };
    use crate::domain::project_def::diff_project_definitions;
    use crate::domain::unit_test::{UnitTest, UnitTestExpect, UnitTestGiven, UnitTestOverrides};

    // ----- builders -----

    /// A model node with `raw_code`, identity, and optional source path.
    fn model(
        id: &str,
        raw_code: Option<&str>,
        package: Option<&str>,
        path: Option<&str>,
        macros: &[&str],
    ) -> (NodeId, Node) {
        let node_id = NodeId::new(id);
        let node = Node::new(
            node_id.clone(),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            raw_code.map(str::to_owned),
            DependsOn::new(macros.iter().map(|m| (*m).to_owned()).collect(), Vec::new()),
            path.map(str::to_owned),
            NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        )
        .with_identity(None, package.map(str::to_owned));
        (node_id, node)
    }

    fn manifest_of(nodes: Vec<(NodeId, Node)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12").with_project_name(Some("shop".to_owned())),
            nodes.into_iter().collect::<HashMap<_, _>>(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    /// A definition whose `vars:` block holds exactly `entries`.
    fn def_with_vars(entries: &[(&str, Value)]) -> ProjectDefinition {
        ProjectDefinition {
            vars: entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            ..ProjectDefinition::default()
        }
    }

    fn packages(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    // ----- POD serde round-trips (ADR-5) -----

    #[test]
    fn var_reference_round_trips_and_tier_serializes_snake_case() {
        let reference = VarReference {
            name: "dq_threshold".to_owned(),
            tier: VarTier::Macro,
            via: Some("macro.shop.add_dq_flags".to_owned()),
        };
        let json = serde_json::to_string(&reference).expect("serialize");
        assert!(json.contains(r#""tier":"macro""#), "{json}");
        let back: VarReference = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(reference, back);
        assert_eq!(
            serde_json::to_string(&VarTier::Direct).unwrap(),
            r#""direct""#,
        );
        assert_eq!(
            serde_json::to_string(&VarTier::Config).unwrap(),
            r#""config""#,
        );
    }

    #[test]
    fn var_change_facts_round_trip_and_empty_lists_are_omitted() {
        let facts = VarChangeFacts {
            entries: vec![VarAttribution {
                name: "x".to_owned(),
                package: Some("pkg".to_owned()),
                old: Some(json!(1)),
                new: Some(json!(2)),
                direct: vec!["model.shop.a".to_owned()],
                config: vec!["model.shop.b".to_owned()],
                via_macros: vec![MacroVarHit {
                    model: "model.shop.c".to_owned(),
                    via: "macro.shop.m".to_owned(),
                }],
                dynamic: vec!["model.shop.d".to_owned()],
                masked_packages: vec!["dbt_utils".to_owned()],
                insulated_tests: vec!["unit_test.shop.a.t".to_owned()],
            }],
            footprint: VarScanFootprint {
                models_scanned: 4,
                macros_scanned: 2,
                python_models: 1,
            },
        };
        let json = serde_json::to_string(&facts).expect("serialize");
        let back: VarChangeFacts = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(facts, back);
        // A bare attribution omits every empty list (payload hygiene).
        let bare = serde_json::to_string(&VarAttribution {
            name: "x".to_owned(),
            ..VarAttribution::default()
        })
        .expect("serialize");
        assert_eq!(bare, r#"{"name":"x"}"#);
    }

    // ----- precedence (property tests, house exhaustive style) -----

    #[test]
    fn precedence_total_order_matches_fusion_declaration() {
        // CLI --vars > package-scoped > global > inline default
        // (configured_var.rs:55-129) — Ord derives from declaration
        // order, so the pinned order IS the contract.
        let order = [
            VarPrecedence::CliVars,
            VarPrecedence::PackageScoped,
            VarPrecedence::Global,
            VarPrecedence::InlineDefault,
        ];
        for (i, a) in order.iter().enumerate() {
            for b in &order[i + 1..] {
                assert!(a < b, "{a:?} outranks {b:?}");
            }
        }
    }

    #[test]
    fn resolution_always_picks_the_highest_priority_present_source() {
        // Exhaustive over the observable presence space: package pin
        // present? × global present? → the winner is always the
        // highest-priority present source, for EVERY combination.
        for pin_present in [false, true] {
            for global_present in [false, true] {
                let mut entries: Vec<(&str, Value)> = Vec::new();
                if global_present {
                    entries.push(("x", json!("global")));
                }
                if pin_present {
                    entries.push(("dbt_utils", json!({ "x": "pinned" })));
                }
                let def = def_with_vars(&entries);
                let got =
                    resolve_project_var(&def, &packages(&["shop", "dbt_utils"]), "dbt_utils", "x");
                let expected = match (pin_present, global_present) {
                    (true, _) => Some(("pinned", VarPrecedence::PackageScoped)),
                    (false, true) => Some(("global", VarPrecedence::Global)),
                    (false, false) => None,
                };
                assert_eq!(
                    got.map(|(v, p)| (v.as_str().unwrap(), p)),
                    expected,
                    "pin={pin_present} global={global_present}",
                );
                // A model in ANOTHER package never sees the dbt_utils pin.
                let other =
                    resolve_project_var(&def, &packages(&["shop", "dbt_utils"]), "shop", "x");
                let expected_other = global_present.then_some(("global", VarPrecedence::Global));
                assert_eq!(other.map(|(v, p)| (v.as_str().unwrap(), p)), expected_other);
            }
        }
    }

    #[test]
    fn a_package_named_vars_key_is_a_scope_map_never_a_global_var() {
        // fusion's load_vars treats a package-named key as the package
        // scope: resolving the KEY ITSELF as a var name finds nothing.
        let def = def_with_vars(&[("dbt_utils", json!({ "x": 1 }))]);
        assert_eq!(
            resolve_project_var(&def, &packages(&["dbt_utils"]), "shop", "dbt_utils"),
            None,
        );
        // …but the same-shaped key NOT naming a package IS a (dict) var.
        let got = resolve_project_var(&def, &packages(&["shop"]), "shop", "dbt_utils");
        assert_eq!(got.map(|(_, p)| p), Some(VarPrecedence::Global));
    }

    // ----- changed_vars -----

    #[test]
    fn changed_vars_is_reflexively_empty() {
        let def = def_with_vars(&[("x", json!(1)), ("dbt_utils", json!({ "y": 2 }))]);
        assert!(changed_vars(&def, &def, &packages(&["dbt_utils"])).is_empty());
    }

    #[test]
    fn changed_vars_reports_global_add_change_and_remove() {
        let old = def_with_vars(&[("changed", json!(1)), ("removed", json!(true))]);
        let new = def_with_vars(&[("changed", json!(2)), ("added", json!("v"))]);
        let edits = changed_vars(&old, &new, &packages(&[]));
        assert_eq!(
            edits,
            vec![
                VarEdit {
                    name: "added".to_owned(),
                    package: None,
                    old: None,
                    new: Some(json!("v")),
                },
                VarEdit {
                    name: "changed".to_owned(),
                    package: None,
                    old: Some(json!(1)),
                    new: Some(json!(2)),
                },
                VarEdit {
                    name: "removed".to_owned(),
                    package: None,
                    old: Some(json!(true)),
                    new: None,
                },
            ],
        );
    }

    #[test]
    fn changed_vars_expands_a_package_scope_per_inner_var() {
        let old = def_with_vars(&[("dbt_utils", json!({ "a": 1, "same": 0 }))]);
        let new = def_with_vars(&[("dbt_utils", json!({ "a": 2, "same": 0, "b": 3 }))]);
        let edits = changed_vars(&old, &new, &packages(&["dbt_utils"]));
        assert_eq!(
            edits,
            vec![
                VarEdit {
                    name: "a".to_owned(),
                    package: Some("dbt_utils".to_owned()),
                    old: Some(json!(1)),
                    new: Some(json!(2)),
                },
                VarEdit {
                    name: "b".to_owned(),
                    package: Some("dbt_utils".to_owned()),
                    old: None,
                    new: Some(json!(3)),
                },
            ],
        );
        assert_eq!(edits[0].label(), "dbt_utils");
    }

    #[test]
    fn changed_vars_treats_a_non_package_dict_var_as_one_global_edit() {
        // A mapping-valued var whose key names NO package is a dict
        // var, not a scope — one opaque edit, never expanded.
        let old = def_with_vars(&[("grid", json!({ "density": "weekly" }))]);
        let new = def_with_vars(&[("grid", json!({ "density": "monthly" }))]);
        let edits = changed_vars(&old, &new, &packages(&["shop"]));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].name, "grid");
        assert_eq!(edits[0].package, None);
    }

    #[test]
    fn changed_vars_degrades_a_non_mapping_package_key_to_an_opaque_edit() {
        // Tolerant ingestion: a package-named key that is a scalar on
        // both sides cannot be a scope — the change still reports.
        let old = def_with_vars(&[("dbt_utils", json!(1))]);
        let new = def_with_vars(&[("dbt_utils", json!(2))]);
        let edits = changed_vars(&old, &new, &packages(&["dbt_utils"]));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].package, None, "no scope to expand");
    }

    #[test]
    fn changed_vars_expands_when_only_one_side_is_a_mapping() {
        // Scope created in this PR: every inner var reports as added.
        let old = def_with_vars(&[]);
        let new = def_with_vars(&[("dbt_utils", json!({ "x": 1 }))]);
        let edits = changed_vars(&old, &new, &packages(&["dbt_utils"]));
        assert_eq!(
            edits,
            vec![VarEdit {
                name: "x".to_owned(),
                package: Some("dbt_utils".to_owned()),
                old: None,
                new: Some(json!(1)),
            }],
        );
    }

    // ----- the var() call scanner -----

    /// Names found scanning `text` as Jinja-bearing model SQL.
    fn names_of(text: &str) -> Vec<String> {
        scan_jinja_text(text).names.into_iter().collect()
    }

    #[test]
    fn scanner_accepts_every_fusion_call_form() {
        // VarFunction::parse_args (var.rs:25-92) + has_var (111-126):
        // the exhaustive literal-name grammar.
        let forms = [
            "{{ var('x') }}",
            "{{ var(\"x\") }}",
            "{{ var ('x') }}",
            "{{ var('x', 'fallback') }}",
            "{{ var('x', default=None) }}",
            "{{ var('x', {'k': {'nested': 1}}) }}",
            "{{ var.has_var('x') }}",
            "{% if var('x') %}select 1{% endif %}",
            "{% set v = var('x') %}",
        ];
        for form in forms {
            assert_eq!(names_of(form), vec!["x".to_owned()], "form: {form}");
            assert!(!scan_jinja_text(form).dynamic, "literal form: {form}");
        }
    }

    #[test]
    fn scanner_inline_default_never_hides_the_read() {
        // Precedence fact encoded: a call WITH a default still reads the
        // project value (the default is LOWEST) — the scan must report it.
        let refs = scan_jinja_text("{{ var('x', 'safe') }}");
        assert!(refs.names.contains("x"));
    }

    #[test]
    fn scanner_marks_dynamic_names_without_extracting_them() {
        let refs = scan_jinja_text("{{ var(my_name) }}");
        assert!(refs.names.is_empty());
        assert!(refs.dynamic);
        // Mixed: a literal sibling still extracts.
        let mixed = scan_jinja_text("{{ var('lit') }} {{ var(computed) }}");
        assert!(mixed.names.contains("lit"));
        assert!(mixed.dynamic);
    }

    #[test]
    fn scanner_skips_comments_raw_blocks_and_plain_sql() {
        assert!(names_of("{# var('commented') #}").is_empty());
        assert!(names_of("{% raw %}{{ var('rawed') }}{% endraw %}").is_empty());
        assert!(names_of("{%- raw -%}{{ var('rawed') }}{%- endraw -%}").is_empty());
        assert!(
            names_of("select * from t where note = \"var('plain_sql')\"").is_empty(),
            "outside any Jinja region nothing evaluates",
        );
        // …but a region AFTER the raw block still scans.
        assert_eq!(
            names_of("{% raw %}{{ var('rawed') }}{% endraw %}{{ var('live') }}"),
            vec!["live".to_owned()],
        );
    }

    #[test]
    fn scanner_word_boundary_rejects_lookalikes() {
        assert!(names_of("{{ myvar('x') }}").is_empty());
        assert!(names_of("{{ varchar('x') }}").is_empty());
        assert!(
            names_of("{{ graph.var('x') }}").is_empty(),
            "a method on another object is not the var global",
        );
        assert!(!scan_jinja_text("{{ myvar('x') }}").dynamic);
    }

    #[test]
    fn scanner_counts_a_sql_comment_inside_a_jinja_region() {
        // Jinja does not know SQL comments — the call still evaluates
        // (a true positive per the research evidence).
        assert_eq!(
            names_of("{{\n  -- var('in_sql_comment')\n  1\n}}"),
            vec!["in_sql_comment".to_owned()],
        );
    }

    #[test]
    fn scanner_map_default_does_not_truncate_the_region() {
        // `}}` inside a dict default must not end the expression early —
        // a second call after the dict still scans.
        assert_eq!(
            names_of("{{ var('a', {'k': {'n': 1}}) }} {{ var('b') }}"),
            vec!["a".to_owned(), "b".to_owned()],
        );
    }

    #[test]
    fn scanner_unterminated_region_scans_conservatively() {
        assert_eq!(names_of("{{ var('open')"), vec!["open".to_owned()]);
        assert!(names_of("{% raw %}{{ var('never_closed') }}").is_empty());
    }

    #[test]
    fn config_scan_reads_braced_and_bare_expression_strings() {
        // Braced (project-file / schema-YAML shape) and bare expression
        // (fusion's inline-config preservation) both count; prose and
        // non-strings never do.
        let config: BTreeMap<String, Value> = BTreeMap::from([
            ("enabled".to_owned(), json!("{{ var('flag') }}")),
            ("alias".to_owned(), json!("var('alias_name')")),
            (
                "meta".to_owned(),
                json!({ "hooks": ["{{ var('hooked') }}"], "n": 7 }),
            ),
            ("note".to_owned(), json!("uses vars heavily")),
        ]);
        let mut refs = VarRefs::default();
        for value in config.values() {
            scan_config_value(value, &mut refs);
        }
        assert_eq!(
            refs.names.iter().collect::<Vec<_>>(),
            vec!["alias_name", "flag", "hooked"],
        );
        assert!(!refs.dynamic);
    }

    // ----- attribution -----

    /// The canonical scenario manifest: one direct reader, one
    /// config-driven node, one macro-mediated reader, one bystander.
    fn scenario_manifest() -> Manifest {
        let (direct_id, direct) = model(
            "model.shop.reads_direct",
            Some("select {{ var('dq_threshold') }} as cap"),
            Some("shop"),
            Some("models/reads_direct.sql"),
            &[],
        );
        let (config_id, config_node) = model(
            "model.shop.config_driven",
            Some("select 1"),
            Some("shop"),
            Some("models/config_driven.sql"),
            &[],
        );
        let config_node = config_node.with_unrendered_config(BTreeMap::from([(
            "enabled".to_owned(),
            json!("{{ var('dq_threshold') > 0 }}"),
        )]));
        let (macro_id, macro_reader) = model(
            "model.shop.via_macro",
            Some("select 2"),
            Some("shop"),
            Some("models/via_macro.sql"),
            &["macro.shop.outer"],
        );
        let (bystander_id, bystander) = model(
            "model.shop.bystander",
            Some("select 3"),
            Some("shop"),
            Some("models/bystander.sql"),
            &[],
        );
        let mut manifest = Manifest::new(
            ManifestMetadata::new("v12").with_project_name(Some("shop".to_owned())),
            vec![
                (direct_id, direct),
                (config_id, config_node.clone()),
                (macro_id, macro_reader),
                (bystander_id, bystander),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
            HashMap::new(),
            vec![
                ("macro.shop.outer".to_owned(), "{{ inner() }}".to_owned()),
                (
                    "macro.shop.inner".to_owned(),
                    "{% if var('dq_threshold') %}1{% endif %}".to_owned(),
                ),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        );
        manifest = manifest.with_macro_depends_on(BTreeMap::from([(
            "macro.shop.outer".to_owned(),
            vec!["macro.shop.inner".to_owned()],
        )]));
        manifest
    }

    #[test]
    fn attribution_tiers_direct_config_and_transitive_macro() {
        let current = scenario_manifest();
        let old = def_with_vars(&[("dq_threshold", json!(10))]);
        let new = def_with_vars(&[("dq_threshold", json!(5))]);
        let analysis = attribute_var_changes(&current, &old, &new);
        let facts = &analysis.facts_by_label["dq_threshold"];
        assert_eq!(facts.entries.len(), 1);
        let entry = &facts.entries[0];
        assert_eq!(entry.direct, vec!["model.shop.reads_direct"]);
        assert_eq!(entry.config, vec!["model.shop.config_driven"]);
        assert_eq!(
            entry.via_macros,
            vec![MacroVarHit {
                model: "model.shop.via_macro".to_owned(),
                via: "macro.shop.inner".to_owned(),
            }],
            "the via macro is the closure macro that CARRIES the call",
        );
        assert!(entry.dynamic.is_empty());
        assert_eq!(
            (entry.old.as_ref(), entry.new.as_ref()),
            (Some(&json!(10)), Some(&json!(5)))
        );
        assert_eq!(facts.footprint.models_scanned, 4);
        assert_eq!(facts.footprint.macros_scanned, 2);
        assert_eq!(facts.footprint.python_models, 0);
        // Chips: one per (model, tier); the bystander gets none.
        assert_eq!(
            analysis.references["model.shop.reads_direct"],
            vec![VarReference {
                name: "dq_threshold".to_owned(),
                tier: VarTier::Direct,
                via: None,
            }],
        );
        assert_eq!(
            analysis.references["model.shop.via_macro"][0]
                .via
                .as_deref(),
            Some("macro.shop.inner"),
        );
        assert!(!analysis.references.contains_key("model.shop.bystander"));
    }

    #[test]
    fn attribution_global_edit_is_masked_by_an_unchanged_package_pin() {
        // dbt_utils pins dq_threshold in BOTH versions: its model's
        // resolved value never changes — masked, not attributed.
        let (pinned_id, pinned) = model(
            "model.dbt_utils.helper",
            Some("select {{ var('dq_threshold') }}"),
            Some("dbt_utils"),
            None,
            &[],
        );
        let (own_id, own) = model(
            "model.shop.reader",
            Some("select {{ var('dq_threshold') }}"),
            Some("shop"),
            None,
            &[],
        );
        let current = manifest_of(vec![(pinned_id, pinned), (own_id, own)]);
        let pin = ("dbt_utils", json!({ "dq_threshold": 99 }));
        let old = def_with_vars(&[("dq_threshold", json!(10)), pin.clone()]);
        let new = def_with_vars(&[("dq_threshold", json!(5)), pin]);
        let analysis = attribute_var_changes(&current, &old, &new);
        let entry = &analysis.facts_by_label["dq_threshold"].entries[0];
        assert_eq!(
            entry.direct,
            vec!["model.shop.reader"],
            "the pinned package's model is not reached",
        );
        assert_eq!(entry.masked_packages, vec!["dbt_utils"]);
        assert!(!analysis.references.contains_key("model.dbt_utils.helper"));
    }

    #[test]
    fn attribution_package_scoped_edit_reaches_only_that_package() {
        let (pkg_id, pkg_model) = model(
            "model.dbt_utils.helper",
            Some("select {{ var('dq_threshold') }}"),
            Some("dbt_utils"),
            None,
            &[],
        );
        let (own_id, own) = model(
            "model.shop.reader",
            Some("select {{ var('dq_threshold') }}"),
            Some("shop"),
            None,
            &[],
        );
        let current = manifest_of(vec![(pkg_id, pkg_model), (own_id, own)]);
        let old = def_with_vars(&[("dbt_utils", json!({ "dq_threshold": 1 }))]);
        let new = def_with_vars(&[("dbt_utils", json!({ "dq_threshold": 2 }))]);
        let analysis = attribute_var_changes(&current, &old, &new);
        let facts = &analysis.facts_by_label["dbt_utils"];
        let entry = &facts.entries[0];
        assert_eq!(entry.package.as_deref(), Some("dbt_utils"));
        assert_eq!(entry.direct, vec!["model.dbt_utils.helper"]);
        assert!(entry.masked_packages.is_empty());
    }

    #[test]
    fn attribution_a_new_pin_changes_resolution_and_attributes() {
        // Pin added only in NEW: dbt_utils models flip global→pin — a
        // real resolution change, attributed (never masked).
        let (pkg_id, pkg_model) = model(
            "model.dbt_utils.helper",
            Some("select {{ var('dq_threshold') }}"),
            Some("dbt_utils"),
            None,
            &[],
        );
        let current = manifest_of(vec![(pkg_id, pkg_model)]);
        let old = def_with_vars(&[("dq_threshold", json!(10))]);
        let new = def_with_vars(&[
            ("dq_threshold", json!(10)),
            ("dbt_utils", json!({ "dq_threshold": 99 })),
        ]);
        let analysis = attribute_var_changes(&current, &old, &new);
        let entry = &analysis.facts_by_label["dbt_utils"].entries[0];
        assert_eq!(entry.direct, vec!["model.dbt_utils.helper"]);
    }

    #[test]
    fn attribution_subtracts_override_pinned_unit_tests() {
        let current = scenario_manifest();
        let mut overrides = UnitTestOverrides::new();
        overrides.insert(
            "vars".to_owned(),
            BTreeMap::from([("dq_threshold".to_owned(), json!(5))]),
        );
        let pinned = UnitTest::new(
            "pinned_test",
            NodeId::new("model.shop.reads_direct"),
            Vec::<UnitTestGiven>::new(),
            UnitTestExpect::new(Value::Array(Vec::new()), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_overrides(Some(overrides));
        let mut other_overrides = UnitTestOverrides::new();
        other_overrides.insert(
            "vars".to_owned(),
            BTreeMap::from([("some_other_var".to_owned(), json!(1))]),
        );
        let unpinned = UnitTest::new(
            "unpinned_test",
            NodeId::new("model.shop.reads_direct"),
            Vec::<UnitTestGiven>::new(),
            UnitTestExpect::new(Value::Array(Vec::new()), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_overrides(Some(other_overrides));
        let nodes = current.nodes().clone();
        let macros = current.macros().clone();
        let with_tests = Manifest::new(
            current.metadata().clone(),
            nodes,
            vec![
                ("unit_test.shop.reads_direct.pinned_test".to_owned(), pinned),
                (
                    "unit_test.shop.reads_direct.unpinned_test".to_owned(),
                    unpinned,
                ),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
            macros,
        )
        .with_macro_depends_on(BTreeMap::from([(
            "macro.shop.outer".to_owned(),
            vec!["macro.shop.inner".to_owned()],
        )]));
        let old = def_with_vars(&[("dq_threshold", json!(10))]);
        let new = def_with_vars(&[("dq_threshold", json!(5))]);
        let analysis = attribute_var_changes(&with_tests, &old, &new);
        assert_eq!(
            analysis.facts_by_label["dq_threshold"].entries[0].insulated_tests,
            vec!["unit_test.shop.reads_direct.pinned_test"],
            "only the test pinning THIS var is insulated",
        );
    }

    #[test]
    fn attribution_dynamic_bucket_holds_unruled_out_models_only() {
        let (dynamic_id, dynamic_model) = model(
            "model.shop.dynamic_caller",
            Some("select {{ var(computed_name) }}"),
            Some("shop"),
            None,
            &[],
        );
        let (both_id, both) = model(
            "model.shop.both",
            Some("select {{ var('dq_threshold') }} {{ var(other) }}"),
            Some("shop"),
            None,
            &[],
        );
        let current = manifest_of(vec![(dynamic_id, dynamic_model), (both_id, both)]);
        let old = def_with_vars(&[("dq_threshold", json!(10))]);
        let new = def_with_vars(&[("dq_threshold", json!(5))]);
        let analysis = attribute_var_changes(&current, &old, &new);
        let entry = &analysis.facts_by_label["dq_threshold"].entries[0];
        assert_eq!(
            entry.dynamic,
            vec!["model.shop.dynamic_caller"],
            "a literal hit on the var removes the model from the bucket",
        );
        assert_eq!(entry.direct, vec!["model.shop.both"]);
    }

    #[test]
    fn attribution_python_models_skip_the_sql_scan_and_count() {
        // A .py model's source is Python — `var(` text there is not the
        // Jinja global; the scan must skip it and count it honestly.
        let (py_id, py) = model(
            "model.shop.py_model",
            Some("limit = dbt.config.get('x'); v = var('dq_threshold')"),
            Some("shop"),
            Some("models/py_model.py"),
            &[],
        );
        let current = manifest_of(vec![(py_id, py)]);
        let old = def_with_vars(&[("dq_threshold", json!(10))]);
        let new = def_with_vars(&[("dq_threshold", json!(5))]);
        let analysis = attribute_var_changes(&current, &old, &new);
        let facts = &analysis.facts_by_label["dq_threshold"];
        assert!(facts.entries[0].direct.is_empty());
        assert_eq!(facts.footprint.python_models, 1);
    }

    #[test]
    fn attribution_without_edits_is_empty_and_scans_nothing() {
        let def = def_with_vars(&[("x", json!(1))]);
        assert_eq!(
            attribute_var_changes(&scenario_manifest(), &def, &def),
            VarAnalysis::default(),
        );
    }

    #[test]
    fn attribution_unknown_package_models_join_global_edits_only() {
        // Pre-#256 wire: no package_name + no project_name. A global
        // edit reaches the model (no pin can exist); a package-scoped
        // edit never does.
        let (id, node) = model(
            "model.shop.unlabelled",
            Some("select {{ var('x') }}"),
            None,
            None,
            &[],
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            vec![(id, node)].into_iter().collect::<HashMap<_, _>>(),
            HashMap::new(),
            HashMap::new(),
        );
        let old = def_with_vars(&[("x", json!(1))]);
        let new = def_with_vars(&[("x", json!(2))]);
        let analysis = attribute_var_changes(&current, &old, &new);
        assert_eq!(
            analysis.facts_by_label["x"].entries[0].direct,
            vec!["model.shop.unlabelled"],
        );
    }

    // ----- attach_var_facts + integration with the structural diff -----

    #[test]
    fn attach_joins_facts_to_vars_rows_by_label_and_skips_others() {
        let current = scenario_manifest();
        let old = ProjectDefinition {
            vars: BTreeMap::from([("dq_threshold".to_owned(), json!(10))]),
            name: Some("shop".to_owned()),
            ..ProjectDefinition::default()
        };
        let mut new = old.clone();
        new.vars.insert("dq_threshold".to_owned(), json!(5));
        new.name = Some("renamed".to_owned());
        let mut changes = diff_project_definitions(&old, &new);
        let analysis = attribute_var_changes(&current, &old, &new);
        attach_var_facts(&mut changes, &analysis);
        let vars_row = changes
            .iter()
            .find(|c| c.category == ProjectChangeCategory::Vars)
            .expect("a vars row");
        let facts = vars_row.vars.as_ref().expect("facts attached");
        assert_eq!(facts.entries[0].name, "dq_threshold");
        assert_eq!(facts.entries[0].direct, vec!["model.shop.reads_direct"]);
        let identity_row = changes
            .iter()
            .find(|c| c.category == ProjectChangeCategory::Identity)
            .expect("an identity row");
        assert_eq!(identity_row.vars, None, "non-vars rows stay untouched");
    }

    #[test]
    fn attach_package_scope_row_carries_every_inner_edit() {
        // The structural diff emits ONE row for the package key; the
        // facts expand it per inner var under the same label.
        let (pkg_id, pkg_model) = model(
            "model.dbt_utils.helper",
            Some("select {{ var('a') }}, {{ var('b') }}"),
            Some("dbt_utils"),
            None,
            &[],
        );
        let current = manifest_of(vec![(pkg_id, pkg_model)]);
        let old = def_with_vars(&[("dbt_utils", json!({ "a": 1, "b": 1 }))]);
        let new = def_with_vars(&[("dbt_utils", json!({ "a": 2, "b": 2 }))]);
        let mut changes = diff_project_definitions(&old, &new);
        let analysis = attribute_var_changes(&current, &old, &new);
        attach_var_facts(&mut changes, &analysis);
        let facts = changes[0].vars.as_ref().expect("facts attached");
        assert_eq!(
            facts
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"],
        );
    }
}
