//! Operator-supplied analysis configuration.
//!
//! Loaded via clap's `--config <PATH>` value-parser in the `cli` layer
//! (a TOML file → `AnalysisConfig` POD). The CLI resolves the rendered
//! report title from this struct (`config.report.title` →
//! [`DEFAULT_REPORT_TITLE`]); the renderer reads the resolved strings as
//! plain `&str` parameters and never imports `toml` or
//! `AnalysisConfig` directly.
//!
//! The surface grows by additive POD sections, each a new `pub` field
//! on `AnalysisConfig` with `#[serde(default)]`, never a comparator /
//! scoping rewrite: `[report]` (two optional keys, PR 14) and
//! `[checks]` (selection + suppression, cute-dbt#171 — POD in
//! `crate::domain::check_config`).
//!
//! `#[serde(deny_unknown_fields)]` is applied at both nesting levels so
//! a misnamed key (`report.tilte`, `repotr.title`) fails the
//! value-parser loudly rather than being silently ignored.
//!
//! Errors raised on parse are clap usage errors (exit 2), **not**
//! [`crate::domain::PreflightError`] variants — config errors are
//! usage-time, not runtime preflight (ARCHITECTURE.md §3, the same
//! baseline-missing precedent).

use serde::Deserialize;

use crate::domain::check_config::ChecksConfig;
use crate::domain::experimental::ExperimentalConfig;

/// Default `<title>` and `<h1>` text when no `--config` is supplied or
/// the config omits `report.title`.
///
/// Single source of truth — the askama template falls back to whatever
/// string `cli::execute` resolves for the report title; this constant
/// pins the fallback to the v0.0 baseline string so absent-config
/// renders byte-for-byte unchanged.
pub const DEFAULT_REPORT_TITLE: &str = "cute-dbt report";

/// Operator-supplied configuration, deserialized from the
/// `--config <PATH>` TOML file.
///
/// All fields are optional with `Default` populating them — an empty
/// TOML file (or no `--config` flag at all) yields
/// `AnalysisConfig::default()`, which renders identically to the
/// pre-PR-14 output.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalysisConfig {
    /// `[report]` section — report-metadata overrides surfaced in the
    /// rendered HTML's `<title>`, `<h1>`, and (optionally) a new
    /// `<p class="report-subtitle">` element.
    #[serde(default)]
    pub report: ReportConfig,
    /// `[checks]` section (cute-dbt#171) — check selection
    /// (opt-out/opt-in modes with id/group-glob lists) and
    /// `[[checks.suppress]]` acknowledgements. Cross-field legality and
    /// id/glob resolution against the check registry are validated by
    /// [`crate::domain::check_config::resolve_check_policy`] at
    /// `--config` parse time (the same clap usage-error surface as a
    /// TOML syntax error).
    #[serde(default)]
    pub checks: ChecksConfig,
    /// `[experimental]` section (cute-dbt#289, epic #288) — opt-in to
    /// not-yet-stable surfaces by exact experiment id
    /// (`enable = ["project-state"]`). Validated against the closed
    /// vocabulary by
    /// [`crate::domain::experimental::resolve_experimental_config`] at
    /// `--config` parse time (the `[checks]` posture — an unknown id is
    /// a clap usage error, exit 2). Union semantics with the
    /// `CUTE_DBT_EXPERIMENTAL` env var.
    #[serde(default)]
    pub experimental: ExperimentalConfig,
    /// `[pr]` section (cute-dbt#346) — the source-PR context surfaced as a
    /// link in the `--pr-diff` change-context banner. All keys optional; a
    /// usable ref needs both `url` and `title` (the CLI / `review`
    /// resolution then [`PrConfig::resolve`]s them into a [`PrRef`]).
    /// Absent / incomplete ⇒ the banner renders link-free, byte-identical
    /// to the pre-#346 output.
    #[serde(default)]
    pub pr: PrConfig,
}

/// `[pr]` table (cute-dbt#346) — the source-PR context for the
/// change-context banner link. All keys optional.
///
/// The CLI flags (`--pr-url`, `--pr-title`, `--pr-number`) override these
/// per-key (the CLI-over-TOML precedence the `[experimental]` cap already
/// follows). [`PrConfig::resolve`] folds the merged values into an
/// `Option<PrRef>`: `Some` only when both a `url` and a `title` are
/// present.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrConfig {
    /// The PR's GitHub URL — the `<a href>` navigation target
    /// (`https://github.com/<owner>/<repo>/pull/<n>`). Absent ⇒ no link.
    pub url: Option<String>,
    /// The PR title — rendered as adjacent (askama-escaped) text after the
    /// linked `PR #<n>` token. Absent ⇒ no link.
    pub title: Option<String>,
    /// The PR number, shown as `PR #<n>`. Absent ⇒ derived from the
    /// trailing `/pull/<n>` segment of [`url`](Self::url).
    pub number: Option<u64>,
}

