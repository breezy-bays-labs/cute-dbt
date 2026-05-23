//! `PreflightError` → operator-facing remediation message.
//!
//! ADR-2 keeps remediation wording out of the `domain` enum: the enum is
//! the fail-closed *currency*; the `cli` layer owns how a failure is
//! *presented*. Each variant gets a stderr message naming what is wrong
//! and what to do about it.

use crate::domain::PreflightError;

/// Build the operator-facing stderr message for a fail-closed failure:
/// the error's own description followed by a remediation line.
///
/// The match is exhaustive over the four `PreflightError` variants with
/// no `_` arm — a fifth variant would fail to compile here, forcing a
/// deliberate remediation decision rather than a silent generic message.
///
/// `NotCompiled` destructures `unit_test: Option<String>` (widened in
/// PR C / #30) but the hint text is identical for both shapes: the
/// error's own `Display` already carries the shape-specific description;
/// the hint adds the remediation action which is the same either way.
#[must_use]
pub fn remediation(err: &PreflightError) -> String {
    let hint = match err {
        PreflightError::Unreadable { .. } => {
            "Check that the path exists and points to a dbt manifest.json.".to_owned()
        }
        PreflightError::SchemaUnsupported { minimum, .. } => format!(
            "cute-dbt needs a manifest from dbt 1.8 or newer (schema {minimum}). \
             Re-run with a current dbt."
        ),
        PreflightError::NotCompiled { .. } => {
            "Run `dbt compile` or `dbt run` so the manifest carries compiled SQL, \
             then re-run cute-dbt."
                .to_owned()
        }
        PreflightError::BaselineUnusable { .. } => {
            "Check the --baseline-manifest path; it must be a readable dbt \
             manifest.json to diff against."
                .to_owned()
        }
    };
    format!("{err}\n{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_message_explains_the_read_failure() {
        let msg = remediation(&PreflightError::Unreadable {
            detail: "expected value at line 1 column 1".to_owned(),
        });
        assert!(msg.contains("manifest unreadable"), "{msg}");
        assert!(msg.contains("expected value at line 1 column 1"), "{msg}");
        assert!(msg.contains("manifest.json"), "names the remedy: {msg}");
    }

    #[test]
    fn schema_unsupported_message_states_the_minimum_version() {
        let msg = remediation(&PreflightError::SchemaUnsupported {
            found: "v9".to_owned(),
            minimum: "v12",
        });
        assert!(msg.contains("v9"), "echoes the found version: {msg}");
        assert!(msg.contains("v12"), "states the minimum: {msg}");
        assert!(msg.contains("1.8"), "names the dbt version floor: {msg}");
    }

    #[test]
    fn not_compiled_message_names_the_node_and_recommends_compile_with_unit_test() {
        // With unit_test: Some — both node and test name appear; hint
        // recommends dbt compile / dbt run.
        let msg = remediation(&PreflightError::NotCompiled {
            node_id: "model.shop.stg_orders".to_owned(),
            unit_test: Some("test_dedup".to_owned()),
        });
        assert!(
            msg.contains("model.shop.stg_orders"),
            "names the node: {msg}"
        );
        assert!(msg.contains("test_dedup"), "names the unit test: {msg}");
        assert!(
            msg.contains("dbt compile") && msg.contains("dbt run"),
            "recommends compiling: {msg}"
        );
    }

    #[test]
    fn not_compiled_message_names_the_node_and_recommends_compile_without_unit_test() {
        // With unit_test: None — node appears; "unit test" does NOT appear
        // in the error display (enforced by the domain Display impl);
        // hint still recommends dbt compile / dbt run.
        let msg = remediation(&PreflightError::NotCompiled {
            node_id: "model.shop.stg_orders".to_owned(),
            unit_test: None,
        });
        assert!(
            msg.contains("model.shop.stg_orders"),
            "names the node: {msg}"
        );
        assert!(
            msg.contains("dbt compile") && msg.contains("dbt run"),
            "recommends compiling: {msg}"
        );
        assert!(
            !msg.contains("unit test"),
            "should not mention a unit test when none is in scope: {msg}"
        );
    }

    #[test]
    fn baseline_unusable_message_points_at_the_baseline_flag() {
        let msg = remediation(&PreflightError::BaselineUnusable {
            detail: "no such file".to_owned(),
        });
        assert!(msg.contains("baseline manifest unusable"), "{msg}");
        assert!(msg.contains("no such file"), "{msg}");
        assert!(msg.contains("--baseline-manifest"), "names the flag: {msg}");
    }
}
