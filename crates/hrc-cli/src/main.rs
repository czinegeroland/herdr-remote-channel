//! The single self-contained `hrc` executable.
//!
//! One binary serves the CLI, the daemon, and every Herdr entry point (PRD
//! requirement HRC-TECH-002). Commands that have shipped run here; the rest
//! report the documented not-implemented code and name the milestone that
//! delivers them.

mod cli;
mod commands;
mod dispatch;
mod error;
mod exit;
mod paths;
mod render;

use clap::Parser;
use serde_json::{Value, json};

use crate::cli::{Cli, Command};
use crate::dispatch::Outcome;
use crate::error::CliError;
use crate::paths::Paths;

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

    if matches!(cli.command, Command::Daemon) {
        return run_daemon(&cli, &path);
    }

    match run(&cli, &path) {
        Ok(value) => {
            render::success(cli.json, &path, &value);
            code(exit::SUCCESS)
        }
        Err(Failure::Cli(error)) => {
            report(cli.json, error.code(), &path, &error.to_string(), None);
            code(error.exit_code())
        }
        Err(Failure::NotShipped { milestone }) => {
            let message = format!("`hrc {path}` is not implemented yet; it ships in {milestone}");
            report(cli.json, "unimplemented", &path, &message, Some(milestone));
            code(exit::UNIMPLEMENTED)
        }
        Err(Failure::AuthorizationRequired) => {
            let message = format!(
                "`hrc {path}` crosses the human authorization boundary and must be completed \
                 in the trusted local interface"
            );
            report(cli.json, "authorization_required", &path, &message, None);
            code(exit::AUTHORIZATION_REQUIRED)
        }
    }
}

fn run_daemon(cli: &Cli, path: &str) -> std::process::ExitCode {
    let context = commands::Context::from_environment(match Paths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            report(cli.json, error.code(), path, &error.to_string(), None);
            return code(error.exit_code());
        }
    });

    match commands::daemon_tick(&context) {
        Ok(value) => {
            render::success(cli.json, path, &value);
            let mut stdout = std::io::stdout();
            if let Err(error) = std::io::Write::flush(&mut stdout) {
                report(
                    cli.json,
                    "io_error",
                    path,
                    &format!("could not flush daemon startup output: {error}"),
                    None,
                );
                return code(exit::FAILURE);
            }
        }
        Err(error) => {
            report(cli.json, error.code(), path, &error.to_string(), None);
            return code(error.exit_code());
        }
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            report(
                cli.json,
                "io_error",
                path,
                &format!("could not start the Tokio runtime: {error}"),
                None,
            );
            return code(exit::FAILURE);
        }
    };

    match runtime.block_on(commands::daemon(&context)) {
        Ok(()) => code(exit::SUCCESS),
        Err(error) => {
            report(cli.json, error.code(), path, &error.to_string(), None);
            code(error.exit_code())
        }
    }
}

/// Why a command did not produce a result.
enum Failure {
    /// The command ran and failed.
    Cli(CliError),
    /// The command has no behavior yet.
    NotShipped {
        /// Milestone that delivers it.
        milestone: &'static str,
    },
    /// The command needs the trusted human interface.
    AuthorizationRequired,
}

impl From<CliError> for Failure {
    fn from(error: CliError) -> Self {
        Failure::Cli(error)
    }
}

/// Runs a parsed command.
fn run(cli: &Cli, _path: &str) -> std::result::Result<Value, Failure> {
    // The authorization boundary is checked before anything runs, so a
    // boundary command cannot do work and then be refused.
    if dispatch::requires_trusted_human(&cli.command) {
        return Err(Failure::AuthorizationRequired);
    }

    let context = commands::Context::from_environment(Paths::resolve().map_err(Failure::from)?);

    match &cli.command {
        Command::Init => commands::init(&context).map_err(Failure::from),
        Command::Whoami => commands::whoami(&context).map_err(Failure::from),
        Command::Channels => commands::channels(&context).map_err(Failure::from),
        Command::Status => commands::status(&context).map_err(Failure::from),
        Command::Doctor => commands::doctor(&context).map_err(Failure::from),
        Command::Audit(args) => {
            commands::audit(&context, args.since.as_deref()).map_err(Failure::from)
        }
        Command::Sync(args) if args.once => commands::sync_once(&context).map_err(Failure::from),

        other => match dispatch::classify(other) {
            Outcome::Unimplemented { milestone } => Err(Failure::NotShipped { milestone }),
            Outcome::AuthorizationRequired => Err(Failure::AuthorizationRequired),
        },
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
