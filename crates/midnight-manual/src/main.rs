//! `midnight-manual` — midnight-manual / mnm binary entrypoint.

use std::process::ExitCode;

use midnight_manual::cli;

#[tokio::main]
async fn main() -> ExitCode {
    match cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Errors are formatted as JSON on stdout when --json is set; otherwise
            // a human-readable summary lands on stderr. cli::run handles both.
            eprintln!("mnm: {err:#}");
            ExitCode::from(1)
        }
    }
}
