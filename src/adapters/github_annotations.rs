//! GitHub workflow-command annotation projection (cute-dbt#393).
//!
//! The first GitHub-review-UX projection off the findings: a pure
//! formatter that turns policy-applied [`Finding`]s + their resolved
//! [`ResolvedAnchor`]s into the `::warning file=,line=,title=::message`
//! [workflow commands][wc] GitHub renders **inline on the Files-changed
//! tab** — zero auth, zero API call, zero token, identical on public and
//! private repos. The emit is a gen-time `stdout` print; it never touches
//! `report.html`, so the view-time zero-egress gate is untouched.
//!
//! [wc]: https://docs.github.com/actions/reference/workflow-commands-for-github-actions
//!
//! ## What gets annotated
//!
//! Only **[`Verdict::Uncovered`]** findings — they are the ones carrying a
//! `recommendation` (the annotation message) AND a coverage gap worth a
//! reviewer's eye. A covered / unknown / suppressed finding produces no
//! annotation (it still feeds the [check-run summary](summary_markdown)
//! roll-up). A finding with no resolvable [`ResolvedAnchor`] (its model
//! file isn't in the diff) is **summary-only**: it is counted but not
//! emitted as an inline annotation, because there is no honest line to
//! pin it to.
//!
//! ## Tier → annotation level
//!
//! | [`Tier`]   | level     | when                                    |
//! |------------|-----------|-----------------------------------------|
//! | `Advisory` | `notice`  | always                                  |
//! | `High`     | `warning` | always                                  |
//! | `Total`    | `error`   | only when the uncovered-gate trips      |
//! | `Total`    | `warning` | when the gate is NOT tripping (advisory)|
//!
//! A `Total`-tier uncovered finding is a by-definition gap (the
//! cute-dbt#388 `--fail-on-uncovered` gate's trigger). When the caller is
//! running that gate it escalates to `error` (a red annotation that
//! matches the failing check); otherwise it rides as a `warning` so the
//! annotation still surfaces without falsely implying a hard failure. The
//! caller passes the gate state in via [`AnnotationLevels`].
//!
//! ## The per-step cap
//!
//! GitHub renders at most ~10 annotations **per step** before truncating
//! silently. [`emit_annotations`] honours that honestly: it emits the top
//! `cap` (highest-severity first — `error` ▸ `warning` ▸ `notice`), then a
//! single `::notice::+K more …` overflow line pointing at the full report,
//! so a reviewer is never misled into thinking the capped list is the
//! whole story.
//!
//! Pure adapter: borrows the policy-applied findings + the anchor resolver
//! output, returns owned `String`s. No I/O — the CLI does the `println!`.

use std::fmt::Write as _;

use crate::domain::checks::{CheckId, Finding, Tier, Verdict};
use crate::domain::finding_anchor::ResolvedAnchor;

/// GitHub's documented soft cap on annotations rendered per workflow step
/// before silent truncation. The default [`emit_annotations`] cap.
pub const DEFAULT_ANNOTATION_CAP: usize = 10;

/// The workflow-command level an annotation emits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationLevel {
    /// `::notice` — informational (advisory-tier).
    Notice,
    /// `::warning` — high-tier, or a total-tier gap when the gate is off.
    Warning,
    /// `::error` — a total-tier gap when the uncovered-gate is tripping.
    Error,
}

impl AnnotationLevel {
    /// The workflow-command keyword (`notice` / `warning` / `error`).
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// Severity rank for the cap's highest-first ordering (higher = more
    /// severe = emitted first).
    fn rank(self) -> u8 {
        match self {
            Self::Notice => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }
}

/// Whether the uncovered-gate (`--fail-on-uncovered`, cute-dbt#388) is
/// tripping for this run — the single bit that escalates a `Total`-tier
/// gap from `warning` to `error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnotationLevels {
    /// `true` when the run is enforcing the uncovered-gate, so a
    /// total-tier uncovered finding is a genuine build failure → `error`.
    pub gate_tripping: bool,
}

impl AnnotationLevels {
    /// The default posture: no gate, so a total-tier gap is an advisory
    /// `warning`, not an `error`.
    #[must_use]
    pub fn advisory() -> Self {
        Self {
            gate_tripping: false,
        }
    }

