//! `mnm` — the short-name alias binary. Defers to the shared
//! [`midnight_manual::cli::run`] entrypoint so both `midnight-manual` and `mnm` install
//! from one library implementation.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match midnight_manual::cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mnm: {err:#}");
            ExitCode::from(1)
        }
    }
}
