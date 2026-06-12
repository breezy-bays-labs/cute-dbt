//! `dbt_project.yml` parser — dbt-yaml (the engine's own published
//! serde-yaml fork) → [`ProjectDefinition`] (cute-dbt#266).
//!
//! **Engine fidelity by construction**: this adapter mirrors the exact
//! loading semantics dbt-fusion applies to every YAML file
//! (`crates/dbt-jinja-utils/src/serde.rs:339-373` @ 9977b6cb) —
//! [`dbt_yaml::Value::from_str`] with a [`DuplicateKey::Overwrite`]
//! callback (last key wins, dbt semantics) followed by
//! [`apply_merge`](dbt_yaml::Value::apply_merge) (explicit `<<:`
//! merge-key resolution; anchors/aliases resolve natively in libyaml).
//! Quoted Jinja scalars (`"{{ var(...) }}"`) stay opaque strings —
//! zero-compute cute-dbt never renders them.
//!
//! **Tolerant ingestion, never validation**: both engine dialects parse.
//! A bare legacy config key without `+` (dbt-core warns, fusion
//! strict-errors) is ingested as a config key; deprecated path keys
//! (`source-paths`, `data-paths`) land in
//! [`ProjectDefinition::paths`] like their modern twins. Failure
//! surfaces only for YAML the parser itself rejects — mapped into the
//! owned [`ProjectParseError`] (no `dbt_yaml` type leaves this module;
//! the clean-arch guard greps the domain for the bare crate name).
//!
//! **Per-subtree degrade for non-JSON YAML**: the domain vocabulary is
//! `serde_json::Value`, so the rare YAML-only shapes convert to explicit
//! marker strings instead of failing the parse — a non-finite float
//! (`.nan` / `.inf`) becomes its YAML literal as a string, a `!tagged`
//! value becomes `"<unsupported YAML value (!tag)>"`, and a non-string
//! mapping key is stringified. Visible degrades, never silent drops.
//!
//! **No port trait**: one impl, a plain `fn parse(&str)` — ADR-1's
//! more-than-one-impl bar is not met (the serde-saphyr contingency
//! swaps behind this same seam). File access stays on the existing
//! [`crate::ports::ProjectFileReader`] port; this module never does I/O.

use dbt_yaml::mapping::DuplicateKey;
use dbt_yaml::{Mapping, Value as YamlValue};
use serde_json::Value;

use crate::domain::project_def::Span;
use crate::domain::{ConfigTree, ProjectDefinition};

/// Why a `dbt_project.yml` text failed to parse — an **owned** degradation
/// enum (no parser types cross it). Every variant degrades the panel to
/// the Shape-A raw-diff row; report generation never fails on it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjectParseError {
    /// The YAML itself did not parse (syntax error, unresolvable alias,
    /// merge failure). Carries the parser's message verbatim.
    Yaml {
        /// The underlying parser error, stringified.
        message: String,
    },
    /// The document parsed but its root is not a mapping (a bare scalar
    /// or sequence) — not a `dbt_project.yml` shape.
    NotAMapping,
}

/// The top-level keys that open a per-resource config tree — fusion's
/// `RootProjectConfigs` section vocabulary (dbt-parser
/// `dbt_project_config.rs:346-371` @ 9977b6cb), accepted in both the
/// hyphenated and underscored spellings (ingest, never validate).
const CONFIG_TREE_SECTIONS: [&str; 17] = [
    "models",
    "seeds",
    "snapshots",
    "tests",
    "data_tests",
    "data-tests",
    "unit_tests",
    "unit-tests",
    "sources",
    "exposures",
    "metrics",
    "semantic-models",
    "semantic_models",
    "saved-queries",
    "saved_queries",
    "analyses",
    "functions",
];

/// Parse `dbt_project.yml` text into the domain [`ProjectDefinition`].
///
/// Loading semantics match fusion's shared YAML entry: duplicate keys
/// resolve last-wins ([`DuplicateKey::Overwrite`]), then `<<:` merge
/// keys apply ([`dbt_yaml::Value::apply_merge`]). An empty / null
/// document parses to [`ProjectDefinition::default()`] (tolerant — the
/// caller decides what absence means).
///
/// # Errors
///
/// [`ProjectParseError::Yaml`] for text the parser rejects;
/// [`ProjectParseError::NotAMapping`] for a non-mapping document root.
pub fn parse(text: &str) -> Result<ProjectDefinition, ProjectParseError> {
    let mut value = YamlValue::from_str(text, |_path, _new, _existing| DuplicateKey::Overwrite)
        .map_err(|err| ProjectParseError::Yaml {
            message: err.to_string(),
        })?;
    value.apply_merge().map_err(|err| ProjectParseError::Yaml {
        message: err.to_string(),
    })?;
    match &value {
        YamlValue::Null(_) => Ok(ProjectDefinition::default()),
        YamlValue::Mapping(map, _) => Ok(definition_from_mapping(map)),
        _ => Err(ProjectParseError::NotAMapping),
    }
}

