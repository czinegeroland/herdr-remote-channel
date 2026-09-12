//! Classification of a parsed command into an outcome.
//!
//! Behavior has not shipped yet, so every command resolves to one of two
//! outcomes: it is still unimplemented, or it crosses the human
//! authorization boundary and can never be completed by a non-interactive
//! caller. Keeping the classification in one place means the boundary of PRD
//! section 22.7 is enforced by a single table that tests can read.

use crate::cli::{
    Command, DeviceAction, HerdrAction, InviteAction, JoinAction, MemberAction, Visibility,
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
    !matches!(command, Command::Review(_) | Command::Approve(_))
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