impl PrConfig {
    /// Fold the configured PR fields into an `Option<PrRef>`.
    ///
    /// `Some(PrRef)` iff a non-blank `url` AND a non-blank `title` are
    /// present (the change-context banner needs both — a link with no
    /// title, or a title with no href, is the dangling-affordance the
    /// graceful-degradation AC forbids). The number falls back to the
    /// `/pull/<n>` segment parsed from the url; an unparseable url with no
    /// explicit number yields `0` (rendered `PR #0`, never a panic).
    #[must_use]
    pub fn resolve(&self) -> Option<PrRef> {
        let url = non_blank(self.url.as_deref())?;
        let title = non_blank(self.title.as_deref())?;
        let number = self.number.or_else(|| pr_number_from_url(url)).unwrap_or(0);
        Some(PrRef {
            number,
            title: title.to_owned(),
            url: url.to_owned(),
        })
    }
}

/// The resolved source-PR reference rendered into the change-context
/// banner (cute-dbt#346) — a pure POD the renderer turns into a linked
/// `PR #<n> — <title>` clause. Built by [`PrConfig::resolve`] from the
/// merged CLI / TOML / `review`-derived inputs; `None` keeps the banner
/// link-free (graceful degradation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    /// The PR number, shown as `PR #<n>`.
    pub number: u64,
    /// The PR title — rendered as askama-escaped adjacent text.
    pub title: String,
    /// The GitHub URL — the `<a href>` navigation target.
    pub url: String,
}

/// `Some(trimmed)` when `s` is present and not blank; `None` otherwise.
fn non_blank(s: Option<&str>) -> Option<&str> {
    let t = s?.trim();
    (!t.is_empty()).then_some(t)
}