/// Route every top-level key into exactly one [`ProjectDefinition`]
/// field — nothing a project file says is silently dropped.
fn definition_from_mapping(map: &Mapping) -> ProjectDefinition {
    let mut def = ProjectDefinition::default();
    for (key, value) in map {
        route_top_level_key(&mut def, key_string(key), value);
    }
    def
}

/// The per-key router: each `ProjectDefinition` family has its own
/// small ingestion helper (CRAP-gate decomposition, cute-dbt#266
/// review) so every branch is independently testable.
fn route_top_level_key(def: &mut ProjectDefinition, key_name: String, value: &YamlValue) {
    match key_name.as_str() {
        "name" => def.name = Some(scalar_display_string(value)),
        "version" => def.version = Some(yaml_to_json(value)),
        "require-dbt-version" | "require_dbt_version" => {
            def.require_dbt_version = Some(yaml_to_json(value));
        }
        "vars" => ingest_vars(def, key_name, value),
        section if CONFIG_TREE_SECTIONS.contains(&section) => {
            ingest_config_section(def, key_name, value);
        }
        "dispatch" => def.dispatch = Some(yaml_to_json(value)),
        "on-run-start" | "on_run_start" => def.on_run_start = hook_entries(value),
        "on-run-end" | "on_run_end" => def.on_run_end = hook_entries(value),
        "flags" => def.flags = Some(yaml_to_json(value)),
        _ if is_path_key(&key_name) => {
            def.paths.insert(key_name, yaml_to_json(value));
        }
        _ => {
            def.other.insert(key_name, yaml_to_json(value));
        }
    }
}

/// Ingest the `vars:` block: each entry's value plus its definition-site
/// span. A non-mapping `vars:` is not a vars block — a null body is an
/// empty block (dropped), any other shape is kept verbatim in `other`
/// where it can still be diffed truthfully.
fn ingest_vars(def: &mut ProjectDefinition, key_name: String, value: &YamlValue) {
    match value.as_mapping() {
        Some(vars) => {
            for (var_key, var_value) in vars {
                ingest_var_entry(def, var_key, var_value);
            }
        }
        None => {
            if !value.is_null() {
                def.other.insert(key_name, yaml_to_json(value));
            }
        }
    }
}

/// Ingest one `vars:` entry — the value into
/// [`ProjectDefinition::vars`], the key's source position (when the
/// parser produced one) into [`ProjectDefinition::vars_spans`].
fn ingest_var_entry(def: &mut ProjectDefinition, var_key: &YamlValue, var_value: &YamlValue) {
    let var_name = key_string(var_key);
    let start = &var_key.span().start;
    if start.line > 0 {
        def.vars_spans.insert(
            var_name.clone(),
            Span {
                line: start.line,
                column: start.column,
            },
        );
    }
    def.vars.insert(var_name, yaml_to_json(var_value));
}

/// Ingest one per-resource config-tree section (`models:` / `seeds:` /
/// …): a mapping builds the tree, a null body is an empty tree, and any
/// other scalar shape is kept verbatim in `other`.
fn ingest_config_section(def: &mut ProjectDefinition, key_name: String, value: &YamlValue) {
    match value.as_mapping() {
        Some(tree) => {
            def.config_trees
                .insert(key_name, config_tree_from_mapping(tree));
        }
        None if value.is_null() => {
            def.config_trees.insert(key_name, ConfigTree::default());
        }
        None => {
            def.other.insert(key_name, yaml_to_json(value));
        }
    }
}

/// Whether a top-level key is path configuration: every `…-paths` /
/// `…_paths` / `…-path` / `…_path` key (the deprecated `source-paths` /
/// `data-paths` included) plus `clean-targets`.
fn is_path_key(key: &str) -> bool {
    key.ends_with("-paths")
        || key.ends_with("_paths")
        || key.ends_with("-path")
        || key.ends_with("_path")
        || key == "clean-targets"
        || key == "clean_targets"
}