    /// The gate-enforcing posture: a total-tier uncovered gap escalates
    /// to `error`.
    #[must_use]
    pub fn enforcing() -> Self {
        Self {
            gate_tripping: true,
        }
    }

    /// Map a finding's tier to its annotation level under this posture.
    #[must_use]
    pub fn level_for(self, tier: Tier) -> AnnotationLevel {
        match tier {
            Tier::Advisory => AnnotationLevel::Notice,
            Tier::High => AnnotationLevel::Warning,
            Tier::Total => {
                if self.gate_tripping {
                    AnnotationLevel::Error
                } else {
                    AnnotationLevel::Warning
                }
            }
        }
    }
}

/// One renderable annotation: a level, an anchor, and the message body.
///
/// The intermediate the formatter sorts + caps before serializing to the
/// workflow-command line. Owns its strings (the path comes off the
/// finding's resolved [`ResolvedAnchor`], the id + message off the finding)
/// so the value is self-contained and trivially sortable.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Annotation {
    level: AnnotationLevel,
    check_id: String,
    path: String,
    line: usize,
    message: String,
}

impl Annotation {
    /// Serialize to the GitHub workflow-command line:
    /// `::<level> file=<path>,line=<n>,title=cute-dbt: <id>::<message>`.
    fn to_command(&self) -> String {
        format!(
            "::{} file={},line={},title={}::{}",
            self.level.keyword(),
            escape_property(&self.path),
            self.line,
            escape_property(&format!("cute-dbt: {}", self.check_id)),
            escape_data(&self.message),
        )
    }
}

