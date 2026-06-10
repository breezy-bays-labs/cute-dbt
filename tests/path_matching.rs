//! Mutation-kill suite for the path-matching free functions in
//! `cute_dbt::domain::path` (per cute-dbt#81 + CQO audit obligation;
//! moved out of `scope` at cute-dbt#96 to break the `scope ⇄ pr_diff`
//! cycle).
//!
//! `tests/steps/diff_scoping.rs` and the cucumber BDD outer loop assert
//! the user-visible behavior (model X appears in scope when PR file Y
//! changed). This suite kills mutants on the path-matching primitives
//! themselves so a regression in `normalize_path` / `match_changed_path`
//! cannot hide behind a coarse-grained BDD pass.
//!
//! Windows-style `\` separators are canonicalized to `/` (cute-dbt#183,
//! resolving the v0.1 deferral split out of cute-dbt#80): dbt manifests
//! compiled **on Windows** serialize `original_file_path` from a native
//! `PathBuf` with `\` separators (dbt-fusion `crates/dbt-schemas/src/
//! schemas/manifest/manifest_nodes.rs` @ `9977b6cb`), while `git diff`
//! emits `/`-separated repo paths on every platform (git stores index /
//! tree entry path names with `/` — `git/git` `Documentation/
//! gitformat-index.adoc` @ `1ff279f34`). The `\`-separator cases below
//! mirror each `/` case shape for mutation-kill parity.

use std::path::Path;

use cute_dbt::domain::path::{match_changed_path, normalize_path};

// =====================================================================
// normalize_path — exhaustive case set
// =====================================================================

#[test]
fn unchanged_paths_pass_through() {
    assert_eq!(normalize_path("models/x.sql", None), "models/x.sql");
}

#[test]
fn leading_dot_slash_is_stripped() {
    assert_eq!(normalize_path("./models/x.sql", None), "models/x.sql");
}

#[test]
fn repeated_leading_dot_slash_is_stripped() {
    assert_eq!(normalize_path("././models/x.sql", None), "models/x.sql");
    assert_eq!(normalize_path("./././models/x.sql", None), "models/x.sql");
}

#[test]
fn project_root_prefix_is_stripped() {
    assert_eq!(
        normalize_path("dbt_project/models/x.sql", Some(Path::new("dbt_project"))),
        "models/x.sql"
    );
}

#[test]
fn project_root_prefix_with_trailing_slash_is_stripped() {
    assert_eq!(
        normalize_path("dbt_project/models/x.sql", Some(Path::new("dbt_project/"))),
        "models/x.sql"
    );
}

#[test]
fn project_root_prefix_not_applied_when_path_does_not_start_with_it() {
    // changed path is repo-relative without the project_root segment —
    // happens for non-dbt files like `README.md` or root-level configs.
    assert_eq!(
        normalize_path("README.md", Some(Path::new("dbt_project"))),
        "README.md"
    );
}

#[test]
fn double_slash_runs_collapse() {
    assert_eq!(normalize_path("models//x.sql", None), "models/x.sql");
    assert_eq!(
        normalize_path("models////marts/x.sql", None),
        "models/marts/x.sql"
    );
}

#[test]
fn leading_dot_slash_and_project_root_combine() {
    // Order matters: `./` strip happens BEFORE prefix strip so a path
    // emitted by `git diff` as `./dbt_project/models/x.sql` is still
    // matched against a `dbt_project`-rooted manifest.
    assert_eq!(
        normalize_path("./dbt_project/models/x.sql", Some(Path::new("dbt_project"))),
        "models/x.sql"
    );
}

#[test]
fn project_root_strip_does_not_match_substring() {
    // A path containing `dbt_project` mid-string (e.g. someone names a
    // file `my_dbt_project_notes.md`) is not stripped — only the literal
    // prefix at position 0.
    assert_eq!(
        normalize_path(
            "models/my_dbt_project_notes.sql",
            Some(Path::new("dbt_project"))
        ),
        "models/my_dbt_project_notes.sql"
    );
}

#[test]
fn project_root_strip_requires_full_segment_match() {
    // Bot-review finding (Gemini + CodeRabbit on cute-dbt#86): a directory
    // whose name STARTS WITH the prefix (e.g. `dbt_project_notes/x.sql`
    // with prefix `dbt_project`) must NOT be stripped — that would
    // produce `_notes/x.sql` and wrongly match a manifest path like
    // `_notes/x.sql`. Prefix matching is segment-aware: `prefix` or
    // `prefix/` only.
    assert_eq!(
        normalize_path("dbt_project_notes/x.sql", Some(Path::new("dbt_project"))),
        "dbt_project_notes/x.sql"
    );
}

