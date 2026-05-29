//! Binary entry. Delegates to [`cute_dbt::cli::run`] and exits with the
//! mapped status.

use std::process::ExitCode;

fn main() -> ExitCode {
    cute_dbt::cli::run()
}