/// Escape a workflow-command **message** (the text after the final `::`).
///
/// Per GitHub's spec only `%`, CR, and LF are special in message data.
fn escape_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escape a workflow-command **property value** (`file=` / `line=` /
/// `title=`).
///
/// Property values additionally reserve `,` (the property separator) and
/// `:` (which would otherwise close the command), so those escape too.
fn escape_property(value: &str) -> String {
    escape_data(value).replace(':', "%3A").replace(',', "%2C")
}

/// Build the renderable annotation list from the policy-applied findings
/// and a resolver, highest-severity first.
///
/// `anchor_for` is the anchor resolver applied to each finding (the CLI
/// passes a closure over [`resolve_finding_anchor`](crate::domain::resolve_finding_anchor)
/// bound to the manifest + diff index). Only `Uncovered`, non-suppressed
/// findings with a resolvable anchor AND a recommendation become
/// annotations; everything else is summary-only.
fn build_annotations<Id: CheckId>(
    findings: &[Finding<Id>],
    levels: AnnotationLevels,
    anchor_for: &impl Fn(&Finding<Id>) -> Option<ResolvedAnchor>,
) -> Vec<Annotation> {
    let mut annotations: Vec<Annotation> = findings
        .iter()
        .filter(|f| matches!(f.verdict, Verdict::Uncovered) && f.suppressed.is_none())
        .filter_map(|f| {
            let recommendation = f.recommendation.clone()?;
            let anchor = anchor_for(f)?;
            Some(Annotation {
                level: levels.level_for(f.tier),
                check_id: f.check.as_str().to_owned(),
                path: anchor.path,
                line: anchor.line,
                message: recommendation,
            })
        })
        .collect();
    // Highest severity first, then by check id + path + line for
    // determinism (stable, capped output regardless of finding order).
    annotations.sort_by(|a, b| {
        b.level
            .rank()
            .cmp(&a.level.rank())
            .then_with(|| a.check_id.cmp(&b.check_id))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    annotations
}

/// The set of stdout lines a `--annotations` run emits: the capped
/// per-finding workflow commands plus an optional overflow notice.
///
/// Returned as owned lines so the CLI does the `println!` (keeping the I/O
/// at the boundary). `total` is the count of *annotatable* findings before
/// the cap; `lines.len()` may be `cap + 1` (the overflow notice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationEmit {
    /// The workflow-command lines, in emit order, including any overflow
    /// `::notice::+K more` line at the end.
    pub lines: Vec<String>,
    /// How many annotatable findings there were before the cap was
    /// applied.
    pub total: usize,
    /// How many were suppressed by the cap (`0` when nothing overflowed).
    pub overflowed: usize,
}

/// Format the workflow-command annotations for a run's findings, capped at
/// `cap` with an honest overflow notice.
///
/// The top `cap` annotations (highest-severity first) emit as
/// `::<level> …::<message>` lines; if more than `cap` were annotatable, a
/// final `::notice::+K more uncovered finding(s) — see the full cute-dbt
/// report` line is appended so the truncation is never silent.
#[must_use]
pub fn emit_annotations<Id: CheckId>(
    findings: &[Finding<Id>],
    levels: AnnotationLevels,
    cap: usize,
    anchor_for: &impl Fn(&Finding<Id>) -> Option<ResolvedAnchor>,
) -> AnnotationEmit {
    let annotations = build_annotations(findings, levels, anchor_for);
    let total = annotations.len();
    let mut lines: Vec<String> = annotations
        .iter()
        .take(cap)
        .map(Annotation::to_command)
        .collect();
    let overflowed = total.saturating_sub(lines.len());
    if overflowed > 0 {
        lines.push(format!(
            "::notice::+{overflowed} more uncovered finding(s) — see the full cute-dbt report"
        ));
    }
    AnnotationEmit {
        lines,
        total,
        overflowed,
    }
}

/// A one-glance roll-up of a run's findings for a GitHub check-run summary
/// (or the cute-dbt#71 sticky comment).
///
/// Counts uncovered findings by tier and the covered total — no
/// line-anchor needed. `report_link` is an optional URL to the full HTML
/// report (a placeholder until the CI publishes one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingsSummary {
    /// Uncovered `Total`-tier findings (the by-definition gaps).
    pub total_uncovered: usize,
    /// Uncovered `High`-tier findings.
    pub high_uncovered: usize,
    /// Uncovered `Advisory`-tier findings.
    pub advisory_uncovered: usize,
    /// Covered findings (a satisfying test attributes).
    pub covered: usize,
}

impl FindingsSummary {
    /// Tally a run's policy-applied findings into the roll-up. Suppressed
    /// findings are excluded from the uncovered counts (an operator
    /// acknowledged them) but a covered finding is always covered.
    #[must_use]
    pub fn tally<Id: CheckId>(findings: &[Finding<Id>]) -> Self {
        let mut summary = Self {
            total_uncovered: 0,
            high_uncovered: 0,
            advisory_uncovered: 0,
            covered: 0,
        };
        for finding in findings {
            match &finding.verdict {
                Verdict::Covered { .. } => summary.covered += 1,
                Verdict::Uncovered if finding.suppressed.is_none() => match finding.tier {
                    Tier::Total => summary.total_uncovered += 1,
                    Tier::High => summary.high_uncovered += 1,
                    Tier::Advisory => summary.advisory_uncovered += 1,
                },
                // Suppressed uncovered + Unknown verdicts: not a gap to
                // count, not a covered guarantee either.
                Verdict::Uncovered | Verdict::Unknown => {}
            }
        }
        summary
    }

    /// `true` when the run surfaced zero uncovered gaps of any tier.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.total_uncovered == 0 && self.high_uncovered == 0 && self.advisory_uncovered == 0
    }
}

