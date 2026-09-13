//! Classification of a parsed command into an outcome.
//!
//! Behavior has not shipped yet, so every command resolves to one of two
//! outcomes: it is still unimplemented, or it crosses the human
//! authorization boundary and can never be completed by a non-interactive
//! caller. Keeping the classification in one place means the boundary of PRD
//! section 22.7 is enforced by a single table that tests can read.

use crate::cli::{
    Command, ContextAction, ContextCommand, DeviceAction, HerdrAction, InviteAction, JoinAction,
    MemberAction, Visibility,
};

/// What the process should do with a parsed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The command is part of the published contract but does not act yet.
    Unimplemented {
        /// Milestone that will deliver the behavior.
        milestone: &'static str,
    },
    /// The command must be completed in the trusted local human interface.
    AuthorizationRequired,
}

/// Commands that may never be completed by an agent-safe or non-interactive
/// caller, per PRD section 22.7.
///
/// `hrc create --visibility public` is included because making a repository
/// public is on the same list.
pub fn requires_trusted_human(command: &Command) -> bool {
    match command {
        Command::Review(_) | Command::Approve(_) | Command::Rollover => true,
        Command::Context(context) => !matches!(context.action, ContextAction::Draft { .. }),
        Command::Create(args) => args.visibility == Visibility::Public,
        Command::Join(join) => matches!(
            join.action,
            Some(JoinAction::Approve { .. } | JoinAction::Reject { .. })
        ),
        Command::Member(member) => matches!(member.action, MemberAction::Remove { .. }),
        Command::Device(device) => matches!(device.action, DeviceAction::Revoke { .. }),
        _ => false,
    }
}

/// Commands that may not emit machine-readable output at all.
///
/// PRD section 22.5: `hrc review` and `hrc approve` drive the trusted human
/// interface and must not print pending plaintext to standard output.
pub fn supports_json(command: &Command) -> bool {
    if let Command::Invite(invite) = command {
        // The whole output of `invite create` is a secret. Machine-readable
        // output is the form most likely to end up in a transcript, a log,
        // or an agent's context, so the command has no JSON mode at all
        // rather than a filtered one (decision DEC-047).
        return !matches!(invite.action, InviteAction::Create { .. });
    }

    !matches!(
        command,
        Command::Review(_)
            | Command::Approve(_)
            | Command::Context(ContextCommand {
                action: ContextAction::Preview { .. } | ContextAction::Send { .. },
            })
    )
}

/// Resolves a command to its outcome.
pub fn classify(command: &Command) -> Outcome {
    if requires_trusted_human(command) {
        return Outcome::AuthorizationRequired;
    }

    Outcome::Unimplemented {
        milestone: milestone(command),
    }
}

/// The roadmap milestone (PRD section 30) that delivers a command.
fn milestone(command: &Command) -> &'static str {
    match command {
        Command::Init
        | Command::Whoami
        | Command::Create(_)
        | Command::Channels
        | Command::Invite(_)
        | Command::Join(_)
        | Command::Members
        | Command::Member(_)
        | Command::Device(_) => "M1",

        Command::Status
        | Command::Doctor
        | Command::Send(_)
        | Command::Ask(_)
        | Command::Reply(_)
        | Command::Inbox(_)
        | Command::Show(_)
        | Command::Thread(_)
        | Command::Wait(_)
        | Command::Sync(_)
        | Command::Daemon
        | Command::Audit(_) => "M2",

        Command::Review(_) | Command::Approve(_) | Command::Herdr(_) => "M3",

        Command::Delegate(_) | Command::Rollover => "M4",
        Command::Context(_) => "M4",
    }
}

