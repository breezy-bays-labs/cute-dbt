//! Binary entry. Delegates to [`cute_dbt::cli::run`] and exits with the
//! mapped status.
//!
//! One test-support exception precedes the CLI: when this binary is run
//! under a name other than `cute-dbt` (a copy the `review` subprocess
//! test harness installed as `dbt`/`gh`) beside a sibling
//! `<name>.spec.toml`, it acts as that stand-in tool (cute-dbt#331)
//! instead of the real CLI. The production binary is always invoked as
//! `cute-dbt`, so no production run enters that branch — see
//! [`fake_tool`].

mod fake_tool;

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(spec) = fake_tool::requested() {
        return fake_tool::run(&spec);
    }
    cute_dbt::cli::run()
}