/// Render the check-run summary markdown roll-up.
///
/// One-glance: `N Total / M High / K Advisory uncovered · X covered`, an
/// optional report link, and a clean-run line when there are no gaps.
#[must_use]
pub fn summary_markdown(summary: &FindingsSummary, report_link: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("### cute-dbt coverage findings\n\n");
    if summary.is_clean() {
        let _ = writeln!(
            out,
            "No uncovered findings. {} construct(s) covered.",
            summary.covered
        );
    } else {
        let _ = writeln!(
            out,
            "**{} Total** / **{} High** / **{} Advisory** uncovered · {} covered",
            summary.total_uncovered,
            summary.high_uncovered,
            summary.advisory_uncovered,
            summary.covered,
        );
    }
    if let Some(link) = report_link {
        let _ = writeln!(out, "\n[Full report]({link})");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::checks::{Evidence, HeuristicId, Verdict};
    use crate::domain::finding_anchor::AnchorSide;
    use crate::domain::manifest::NodeId;

    fn uncovered(check: HeuristicId, model: &str) -> Finding<HeuristicId> {
        Finding::new(
            check,
            NodeId::new(model),
            "config.unique_key",
            Verdict::Uncovered,
            Vec::new(),
        )
    }

    fn covered(check: HeuristicId, model: &str) -> Finding<HeuristicId> {
        Finding::new(
            check,
            NodeId::new(model),
            "config.unique_key",
            Verdict::Covered {
                by: vec!["test.shop.x".to_owned()],
            },
            Vec::new(),
        )
    }

    fn anchor(path: &str, line: usize) -> ResolvedAnchor {
        ResolvedAnchor {
            path: path.to_owned(),
            line,
            diff_context: AnchorSide::Modified,
        }
    }

    // Resolver stub: a fixed anchor for every finding.
    fn always(
        path: &'static str,
        line: usize,
    ) -> impl Fn(&Finding<HeuristicId>) -> Option<ResolvedAnchor> {
        move |_| Some(anchor(path, line))
    }

    // ----- escaping -------------------------------------------------

    #[test]
    fn escape_data_handles_percent_and_newlines() {
        assert_eq!(escape_data("100% off\nnext"), "100%25 off%0Anext");
    }

    #[test]
    fn escape_property_handles_comma_and_colon() {
        assert_eq!(escape_property("a:b,c"), "a%3Ab%2Cc");
    }

    // ----- Tier → level --------------------------------------------

    #[test]
    fn advisory_posture_total_is_warning() {
        assert_eq!(
            AnnotationLevels::advisory().level_for(Tier::Total),
            AnnotationLevel::Warning
        );
    }

    #[test]
    fn enforcing_posture_total_is_error() {
        assert_eq!(
            AnnotationLevels::enforcing().level_for(Tier::Total),
            AnnotationLevel::Error
        );
    }

    #[test]
    fn high_is_warning_and_advisory_is_notice() {
        let levels = AnnotationLevels::enforcing();
        assert_eq!(levels.level_for(Tier::High), AnnotationLevel::Warning);
        assert_eq!(levels.level_for(Tier::Advisory), AnnotationLevel::Notice);
    }

    // ----- command serialization ------------------------------------

    #[test]
    fn emits_a_well_formed_workflow_command() {
        let findings = vec![uncovered(
            HeuristicId::GrainUniqueKeyUnbacked,
            "model.shop.orders",
        )];
        let emit = emit_annotations(
            &findings,
            AnnotationLevels::enforcing(),
            DEFAULT_ANNOTATION_CAP,
            &always("models/orders.sql", 12),
        );
        assert_eq!(emit.total, 1);
        assert_eq!(emit.overflowed, 0);
        assert_eq!(emit.lines.len(), 1);
        let line = &emit.lines[0];
        assert!(line.starts_with(
            "::error file=models/orders.sql,line=12,title=cute-dbt%3A grain.unique-key-unbacked::"
        ));
        // The recommendation message rides after the final `::`.
        assert!(line.contains("uniqueness data test"));
    }

    // ----- only uncovered findings are annotated -------------------

    #[test]
    fn covered_findings_are_not_annotated() {
        let findings = vec![covered(
            HeuristicId::GrainUniqueKeyUnbacked,
            "model.shop.orders",
        )];
        let emit = emit_annotations(
            &findings,
            AnnotationLevels::advisory(),
            DEFAULT_ANNOTATION_CAP,
            &always("models/orders.sql", 1),
        );
        assert!(emit.lines.is_empty());
        assert_eq!(emit.total, 0);
    }

    #[test]
    fn finding_without_an_anchor_is_summary_only() {
        let findings = vec![uncovered(
            HeuristicId::GrainUniqueKeyUnbacked,
            "model.shop.orders",
        )];
        let emit = emit_annotations(
            &findings,
            AnnotationLevels::advisory(),
            DEFAULT_ANNOTATION_CAP,
            &|_| None, // no anchor resolves
        );
        assert!(emit.lines.is_empty());
        assert_eq!(emit.total, 0);
    }

    // ----- severity ordering + cap ----------------------------------

    #[test]
    fn highest_severity_emits_first() {
        // One High (warning) + one Total (error under enforcing).
        let findings = vec![
            uncovered(HeuristicId::UnionArmCoverage, "model.shop.a"),
            uncovered(HeuristicId::GrainUniqueKeyUnbacked, "model.shop.b"),
        ];
        let emit = emit_annotations(
            &findings,
            AnnotationLevels::enforcing(),
            DEFAULT_ANNOTATION_CAP,
            &always("models/x.sql", 1),
        );
        assert_eq!(emit.lines.len(), 2);
        assert!(emit.lines[0].starts_with("::error "));
        assert!(emit.lines[1].starts_with("::warning "));
    }

    #[test]
    fn cap_truncates_and_emits_an_overflow_notice() {
        let findings: Vec<Finding<HeuristicId>> = (0..5)
            .map(|i| uncovered(HeuristicId::UnionArmCoverage, &format!("model.shop.m{i}")))
            .collect();
        let emit = emit_annotations(
            &findings,
            AnnotationLevels::advisory(),
            2, // cap at 2
            &always("models/x.sql", 1),
        );
        assert_eq!(emit.total, 5);
        assert_eq!(emit.overflowed, 3);
        // 2 capped + 1 overflow notice
        assert_eq!(emit.lines.len(), 3);
        assert!(emit.lines[2].starts_with("::notice::+3 more"));
    }

    #[test]
    fn no_overflow_notice_when_under_cap() {
        let findings = vec![uncovered(HeuristicId::UnionArmCoverage, "model.shop.a")];
        let emit = emit_annotations(
            &findings,
            AnnotationLevels::advisory(),
            DEFAULT_ANNOTATION_CAP,
            &always("models/x.sql", 1),
        );
        assert_eq!(emit.overflowed, 0);
        assert!(emit.lines.iter().all(|l| !l.contains("more uncovered")));
    }

    // ----- summary roll-up ------------------------------------------

    #[test]
    fn summary_tallies_by_tier_and_covered() {
        let findings = vec![
            uncovered(HeuristicId::GrainUniqueKeyUnbacked, "model.shop.a"), // Total
            uncovered(HeuristicId::UnionArmCoverage, "model.shop.b"),       // High
            covered(HeuristicId::GrainUniqueKeyUnbacked, "model.shop.c"),
        ];
        let summary = FindingsSummary::tally(&findings);
        assert_eq!(summary.total_uncovered, 1);
        assert_eq!(summary.high_uncovered, 1);
        assert_eq!(summary.advisory_uncovered, 0);
        assert_eq!(summary.covered, 1);
        assert!(!summary.is_clean());
    }

    #[test]
    fn summary_markdown_clean_run() {
        let summary = FindingsSummary {
            total_uncovered: 0,
            high_uncovered: 0,
            advisory_uncovered: 0,
            covered: 7,
        };
        let md = summary_markdown(&summary, None);
        assert!(md.contains("No uncovered findings"));
        assert!(md.contains("7 construct(s) covered"));
    }

    #[test]
    fn summary_markdown_with_gaps_and_link() {
        let summary = FindingsSummary {
            total_uncovered: 2,
            high_uncovered: 1,
            advisory_uncovered: 0,
            covered: 3,
        };
        let md = summary_markdown(&summary, Some("https://example/report.html"));
        assert!(md.contains("**2 Total** / **1 High** / **0 Advisory** uncovered · 3 covered"));
        assert!(md.contains("[Full report](https://example/report.html)"));
    }

    #[test]
    fn suppressed_uncovered_is_not_annotated_or_counted() {
        let mut f = uncovered(HeuristicId::GrainUniqueKeyUnbacked, "model.shop.a");
        f.suppressed = Some(crate::domain::checks::Suppression {
            source: crate::domain::checks::SuppressionSource::Config,
            reason: Some("known".to_owned()),
        });
        let findings = vec![f];
        let emit = emit_annotations(
            &findings,
            AnnotationLevels::enforcing(),
            DEFAULT_ANNOTATION_CAP,
            &always("models/x.sql", 1),
        );
        assert!(emit.lines.is_empty());
        let summary = FindingsSummary::tally(&findings);
        assert!(summary.is_clean());
    }

    // Keep Evidence import used (some checks emit evidence rows).
    #[test]
    fn evidence_does_not_affect_annotation() {
        let mut f = uncovered(HeuristicId::UnionArmCoverage, "model.shop.a");
        f.evidence = vec![Evidence::new("arm", "left")];
        let emit = emit_annotations(
            &[f],
            AnnotationLevels::advisory(),
            DEFAULT_ANNOTATION_CAP,
            &always("models/x.sql", 3),
        );
        assert_eq!(emit.lines.len(), 1);
    }
}