/// Parse the PR number from a GitHub PR url's trailing `/pull/<n>`
/// segment (`https://github.com/o/r/pull/123` → `123`). Tolerates a
/// trailing slash / query / fragment after the number. `None` when no
/// `/pull/<digits>` segment is found.
fn pr_number_from_url(url: &str) -> Option<u64> {
    let after = url.split("/pull/").nth(1)?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// `[report]` table — both keys optional.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReportConfig {
    /// Replaces both the `<title>` and `<h1>` text. Absent → falls back
    /// to [`DEFAULT_REPORT_TITLE`].
    pub title: Option<String>,
    /// When present, renders as a new `<p class="report-subtitle">`
    /// element immediately after the `<h1>`. Absent → the element is
    /// omitted entirely (no empty DOM node).
    pub subtitle: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_yields_all_none() {
        let cfg = AnalysisConfig::default();
        assert!(cfg.report.title.is_none());
        assert!(cfg.report.subtitle.is_none());
    }

    #[test]
    fn empty_toml_string_deserializes_to_default() {
        let cfg: AnalysisConfig = toml::from_str("").expect("empty TOML parses");
        assert_eq!(cfg, AnalysisConfig::default());
    }

    #[test]
    fn report_section_with_title_only_populates_title() {
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[report]
title = "Q3 unit test review"
"#,
        )
        .expect("title-only TOML parses");
        assert_eq!(cfg.report.title.as_deref(), Some("Q3 unit test review"));
        assert!(cfg.report.subtitle.is_none());
    }

    #[test]
    fn report_section_with_subtitle_only_populates_subtitle() {
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[report]
subtitle = "PR 1234 / staging diff"
"#,
        )
        .expect("subtitle-only TOML parses");
        assert!(cfg.report.title.is_none());
        assert_eq!(
            cfg.report.subtitle.as_deref(),
            Some("PR 1234 / staging diff")
        );
    }

    #[test]
    fn report_section_with_both_keys_populates_both() {
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[report]
title = "Q3 review"
subtitle = "PR 1234"
"#,
        )
        .expect("both-keys TOML parses");
        assert_eq!(cfg.report.title.as_deref(), Some("Q3 review"));
        assert_eq!(cfg.report.subtitle.as_deref(), Some("PR 1234"));
    }

    #[test]
    fn missing_report_section_yields_default_report() {
        // A TOML file with no [report] section is well-formed; the
        // ReportConfig default fills in via #[serde(default)].
        let cfg: AnalysisConfig =
            toml::from_str("# only a comment, no sections\n").expect("comment-only TOML parses");
        assert_eq!(cfg, AnalysisConfig::default());
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        // deny_unknown_fields at the AnalysisConfig level: a stray
        // top-level table (typo'd section name) is a clap usage error.
        let err =
            toml::from_str::<AnalysisConfig>("[reprt]\ntitle = \"oops\"\n").expect_err("[reprt]");
        let msg = err.to_string();
        assert!(
            msg.contains("reprt") || msg.contains("unknown field"),
            "error names the unknown field: {msg}"
        );
    }

    #[test]
    fn unknown_report_field_is_rejected() {
        // deny_unknown_fields at the ReportConfig level: a typo'd key
        // inside [report] is a clap usage error.
        let err = toml::from_str::<AnalysisConfig>(
            r#"
[report]
tilte = "typo'd"
"#,
        )
        .expect_err("tilte typo should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("tilte") || msg.contains("unknown field"),
            "error names the unknown field: {msg}"
        );
    }

    #[test]
    fn invalid_toml_syntax_is_rejected() {
        // Wholesale-broken TOML should produce a parse error (the
        // clap value-parser surfaces this as the usage-error path).
        let err = toml::from_str::<AnalysisConfig>("not valid toml { = =").expect_err("garbage");
        let msg = err.to_string();
        assert!(!msg.is_empty(), "error has a description: {msg}");
    }

    #[test]
    fn checks_section_parses_through_analysis_config() {
        // The [checks] POD lives in domain::check_config; this pins the
        // AnalysisConfig wiring (section name + serde(default)).
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[checks]
mode = "opt-in"
enable = ["grain.*"]

[[checks.suppress]]
check = "grain.unique-key-unbacked"
model = "orders"
reason = "we know and don't care"
"#,
        )
        .expect("checks section parses");
        assert_eq!(
            cfg.checks.mode,
            crate::domain::check_config::ChecksMode::OptIn
        );
        assert_eq!(
            cfg.checks.enable.as_deref(),
            Some(&["grain.*".to_owned()][..])
        );
        assert_eq!(cfg.checks.suppress.len(), 1);
    }

    #[test]
    fn absent_checks_section_yields_the_default() {
        let cfg: AnalysisConfig = toml::from_str("[report]\ntitle = \"t\"\n").expect("parses");
        assert_eq!(
            cfg.checks,
            crate::domain::check_config::ChecksConfig::default()
        );
    }

    #[test]
    fn unknown_checks_key_is_rejected() {
        let err = toml::from_str::<AnalysisConfig>("[checks]\nenabel = [\"grain.*\"]\n")
            .expect_err("typo'd checks key");
        let msg = err.to_string();
        assert!(
            msg.contains("enabel") || msg.contains("unknown field"),
            "error names the unknown field: {msg}"
        );
    }

    #[test]
    fn experimental_section_parses_through_analysis_config() {
        // The [experimental] POD lives in domain::experimental; this
        // pins the AnalysisConfig wiring (section name + serde(default)).
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[experimental]
enable = ["project-state"]
"#,
        )
        .expect("experimental section parses");
        assert_eq!(cfg.experimental.enable, vec!["project-state".to_owned()]);
    }

    #[test]
    fn absent_experimental_section_yields_the_default() {
        let cfg: AnalysisConfig = toml::from_str("[report]\ntitle = \"t\"\n").expect("parses");
        assert_eq!(
            cfg.experimental,
            crate::domain::experimental::ExperimentalConfig::default()
        );
        assert!(cfg.experimental.enable.is_empty());
    }

    #[test]
    fn experimental_macro_body_cap_parses_through_analysis_config() {
        // cute-dbt#265 Slice D — the gen-time inline-body cap knob rides the
        // [experimental] section (the macro lens is itself experimental).
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[experimental]
enable = ["macro-lens"]
macro_body_cap = 3
"#,
        )
        .expect("macro_body_cap parses");
        assert_eq!(cfg.experimental.macro_body_cap, Some(3));
        assert_eq!(cfg.experimental.enable, vec!["macro-lens".to_owned()]);
    }

    #[test]
    fn absent_macro_body_cap_yields_none() {
        let cfg: AnalysisConfig =
            toml::from_str("[experimental]\nenable = [\"macro-lens\"]\n").expect("parses");
        assert!(cfg.experimental.macro_body_cap.is_none());
    }

    #[test]
    fn unknown_experimental_key_is_rejected() {
        let err =
            toml::from_str::<AnalysisConfig>("[experimental]\nenabel = [\"project-state\"]\n")
                .expect_err("typo'd experimental key");
        let msg = err.to_string();
        assert!(
            msg.contains("enabel") || msg.contains("unknown field"),
            "error names the unknown field: {msg}"
        );
    }

    #[test]
    fn round_trip_via_default_serialization_preserves_values() {
        // Serialize-then-deserialize equivalence isn't testable directly
        // (Deserialize-only — no Serialize derive). Instead, manually
        // construct a fully-populated value and verify the per-field
        // deserialization round-trip is order-independent.
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[report]
subtitle = "second-key-first"
title = "second-position-title"
"#,
        )
        .expect("out-of-order keys parse");
        assert_eq!(cfg.report.title.as_deref(), Some("second-position-title"));
        assert_eq!(cfg.report.subtitle.as_deref(), Some("second-key-first"));
    }

    // ===== [pr] section + PrConfig::resolve (cute-dbt#346) =====

    #[test]
    fn absent_pr_section_yields_no_ref() {
        let cfg: AnalysisConfig = toml::from_str("[report]\ntitle = \"t\"\n").expect("parses");
        assert_eq!(cfg.pr, PrConfig::default());
        assert!(cfg.pr.resolve().is_none());
    }

    #[test]
    fn pr_section_parses_all_keys() {
        let cfg: AnalysisConfig = toml::from_str(
            r#"
[pr]
url = "https://github.com/o/r/pull/42"
title = "Add churn model"
number = 42
"#,
        )
        .expect("pr section parses");
        assert_eq!(
            cfg.pr.url.as_deref(),
            Some("https://github.com/o/r/pull/42")
        );
        assert_eq!(cfg.pr.title.as_deref(), Some("Add churn model"));
        assert_eq!(cfg.pr.number, Some(42));
    }

    #[test]
    fn resolve_with_url_and_title_yields_ref() {
        let cfg = PrConfig {
            url: Some("https://github.com/o/r/pull/42".to_owned()),
            title: Some("Add churn model".to_owned()),
            number: None,
        };
        let pr = cfg.resolve().expect("url + title ⇒ ref");
        assert_eq!(pr.number, 42, "number derived from /pull/<n>");
        assert_eq!(pr.title, "Add churn model");
        assert_eq!(pr.url, "https://github.com/o/r/pull/42");
    }

    #[test]
    fn resolve_explicit_number_overrides_url_segment() {
        let cfg = PrConfig {
            url: Some("https://github.com/o/r/pull/42".to_owned()),
            title: Some("t".to_owned()),
            number: Some(7),
        };
        assert_eq!(cfg.resolve().expect("ref").number, 7);
    }

    #[test]
    fn resolve_unparseable_url_without_number_is_zero_not_panic() {
        let cfg = PrConfig {
            url: Some("https://example.com/no-pull-segment".to_owned()),
            title: Some("t".to_owned()),
            number: None,
        };
        assert_eq!(cfg.resolve().expect("ref").number, 0);
    }

    #[test]
    fn resolve_missing_title_yields_none() {
        let cfg = PrConfig {
            url: Some("https://github.com/o/r/pull/42".to_owned()),
            title: None,
            number: Some(42),
        };
        assert!(cfg.resolve().is_none(), "no title ⇒ no link (no dangling)");
    }

    #[test]
    fn resolve_missing_url_yields_none() {
        let cfg = PrConfig {
            url: None,
            title: Some("t".to_owned()),
            number: Some(42),
        };
        assert!(cfg.resolve().is_none(), "no url ⇒ no link (no dangling)");
    }

    #[test]
    fn resolve_blank_url_or_title_yields_none() {
        let cfg = PrConfig {
            url: Some("   ".to_owned()),
            title: Some("t".to_owned()),
            number: None,
        };
        assert!(cfg.resolve().is_none(), "blank url is treated as absent");
        let cfg = PrConfig {
            url: Some("https://github.com/o/r/pull/1".to_owned()),
            title: Some("  ".to_owned()),
            number: None,
        };
        assert!(cfg.resolve().is_none(), "blank title is treated as absent");
    }

    #[test]
    fn resolve_trims_url_and_title() {
        let cfg = PrConfig {
            url: Some("  https://github.com/o/r/pull/9  ".to_owned()),
            title: Some("  spacey  ".to_owned()),
            number: None,
        };
        let pr = cfg.resolve().expect("ref");
        assert_eq!(pr.url, "https://github.com/o/r/pull/9");
        assert_eq!(pr.title, "spacey");
        assert_eq!(pr.number, 9);
    }

    #[test]
    fn pr_number_from_url_tolerates_trailing_segments() {
        assert_eq!(
            pr_number_from_url("https://github.com/o/r/pull/123/files"),
            Some(123)
        );
        assert_eq!(
            pr_number_from_url("https://github.com/o/r/pull/123#diff"),
            Some(123)
        );
        assert_eq!(pr_number_from_url("https://github.com/o/r/issues/5"), None);
    }

    #[test]
    fn unknown_pr_key_is_rejected() {
        let err = toml::from_str::<AnalysisConfig>("[pr]\nurll = \"x\"\n")
            .expect_err("typo'd pr key should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("urll") || msg.contains("unknown field"),
            "error names the unknown field: {msg}"
        );
    }
}
