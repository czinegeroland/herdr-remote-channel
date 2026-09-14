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

    // The startup line is printed by `commands::daemon`, once the shutdown
    // signals are armed. Printing it here, before the runtime existed, meant
    // the daemon announced itself during a window in which a SIGTERM would
    // still kill it under the default disposition — so the line claimed a
    // daemon that was up and stoppable before it was either.
    let announce = |value: Value| -> std::result::Result<(), CliError> {
        render::success(cli.json, path, &value);
        std::io::Write::flush(&mut std::io::stdout()).map_err(|source| CliError::Io {
            action: "flush the daemon startup output",
            source,
        })?;
        Ok(())
    };

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

    match runtime.block_on(commands::daemon(&context, announce)) {
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
    // `hrc review` is the trusted human interface rather than a caller of
    // it, so it is the one boundary command that can run here — and only
    // with a person at the terminal. PRD section 22.7 refuses *direct
    // non-interactive invocation*, which is the distinction: a pipe, a
    // captured subprocess and an agent's tool call are all refused, and the
    // refusal `commands::review` returns carries the same stable
    // `authorization_required` code and exit status as the check below, so
    // nothing scripting this can tell the two apart.
    //
    // `--json` never reaches here at all; it is rejected before dispatch.
    if let Command::Review(args) = &cli.command {
        let context = commands::Context::from_environment(Paths::resolve().map_err(Failure::from)?);
        let agent = commands::review::local_agent();
        return commands::review::review(&context, Some(&args.message_id), &agent)
            .map_err(Failure::from);
    }

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
        Command::Create(args) => {
            // Public creation is refused earlier, on the section 22.7
            // boundary, so anything reaching here is a private channel.
            commands::create(&context, &args.repo, None).map_err(Failure::from)
        }
        Command::Members => commands::members(&context).map_err(Failure::from),
        Command::Device(device) if matches!(device.action, cli::DeviceAction::List) => {
            commands::device_list(&context).map_err(Failure::from)
        }
        Command::Send(args) => commands::send(
            &context,
            &args.recipient,
            &args.message,
            args.expires.as_deref(),
        )
        .map_err(Failure::from),
        Command::Ask(args) => commands::ask(
            &context,
            &args.recipient,
            &args.question,
            args.expires.as_deref(),
        )
        .map_err(Failure::from),
        Command::Reply(args) => commands::reply(
            &context,
            &args.message_id,
            args.message.as_deref().unwrap_or_default(),
        )
        .map_err(Failure::from),
        Command::Context(args) => match &args.action {
            cli::ContextAction::Draft {
                manifest,
                repository,
            } => commands::context_draft(&context, manifest, repository.as_deref())
                .map_err(Failure::from),
            cli::ContextAction::Preview { id } => {
                commands::context_preview(&context, id).map_err(Failure::from)
            }
            cli::ContextAction::Send { recipient, id } => {
                commands::context_send(&context, recipient, id).map_err(Failure::from)
            }
        },
        Command::Inbox(args) => commands::inbox(&context, args.pending).map_err(Failure::from),
        Command::Thread(args) => commands::thread(&context, &args.thread_id).map_err(Failure::from),
        Command::Join(join) => match (&join.invite_code, &join.action) {
            (Some(code), _) => commands::join(&context, code).map_err(Failure::from),
            (None, Some(cli::JoinAction::Pending)) => {
                commands::join_pending(&context).map_err(Failure::from)
            }
            _ => match dispatch::classify(&cli.command) {
                Outcome::Unimplemented { milestone } => Err(Failure::NotShipped { milestone }),
                Outcome::AuthorizationRequired => Err(Failure::AuthorizationRequired),
            },
        },
        Command::Invite(invite) => match &invite.action {
            cli::InviteAction::Create {
                github_user,
                expires,
            } => commands::invite_create(&context, github_user, expires).map_err(Failure::from),
            cli::InviteAction::List => commands::invite_list(&context).map_err(Failure::from),
            cli::InviteAction::Revoke { id } => {
                commands::invite_revoke(&context, id).map_err(Failure::from)
            }
        },
        Command::Sync(args) if args.once => commands::sync_once(&context).map_err(Failure::from),
        Command::Herdr(herdr) => match &herdr.action {
            cli::HerdrAction::Startup => commands::herdr_startup(&context).map_err(Failure::from),
            cli::HerdrAction::Action { name } => {
                commands::herdr_action(&context, name).map_err(Failure::from)
            }
            cli::HerdrAction::Event => {
                // Herdr names the event in an environment variable rather
                // than writing it to standard input. Reading it here instead
                // of in `commands` keeps the command function testable
                // without an environment to set up.
                let event = std::env::var(hrc_herdr::event::EVENT_VARIABLE).ok();

                commands::herdr_event(&context, event.as_deref()).map_err(Failure::from)
            }
            cli::HerdrAction::Manifest => commands::herdr_manifest().map_err(Failure::from),
            cli::HerdrAction::Pane { name } => {
                commands::herdr_pane(&context, name).map_err(Failure::from)
            }
        },

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