/// Build one config-tree node, mirroring fusion's raw project-config
/// walk (`merge_raw_config_mappings` / `recur_raw_project_config`,
/// dbt-parser `utils.rs:62-105` @ 9977b6cb): `+key` → config (prefix
/// stripped), non-`+` mapping key → child. The one tolerant extension:
/// a non-`+` key with a **non-mapping** value — the bare legacy
/// dbt-core dialect fusion strict-errors on — is ingested as a config
/// key (ingest, never validate).
fn config_tree_from_mapping(map: &Mapping) -> ConfigTree {
    let mut tree = ConfigTree::default();
    for (key, value) in map {
        let key_name = key_string(key);
        if let Some(config_key) = key_name.strip_prefix('+') {
            tree.configs
                .insert(config_key.to_owned(), yaml_to_json(value));
        } else if let Some(child) = value.as_mapping() {
            tree.children
                .insert(key_name, config_tree_from_mapping(child));
        } else {
            tree.configs.insert(key_name, yaml_to_json(value));
        }
    }
    tree
}

/// Normalize an `on-run-start:` / `on-run-end:` body: a sequence keeps
/// its entries, a null is empty, any scalar wraps into a one-element
/// list (dbt accepts both authored forms).
fn hook_entries(value: &YamlValue) -> Vec<Value> {
    match value {
        YamlValue::Null(_) => Vec::new(),
        YamlValue::Sequence(seq, _) => seq.iter().map(yaml_to_json).collect(),
        other => vec![yaml_to_json(other)],
    }
}

/// Convert a YAML value into the domain's `serde_json::Value`
/// vocabulary — infallible, with explicit per-subtree degrade markers
/// for the shapes JSON cannot represent (module docs).
fn yaml_to_json(value: &YamlValue) -> Value {
    match value {
        YamlValue::Null(_) => Value::Null,
        YamlValue::Bool(b, _) => Value::Bool(*b),
        YamlValue::Number(n, _) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else {
                // Finite floats convert; .nan/.inf have no JSON form —
                // degrade to their YAML literal as a visible string.
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map_or_else(|| Value::String(n.to_string()), Value::Number)
            }
        }
        YamlValue::String(s, _) => Value::String(s.clone()),
        YamlValue::Sequence(seq, _) => Value::Array(seq.iter().map(yaml_to_json).collect()),
        YamlValue::Mapping(map, _) => Value::Object(
            map.iter()
                .map(|(k, v)| (key_string(k), yaml_to_json(v)))
                .collect(),
        ),
        YamlValue::Tagged(tagged, _) => {
            Value::String(format!("<unsupported YAML value ({})>", tagged.tag))
        }
    }
}

/// A scalar's display string: string values verbatim, anything else via
/// its JSON conversion compacted — tolerant ingestion for fields the
/// POD types as `String` (`name:`).
fn scalar_display_string(value: &YamlValue) -> String {
    match value.as_str() {
        Some(s) => s.to_owned(),
        None => yaml_to_json(value).to_string(),
    }
}

