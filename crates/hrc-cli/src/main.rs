//! The single self-contained `hrc` executable.
//!
//! One binary serves the CLI, the daemon, and every Herdr entry point (PRD
//! requirement HRC-TECH-002). This milestone ships the published command
//! contract and the human authorization boundary; the behavior behind each
//! command arrives with its roadmap milestone.

mod cli;
mod dispatch;
mod exit;

use clap::Parser;
use serde_json::json;

use crate::cli::Cli;
use crate::dispatch::Outcome;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let path = dispatch::command_path(&cli.command);

    // The trusted interface commands never produce machine-readable output,
    // so reject `--json` before anything else can be printed.
    if cli.json && !dispatch::supports_json(&cli.command) {
        eprintln!(
            "error: `hrc {path}` does not support --json; it runs in the trusted local interface"
        );
        return code(exit::USAGE);
    }

    match dispatch::classify(&cli.command) {
        Outcome::AuthorizationRequired => {
            let message = format!(
                "`hrc {path}` crosses the human authorization boundary and must be completed \
                 in the trusted local interface"
            );
            report(cli.json, "authorization_required", &path, &message, None);
            code(exit::AUTHORIZATION_REQUIRED)
        }
        Outcome::Unimplemented { milestone } => {
            let message = format!("`hrc {path}` is not implemented yet; it ships in {milestone}");
            report(cli.json, "unimplemented", &path, &message, Some(milestone));
            code(exit::UNIMPLEMENTED)
        }
    }
}

/// Writes one error in the requested format.
///
/// The JSON shape is part of the CLI contract: agents and the Herdr plugin
/// read `status` and `code` rather than parsing prose.
fn report(as_json: bool, error_code: &str, path: &str, message: &str, milestone: Option<&str>) {
    if as_json {
        let mut value = json!({
            "status": "error",
            "code": error_code,
            "command": path,
            "message": message,
        });
        if let Some(milestone) = milestone {
            value["milestone"] = json!(milestone);
        }
        println!("{value}");
    } else {
        eprintln!("error: {message}");
    }
}

fn code(value: i32) -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(value).unwrap_or(1))
}