#[test]
fn project_root_strip_handles_exact_prefix_only() {
    // When the path IS the prefix (no trailing content), strip yields an
    // empty string — caller's manifest path won't match anything empty.
    assert_eq!(
        normalize_path("dbt_project", Some(Path::new("dbt_project"))),
        ""
    );
}

#[test]
fn empty_project_root_is_treated_as_no_prefix() {
    // `Some(Path::new(""))` is treated identically to `None` — strip a
    // zero-length prefix is a no-op.
    assert_eq!(
        normalize_path("models/x.sql", Some(Path::new(""))),
        "models/x.sql"
    );
}

// =====================================================================
// match_changed_path — boolean over a changed-files vec
// =====================================================================

#[test]
fn match_exact_path_returns_true() {
    let changed = vec!["models/x.sql".to_owned()];
    assert!(match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn match_with_leading_dot_slash_returns_true() {
    let changed = vec!["./models/x.sql".to_owned()];
    assert!(match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn match_with_project_root_strip_returns_true() {
    let changed = vec!["dbt_project/models/x.sql".to_owned()];
    assert!(match_changed_path(
        "models/x.sql",
        &changed,
        Some(Path::new("dbt_project"))
    ));
}

#[test]
fn no_match_for_unrelated_path_returns_false() {
    let changed = vec!["README.md".to_owned()];
    assert!(!match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn no_match_when_changed_list_is_empty() {
    let changed: Vec<String> = Vec::new();
    assert!(!match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn match_finds_target_when_present_among_many() {
    let changed = vec![
        "README.md".to_owned(),
        "packages.yml".to_owned(),
        "models/x.sql".to_owned(),
        ".github/workflows/ci.yml".to_owned(),
    ];
    assert!(match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn match_returns_false_when_manifest_path_has_substring_collision() {
    // The manifest path `models/x.sql` must not match a changed path
    // `models/x.sql.bak` — substring containment is NOT acceptance.
    let changed = vec!["models/x.sql.bak".to_owned()];
    assert!(!match_changed_path("models/x.sql", &changed, None));
}

// =====================================================================
// Windows-style `\` separators (cute-dbt#183) — mutation-kill parity
// mirrors of the `/` cases above. The real-world shape: a manifest
// compiled on Windows carries `\`-separated original_file_path while
// the PR diff carries `/`; normalize_path canonicalizes `\` → `/` so
// both sides resolve to the same key.
// =====================================================================

// ----- normalize_path: backslash mirrors -----

#[test]
fn backslash_separators_canonicalize_to_forward_slash() {
    // Mirror of unchanged_paths_pass_through: the backslash analog does
    // NOT pass through — it canonicalizes.
    assert_eq!(normalize_path("models\\x.sql", None), "models/x.sql");
}

#[test]
fn leading_dot_backslash_is_stripped() {
    assert_eq!(normalize_path(".\\models\\x.sql", None), "models/x.sql");
}

#[test]
fn repeated_leading_dot_backslash_is_stripped() {
    assert_eq!(normalize_path(".\\.\\models\\x.sql", None), "models/x.sql");
    assert_eq!(
        normalize_path(".\\.\\.\\models\\x.sql", None),
        "models/x.sql"
    );
}

#[test]
fn project_root_prefix_is_stripped_from_backslash_path() {
    assert_eq!(
        normalize_path("dbt_project\\models\\x.sql", Some(Path::new("dbt_project"))),
        "models/x.sql"
    );
}

#[test]
fn backslash_project_root_prefix_with_trailing_backslash_is_stripped() {
    // Mirror of project_root_prefix_with_trailing_slash_is_stripped: a
    // Windows-style `--project-root dbt_project\` canonicalizes before
    // the trailing-separator trim.
    assert_eq!(
        normalize_path(
            "dbt_project\\models\\x.sql",
            Some(Path::new("dbt_project\\"))
        ),
        "models/x.sql"
    );
}

#[test]
fn project_root_prefix_not_applied_when_backslash_path_does_not_start_with_it() {
    assert_eq!(
        normalize_path("docs\\README.md", Some(Path::new("dbt_project"))),
        "docs/README.md"
    );
}

#[test]
fn double_backslash_runs_collapse() {
    // Two literal backslashes canonicalize to `//` and collapse, the
    // same as the double_slash_runs_collapse mirror; a mixed `\/` run
    // collapses too.
    assert_eq!(normalize_path("models\\\\x.sql", None), "models/x.sql");
    assert_eq!(
        normalize_path("models\\\\\\\\marts\\x.sql", None),
        "models/marts/x.sql"
    );
    assert_eq!(normalize_path("models\\/x.sql", None), "models/x.sql");
}

#[test]
fn leading_dot_backslash_and_project_root_combine() {
    // Mirror of leading_dot_slash_and_project_root_combine: separator
    // canonicalization happens BEFORE the `./` strip and prefix strip,
    // so a fully Windows-shaped path still matches.
    assert_eq!(
        normalize_path(
            ".\\dbt_project\\models\\x.sql",
            Some(Path::new("dbt_project"))
        ),
        "models/x.sql"
    );
}

#[test]
fn backslash_project_root_strip_does_not_match_substring() {
    assert_eq!(
        normalize_path(
            "models\\my_dbt_project_notes.sql",
            Some(Path::new("dbt_project"))
        ),
        "models/my_dbt_project_notes.sql"
    );
}

#[test]
fn backslash_project_root_strip_requires_full_segment_match() {
    // Mirror of project_root_strip_requires_full_segment_match: a
    // directory whose name starts with the prefix must NOT be stripped
    // after canonicalization either.
    assert_eq!(
        normalize_path("dbt_project_notes\\x.sql", Some(Path::new("dbt_project"))),
        "dbt_project_notes/x.sql"
    );
}

#[test]
fn backslash_project_root_strip_handles_exact_prefix_only() {
    // Mirror of project_root_strip_handles_exact_prefix_only with a
    // Windows-style trailing-`\` prefix.
    assert_eq!(
        normalize_path("dbt_project", Some(Path::new("dbt_project\\"))),
        ""
    );
}

#[test]
fn backslash_only_project_root_is_treated_as_no_prefix() {
    // Mirror of empty_project_root_is_treated_as_no_prefix: a `\`-only
    // prefix canonicalizes to `/`, trims to empty, and is a no-op.
    assert_eq!(
        normalize_path("models\\x.sql", Some(Path::new("\\"))),
        "models/x.sql"
    );
}

#[test]
fn mixed_separator_path_normalizes() {
    // A path mixing `/` and `\` (e.g. a Windows tool joining onto a
    // `/`-separated base) resolves to the same key as either pure form.
    assert_eq!(
        normalize_path(
            "dbt_project/models\\marts\\x.sql",
            Some(Path::new("dbt_project"))
        ),
        "models/marts/x.sql"
    );
}

// ----- match_changed_path: backslash mirrors -----

#[test]
fn match_backslash_changed_path_against_forward_slash_manifest() {
    let changed = vec!["models\\x.sql".to_owned()];
    assert!(match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn match_forward_slash_changed_path_against_backslash_manifest() {
    // THE Windows gap (cute-dbt#183): a manifest compiled on Windows
    // carries `\`-separated original_file_path; the git diff carries
    // `/`. Without canonicalization this silently missed.
    let changed = vec!["models/x.sql".to_owned()];
    assert!(match_changed_path("models\\x.sql", &changed, None));
}

#[test]
fn match_with_leading_dot_backslash_returns_true() {
    let changed = vec![".\\models\\x.sql".to_owned()];
    assert!(match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn match_backslash_path_with_project_root_strip_returns_true() {
    let changed = vec!["dbt_project\\models\\x.sql".to_owned()];
    assert!(match_changed_path(
        "models/x.sql",
        &changed,
        Some(Path::new("dbt_project"))
    ));
}

#[test]
fn no_match_for_unrelated_backslash_path_returns_false() {
    let changed = vec!["docs\\README.md".to_owned()];
    assert!(!match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn match_finds_backslash_target_when_present_among_many() {
    let changed = vec![
        "README.md".to_owned(),
        "packages.yml".to_owned(),
        "models\\x.sql".to_owned(),
        ".github\\workflows\\ci.yml".to_owned(),
    ];
    assert!(match_changed_path("models/x.sql", &changed, None));
}

#[test]
fn backslash_substring_collision_returns_false() {
    // Canonicalization must not loosen acceptance: `models\x.sql.bak`
    // still does not match `models/x.sql`.
    let changed = vec!["models\\x.sql.bak".to_owned()];
    assert!(!match_changed_path("models/x.sql", &changed, None));
}
