//! Binary entry. Delegates to [`cute4dbt::cli::run`] and exits with the
//! mapped status.

use std::process::ExitCode;

fn main() -> ExitCode {
    cute4dbt::cli::run()
}