/// A stable dotted path naming the command, used in messages and JSON.
pub fn command_path(command: &Command) -> String {
    match command {
        Command::Init => "init".into(),
        Command::Whoami => "whoami".into(),
        Command::Create(_) => "create".into(),
        Command::Channels => "channels".into(),
        Command::Status => "status".into(),
        Command::Doctor => "doctor".into(),
        Command::Rollover => "rollover".into(),
        Command::Invite(invite) => match invite.action {
            InviteAction::Create { .. } => "invite create".into(),
            InviteAction::List => "invite list".into(),
            InviteAction::Revoke { .. } => "invite revoke".into(),
        },
        Command::Join(join) => match join.action {
            Some(JoinAction::Pending) => "join pending".into(),
            Some(JoinAction::Approve { .. }) => "join approve".into(),
            Some(JoinAction::Reject { .. }) => "join reject".into(),
            None => "join".into(),
        },
        Command::Members => "members".into(),
        Command::Member(member) => match member.action {
            MemberAction::Remove { .. } => "member remove".into(),
        },
        Command::Device(device) => match device.action {
            DeviceAction::List => "device list".into(),
            DeviceAction::Revoke { .. } => "device revoke".into(),
            DeviceAction::Rotate => "device rotate".into(),
        },
        Command::Send(_) => "send".into(),
        Command::Ask(_) => "ask".into(),
        Command::Reply(_) => "reply".into(),
        Command::Delegate(_) => "delegate".into(),
        Command::Context(context) => match context.action {
            ContextAction::Draft { .. } => "context draft".into(),
            ContextAction::Preview { .. } => "context preview".into(),
            ContextAction::Send { .. } => "context send".into(),
        },
        Command::Inbox(_) => "inbox".into(),
        Command::Show(_) => "show".into(),
        Command::Thread(_) => "thread".into(),
        Command::Wait(_) => "wait".into(),
        Command::Review(_) => "review".into(),
        Command::Approve(_) => "approve".into(),
        Command::Sync(_) => "sync".into(),
        Command::Daemon => "daemon".into(),
        Command::Audit(_) => "audit".into(),
        Command::Herdr(herdr) => match herdr.action {
            HerdrAction::Startup => "herdr startup".into(),
            HerdrAction::Action { .. } => "herdr action".into(),
            HerdrAction::Event => "herdr event".into(),
            HerdrAction::Pane { .. } => "herdr pane".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::cli::Cli;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("arguments should parse")
    }

    #[test]
    fn trusted_human_commands_are_recognized() {
        let trusted = [
            vec!["hrc", "review", "01ABC"],
            vec!["hrc", "approve", "01ABC"],
            vec!["hrc", "rollover"],
            vec!["hrc", "create", "--repo", "o/n", "--visibility", "public"],
            vec!["hrc", "join", "approve", "req-1"],
            vec!["hrc", "join", "reject", "req-1"],
            vec!["hrc", "member", "remove", "principal-1"],
            vec!["hrc", "device", "revoke", "device-1"],
        ];

        for args in trusted {
            let cli = parse(&args);
            assert!(
                requires_trusted_human(&cli.command),
                "{args:?} should require trusted human authorization"
            );
            assert_eq!(classify(&cli.command), Outcome::AuthorizationRequired);
        }
    }

    #[test]
    fn agent_safe_commands_do_not_require_authorization() {
        let agent_safe = [
            vec!["hrc", "init"],
            vec!["hrc", "whoami"],
            vec!["hrc", "create", "--repo", "o/n"],
            vec!["hrc", "channels"],
            vec!["hrc", "status"],
            vec!["hrc", "doctor"],
            vec!["hrc", "invite", "list"],
            vec!["hrc", "join", "pending"],
            vec!["hrc", "members"],
            vec!["hrc", "device", "list"],
            vec!["hrc", "device", "rotate"],
            vec!["hrc", "inbox", "--pending"],
            vec!["hrc", "show", "01ABC"],
            vec!["hrc", "thread", "01ABC"],
        ];

        for args in agent_safe {
            let cli = parse(&args);
            assert!(
                !requires_trusted_human(&cli.command),
                "{args:?} should not require trusted human authorization"
            );
        }
    }

    #[test]
    fn create_defaults_to_private() {
        let cli = parse(&["hrc", "create", "--repo", "owner/channel"]);
        assert!(!requires_trusted_human(&cli.command));
        assert_eq!(command_path(&cli.command), "create");
    }

    #[test]
    fn only_the_trusted_interface_commands_refuse_json() {
        assert!(!supports_json(&parse(&["hrc", "review", "01ABC"]).command));
        assert!(!supports_json(&parse(&["hrc", "approve", "01ABC"]).command));
        assert!(!supports_json(
            &parse(&["hrc", "context", "preview", "ctx-1"]).command
        ));
        assert!(!supports_json(
            &parse(&["hrc", "context", "send", "alice", "ctx-1"]).command
        ));
        assert!(supports_json(&parse(&["hrc", "inbox"]).command));
        assert!(supports_json(
            &parse(&["hrc", "join", "approve", "r"]).command
        ));
    }

    #[test]
    fn command_paths_are_stable_and_distinct() {
        let paths = [
            (
                vec!["hrc", "invite", "create", "--github-user", "alice"],
                "invite create",
            ),
            (vec!["hrc", "invite", "revoke", "i-1"], "invite revoke"),
            (vec!["hrc", "join", "code-1"], "join"),
            (vec!["hrc", "join", "pending"], "join pending"),
            (vec!["hrc", "member", "remove", "p-1"], "member remove"),
            (vec!["hrc", "device", "rotate"], "device rotate"),
            (vec!["hrc", "herdr", "startup"], "herdr startup"),
            (vec!["hrc", "herdr", "pane", "inbox"], "herdr pane"),
            (vec!["hrc", "sync", "--once"], "sync"),
        ];

        for (args, expected) in paths {
            assert_eq!(command_path(&parse(&args).command), expected);
        }
    }

    #[test]
    fn every_command_has_a_roadmap_milestone() {
        let commands = [
            vec!["hrc", "init"],
            vec!["hrc", "send", "alice", "hello"],
            vec!["hrc", "herdr", "event"],
            vec!["hrc", "delegate", "alice", "--title", "t"],
        ];

        for args in commands {
            let cli = parse(&args);
            let Outcome::Unimplemented { milestone } = classify(&cli.command) else {
                panic!("{args:?} should be unimplemented");
            };
            assert!(
                milestone.starts_with('M'),
                "unexpected milestone {milestone}"
            );
        }
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use hrc_core::rpc::TrustedRequest;

    /// Parses a command line into a command.
    fn command(args: &[&str]) -> Command {
        use clap::Parser as _;
        crate::cli::Cli::try_parse_from(args).unwrap().command
    }

    #[test]
    fn every_daemon_trusted_method_has_a_refusing_cli_command() {
        // The daemon and the CLI each enforce PRD section 22.7, and two
        // lists of the same rule drift. This is the join between them: an
        // operation the daemon reserves for a human must also be one the CLI
        // refuses to complete non-interactively.
        //
        // The mapping is not one to one — `preview_pending` and `approve`
        // are both reached through `hrc review` — so it is written out
        // rather than derived, and a new trusted method fails to compile
        // here until someone decides which command reaches it.
        let mapping: &[(&str, &[&str])] = &[
            ("preview_context", &["hrc", "context", "preview", "ctx-1"]),
            (
                "send_context",
                &["hrc", "context", "send", "alice", "ctx-1"],
            ),
            ("preview_pending", &["hrc", "review", "01ARZ3"]),
            ("approve", &["hrc", "approve", "01ARZ3"]),
            ("approve_join", &["hrc", "join", "approve", "join-1"]),
            ("reject_join", &["hrc", "join", "reject", "join-1"]),
            ("remove_member", &["hrc", "member", "remove", "alice"]),
            ("revoke_device", &["hrc", "device", "revoke", "device-1"]),
            (
                "make_repository_public",
                &[
                    "hrc",
                    "create",
                    "--repo",
                    "owner/name",
                    "--visibility",
                    "public",
                ],
            ),
            ("rollover", &["hrc", "rollover"]),
        ];

        for (method, args) in mapping {
            assert!(
                requires_trusted_human(&command(args)),
                "the daemon reserves `{method}` for a human but the CLI would run {args:?}"
            );
        }

        // Everything the daemon reserves is either mapped above or listed
        // here as having no CLI surface yet.
        let daemon_only = ["grant_capability"];
        let every_method = [
            TrustedRequest::PreviewContext {
                recipient: String::new(),
                package_id: String::new(),
            },
            TrustedRequest::SendContext {
                recipient: String::new(),
                package_id: String::new(),
                authorization: String::new(),
            },
            TrustedRequest::PreviewPending {
                message_id: String::new(),
            },
            TrustedRequest::Approve {
                message_id: String::new(),
                decision: hrc_core::rpc::WireDecision::KeepInInbox,
                expires_at: String::new(),
            },
            TrustedRequest::ApproveJoin {
                request_id: String::new(),
            },
            TrustedRequest::RejectJoin {
                request_id: String::new(),
            },
            TrustedRequest::RemoveMember {
                principal_id: String::new(),
            },
            TrustedRequest::RevokeDevice {
                device_id: String::new(),
            },
            TrustedRequest::GrantCapability {
                principal_id: String::new(),
                capability: String::new(),
            },
            TrustedRequest::MakeRepositoryPublic,
            TrustedRequest::Rollover,
        ];

        for request in every_method {
            let method = request.method();
            assert!(
                mapping.iter().any(|(mapped, _)| *mapped == method)
                    || daemon_only.contains(&method),
                "`{method}` is reserved by the daemon but reaches no CLI command"
            );
        }
    }
}