/// A mapping key as a string: string keys verbatim; null/bool/number
/// scalar keys stringified; complex (sequence/mapping/tagged) keys
/// degrade to the parser's own serialization, trimmed — deterministic
/// and visibly YAML-shaped.
fn key_string(key: &YamlValue) -> String {
    match key {
        YamlValue::String(s, _) => s.clone(),
        YamlValue::Null(_) => "null".to_owned(),
        YamlValue::Bool(b, _) => b.to_string(),
        YamlValue::Number(n, _) => n.to_string(),
        other => dbt_yaml::to_string(other)
            .map_or_else(|_| "<complex key>".to_owned(), |s| s.trim().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The committed synthetic acceptance fixture (cute-dbt#266 — the
    /// research acceptance list: anchors/aliases/`<<:` merge keys,
    /// duplicate-key last-wins, quoted Jinja scalars, `+`-nested config
    /// trees, flow lists, the v1.10 top-level `anchors:` key, bare
    /// legacy config keys, deprecated path keys).
    const ACCEPTANCE: &str = include_str!("../../tests/fixtures/project-def-acceptance.yml");

    fn parsed() -> ProjectDefinition {
        parse(ACCEPTANCE).expect("the acceptance fixture parses")
    }

    // ----- identity + other routing -----

    #[test]
    fn identity_fields_parse() {
        let def = parsed();
        assert_eq!(def.name.as_deref(), Some("acceptance_project"));
        assert_eq!(def.version, Some(json!("1.0.0")));
        assert_eq!(
            def.require_dbt_version,
            Some(json!([">=1.8.0", "<3.0.0"])),
            "flow list parses into a JSON array",
        );
    }

    #[test]
    fn profile_config_version_query_comment_and_anchors_land_in_other() {
        let def = parsed();
        assert_eq!(def.other.get("profile"), Some(&json!("acceptance")));
        assert_eq!(def.other.get("config-version"), Some(&json!(2)));
        assert_eq!(
            def.other.get("query-comment"),
            Some(&json!("run by cute-dbt acceptance fixture")),
        );
        // The v1.10 top-level `anchors:` reuse block is kept verbatim.
        assert_eq!(
            def.other.get("anchors"),
            Some(&json!({ "default_states": ["CT", "VT", "MA"] })),
        );
    }

    // ----- vars: jinja scalars, aliases, duplicate keys, spans -----

    #[test]
    fn quoted_jinja_scalar_stays_an_opaque_string() {
        assert_eq!(
            parsed().vars.get("greeting"),
            Some(&json!("{{ var('fallback', 'hello') }}")),
        );
    }

    #[test]
    fn an_alias_resolves_to_its_anchored_value() {
        assert_eq!(
            parsed().vars.get("audit_states"),
            Some(&json!(["CT", "VT", "MA"])),
            "*default_states resolves through the anchors: block",
        );
    }

    #[test]
    fn duplicate_keys_resolve_last_wins() {
        // `retry_count` is authored twice (1 then 3) — dbt-yaml's
        // Overwrite policy (fusion parity, serde.rs:366) keeps the last.
        assert_eq!(parsed().vars.get("retry_count"), Some(&json!(3)));
    }

    #[test]
    fn package_scoped_vars_stay_nested() {
        assert_eq!(
            parsed().vars.get("scoped_package"),
            Some(&json!({ "enabled": true })),
        );
    }

    #[test]
    fn var_definition_sites_carry_spans() {
        let def = parsed();
        let span = def
            .vars_spans
            .get("greeting")
            .expect("greeting has a definition span");
        // The fixture authors `greeting:` on a known line; pin it so a
        // fixture edit that moves it fails loudly here, keeping the
        // span semantics honest (1-based source line).
        let expected_line = ACCEPTANCE
            .lines()
            .position(|l| l.trim_start().starts_with("greeting:"))
            .expect("fixture defines greeting")
            + 1;
        assert_eq!(span.line, expected_line);
        assert!(span.column >= 1, "column is 1-based");
    }

    // ----- config trees: merge keys, +-nesting, bare legacy keys -----

    #[test]
    fn merge_key_entries_apply_into_the_package_level() {
        let def = parsed();
        let project = &def.config_trees["models"].children["acceptance_project"];
        // `<<: *shared_config` merged +materialized / +persist_docs in.
        assert_eq!(project.configs.get("materialized"), Some(&json!("view")));
        assert_eq!(
            project.configs.get("persist_docs"),
            Some(&json!({ "relations": true, "columns": true })),
            "flow-mapping config value survives the merge",
        );
        assert_eq!(project.configs.get("tags"), Some(&json!(["project-wide"])));
    }

    #[test]
    fn plus_nested_config_trees_strip_the_prefix_and_keep_hierarchy() {
        let def = parsed();
        let marts = &def.config_trees["models"].children["acceptance_project"].children["marts"];
        assert_eq!(marts.configs.get("materialized"), Some(&json!("table")));
        assert!(marts.children.contains_key("finance"));
    }

    #[test]
    fn a_bare_legacy_config_key_is_ingested_as_config_not_a_child() {
        // `materialized: incremental` without `+` — the dbt-core legacy
        // dialect fusion strict-errors on. Ingest, never validate.
        let def = parsed();
        let finance = &def.config_trees["models"].children["acceptance_project"].children["marts"]
            .children["finance"];
        assert_eq!(
            finance.configs.get("materialized"),
            Some(&json!("incremental")),
        );
        assert!(finance.children.is_empty());
        assert_eq!(
            finance.configs.get("meta"),
            Some(&json!({ "owner": "finance-team" })),
        );
    }

    #[test]
    fn the_anchor_defining_shared_block_is_a_tree_child() {
        // `shared: &shared_config {…}` under models: is a (phantom)
        // hierarchy child — exactly how the engines read it.
        let def = parsed();
        let shared = &def.config_trees["models"].children["shared"];
        assert_eq!(shared.configs.get("materialized"), Some(&json!("view")));
    }

    #[test]
    fn seeds_parse_as_their_own_tree() {
        let def = parsed();
        let seeds = &def.config_trees["seeds"].children["acceptance_project"];
        assert_eq!(seeds.configs.get("quote_columns"), Some(&json!(false)));
    }

    // ----- hooks / dispatch / flags / paths -----

    #[test]
    fn crlf_authored_files_parse_to_lf_clean_hook_entries() {
        // Gemini review on PR #285 (cute-dbt#269): empirical pin that
        // dbt-yaml normalizes CRLF line breaks per the YAML spec —
        // quoted scalars, plain scalars, AND block scalars all parse
        // LF-clean (block scalars keep their clip-chomped trailing
        // `\n`). The hook-comparison helpers in domain::project_def
        // therefore see `\r`-free text from the parsed-file sides; the
        // manifest raw_code side is the unguaranteed vector they defend
        // against.
        let text = "name: p\r\non-run-start:\r\n  - \"grant usage on x\"\r\n  - plain scalar hook\r\n  - |\r\n    line1\r\n    line2\r\non-run-end: single plain\r\n";
        let def = parse(text).expect("CRLF input parses");
        assert_eq!(
            def.on_run_start,
            vec![
                json!("grant usage on x"),
                json!("plain scalar hook"),
                json!("line1\nline2\n"),
            ],
            "every scalar style normalizes CRLF away",
        );
        assert_eq!(def.on_run_end, vec![json!("single plain")]);
    }

    #[test]
    fn scalar_and_list_hooks_both_normalize_to_lists() {
        let def = parsed();
        assert_eq!(
            def.on_run_start,
            vec![json!("grant usage on database analytics to role reporter")],
            "a scalar on-run-start wraps into a one-element list",
        );
        assert_eq!(def.on_run_end, vec![json!("{{ log('done', info=true) }}")]);
    }

    #[test]
    fn dispatch_and_flags_parse_verbatim() {
        let def = parsed();
        assert_eq!(
            def.dispatch,
            Some(json!([{
                "macro_namespace": "dbt_utils",
                "search_order": ["acceptance_project", "dbt_utils"],
            }])),
        );
        assert_eq!(
            def.flags,
            Some(json!({ "send_anonymous_usage_stats": false })),
        );
    }

    #[test]
    fn modern_deprecated_and_clean_target_path_keys_all_land_in_paths() {
        let def = parsed();
        assert_eq!(def.paths.get("model-paths"), Some(&json!(["models"])));
        // Deprecated key (dbt-core dialect) — ingested, never validated.
        assert_eq!(def.paths.get("source-paths"), Some(&json!(["models"])));
        assert_eq!(
            def.paths.get("clean-targets"),
            Some(&json!(["target", "dbt_packages"])),
        );
    }

    // ----- failure + degrade arms -----

    #[test]
    fn malformed_yaml_is_an_owned_yaml_error() {
        let err = parse("models:\n  - [unclosed").expect_err("malformed YAML fails");
        match err {
            ProjectParseError::Yaml { message } => {
                assert!(!message.is_empty(), "carries the parser's message");
            }
            other @ ProjectParseError::NotAMapping => {
                panic!("expected Yaml, got {other:?}")
            }
        }
    }

    #[test]
    fn a_non_mapping_root_is_not_a_project_definition() {
        assert_eq!(parse("just a scalar"), Err(ProjectParseError::NotAMapping),);
        assert_eq!(parse("- a\n- list"), Err(ProjectParseError::NotAMapping));
    }

    #[test]
    fn an_empty_document_parses_to_the_default_definition() {
        assert_eq!(
            parse("").expect("empty parses"),
            ProjectDefinition::default()
        );
        assert_eq!(
            parse("# only a comment\n").expect("comment-only parses"),
            ProjectDefinition::default(),
        );
    }

    #[test]
    fn a_tagged_value_degrades_to_a_visible_marker_string() {
        let def = parse("custom: !secret redacted\n").expect("tags parse tolerantly");
        let marker = def.other.get("custom").expect("custom routed to other");
        let text = marker.as_str().expect("marker is a string");
        assert!(
            text.starts_with("<unsupported YAML value ("),
            "visible per-subtree degrade, never a silent drop: {text}",
        );
    }

    #[test]
    fn a_non_finite_float_degrades_to_its_yaml_literal_string() {
        let def = parse("vars:\n  ratio: .nan\n").expect("parses");
        let v = def.vars.get("ratio").expect("ratio present");
        assert!(v.is_string(), "no JSON form for NaN — degraded: {v:?}");
    }

    #[test]
    fn non_string_scalar_keys_are_stringified() {
        let def = parse("models:\n  2024: { +materialized: table }\n").expect("parses");
        assert!(
            def.config_trees["models"].children.contains_key("2024"),
            "a numeric folder key is stringified, not dropped",
        );
    }

    // ----- per-family degrade arms (direct branch coverage) -----

    #[test]
    fn a_non_mapping_vars_block_is_kept_verbatim_in_other() {
        // `vars: 5` is not a vars block — keep it where it can still be
        // diffed truthfully, never silently dropped.
        let def = parse("vars: 5\n").expect("parses tolerantly");
        assert!(def.vars.is_empty());
        assert_eq!(def.other.get("vars"), Some(&json!(5)));
    }

    #[test]
    fn a_null_vars_block_is_an_empty_block() {
        let def = parse("vars:\nname: p\n").expect("parses");
        assert!(def.vars.is_empty(), "null vars ⇒ empty block");
        assert!(!def.other.contains_key("vars"), "null is not noise");
    }

    #[test]
    fn a_null_config_section_is_an_empty_tree() {
        let def = parse("models:\nname: p\n").expect("parses");
        let tree = def.config_trees.get("models").expect("models present");
        assert!(tree.configs.is_empty() && tree.children.is_empty());
    }

    #[test]
    fn a_scalar_config_section_is_kept_verbatim_in_other() {
        let def = parse("models: 3\n").expect("parses tolerantly");
        assert!(!def.config_trees.contains_key("models"));
        assert_eq!(def.other.get("models"), Some(&json!(3)));
    }

    #[test]
    fn a_null_hook_body_is_an_empty_hook_list() {
        let def = parse("on-run-start:\non-run-end:\n").expect("parses");
        assert!(def.on_run_start.is_empty());
        assert!(def.on_run_end.is_empty());
    }

    #[test]
    fn a_non_string_name_is_stringified() {
        // `name: 123` — tolerant ingestion stringifies the scalar
        // (scalar_display_string's non-string arm).
        let def = parse("name: 123\n").expect("parses tolerantly");
        assert_eq!(def.name.as_deref(), Some("123"));
    }

    #[test]
    fn a_u64_beyond_i64_converts_losslessly() {
        let def = parse("vars:\n  big: 18446744073709551615\n").expect("parses");
        assert_eq!(
            def.vars.get("big"),
            Some(&json!(18_446_744_073_709_551_615_u64)),
        );
    }

    #[test]
    fn bool_and_null_mapping_keys_are_stringified() {
        // YAML 1.1 scalar keys that are not strings — `true:` and `~:`
        // — stringify deterministically (key_string's bool/null arms).
        let def = parse("quoting:\n  true: 1\n  ~: 2\n").expect("parses");
        let quoting = def.other.get("quoting").expect("routed to other");
        assert_eq!(quoting.get("true"), Some(&json!(1)));
        assert_eq!(quoting.get("null"), Some(&json!(2)));
    }

    #[test]
    fn a_complex_mapping_key_degrades_to_its_yaml_serialization() {
        // A sequence key (`? [a, b]`) has no JSON form — key_string
        // degrades to the parser's own serialization, trimmed, so the
        // entry is deterministic and visibly YAML-shaped, never dropped.
        let def = parse("custom:\n  ? [a, b]\n  : 1\n").expect("parses");
        let custom = def.other.get("custom").expect("routed to other");
        let obj = custom.as_object().expect("a mapping converts");
        let key = obj.keys().next().expect("the complex key survives");
        assert!(
            key.contains('a') && key.contains('b'),
            "the serialized key names both elements: {key:?}",
        );
        assert_eq!(obj.values().next(), Some(&json!(1)));
    }
}
