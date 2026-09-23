//! The public `hrc` command tree.
//!
//! The shape of this tree is a product contract: PRD section 22 lists the
//! commands, and PRD section 11.1 requires every non-interactive command to
//! support `--json` with stable output shapes and documented exit codes.
//! `hrc review` and `hrc approve` are the deliberate exceptions; they belong
//! to the trusted human surface and must never emit machine-readable output.

use clap::{Args, Parser, Subcommand};

/// Secure asynchronous communication between independent Herdr sessions.
#[derive(Debug, Parser)]
#[command(name = "hrc", version, about, long_about = None)]
pub struct Cli {
    /// Emit machine-readable JSON instead of human formatting.
    ///
    /// Rejected for commands on the trusted human surface.
    #[arg(long, global = true)]
    pub json: bool,

    /// Channel to operate on. Defaults to the configured current channel.
    #[arg(long, global = true, value_name = "CHANNEL")]
    pub channel: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

/// Top-level `hrc` subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create local principal and device identities.
    Init,
    /// Show the local principal and device fingerprints.
    Whoami,

    /// Create a channel backed by a Git repository.
    Create(CreateArgs),
    /// List configured channels.
    Channels,
    /// Show channel, synchronization, and queue state.
    Status,
    /// Diagnose the local installation and channel health.
    Doctor,
    /// Roll the channel over to a fresh repository.
    Rollover,

    /// Manage expiring, single-use invites.
    Invite(InviteCommand),
    /// Redeem an invite, or review pending join requests.
    Join(JoinCommand),
    /// List channel members.
    Members,
    /// Manage a member.
    Member(MemberCommand),
    /// Manage the devices of the local principal.
    Device(DeviceCommand),

    /// Send a note to a recipient.
    Send(SendArgs),
    /// Ask a question, optionally addressed to a logical endpoint.
    Ask(AskArgs),
    /// Reply in the thread of an existing message.
    Reply(ReplyArgs),
    /// Send a structured delegation request. HRC never executes it.
    Delegate(DelegateArgs),
    /// Report the outcome of a task someone delegated to you.
    #[command(name = "result")]
    TaskResult(TaskResultArgs),
    /// Build, preview, and send explicit context packages.
    Context(ContextCommand),
    /// List inbox entries.
    Inbox(InboxArgs),
    /// Show one message. Pending bodies stay quarantined.
    Show(MessageIdArgs),
    /// Show a thread. Pending bodies are redacted.
    Thread(ThreadArgs),
    /// Wait for a message to reach a state.
    Wait(WaitArgs),

    /// Review a pending message in the trusted human interface.
    Review(MessageIdArgs),
    /// Complete an approval through the trusted human interface.
    Approve(MessageIdArgs),

    /// Synchronize with the transport.
    Sync(SyncArgs),
    /// Run the background daemon.
    Daemon,
    /// Show the local audit log.
    Audit(AuditArgs),

    /// Herdr plugin entry points dispatched by this same executable.
    Herdr(HerdrCommand),
}

/// Arguments of `hrc create`.
#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Repository backing the channel, as `owner/name`.
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: String,

    /// Repository visibility. Public creation requires typed confirmation.
    #[arg(long, value_enum, default_value_t = Visibility::Private)]
    pub visibility: Visibility,
}

/// Repository visibility for a channel. Private is the default (PRD 16.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Visibility {
    /// Only invited collaborators can read the ciphertext.
    Private,
    /// Ciphertext, commit timing, and pusher identities are world readable.
    Public,
}

/// `hrc invite ...`
#[derive(Debug, Args)]
pub struct InviteCommand {
    #[command(subcommand)]
    pub action: InviteAction,
}

/// Invite subcommands.
#[derive(Debug, Subcommand)]
pub enum InviteAction {
    /// Create an expiring, single-use invite.
    Create {
        /// GitHub user the invite is intended for.
        #[arg(long, value_name = "NAME")]
        github_user: String,
        /// Lifetime of the invite, for example `24h`.
        #[arg(long, value_name = "DURATION", default_value = "24h")]
        expires: String,
    },
    /// List outstanding invites. Secrets are never printed.
    List,
    /// Revoke an outstanding invite.
    Revoke {
        /// Invite ID to revoke.
        #[arg(allow_hyphen_values = true)]
        id: String,
    },
}

/// `hrc join <invite-code>` and `hrc join pending|approve|reject`.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct JoinCommand {
    /// Invite code to redeem.
    pub invite_code: Option<String>,

    #[command(subcommand)]
    pub action: Option<JoinAction>,
}

/// Join subcommands.
#[derive(Debug, Subcommand)]
pub enum JoinAction {
    /// List join requests awaiting administrator approval.
    Pending,
    /// Approve a join request after comparing the safety phrase.
    Approve {
        /// Join request ID.
        #[arg(allow_hyphen_values = true)]
        id: String,
    },
    /// Reject a join request.
    Reject {
        /// Join request ID.
        #[arg(allow_hyphen_values = true)]
        id: String,
    },
}

/// `hrc member ...`
#[derive(Debug, Args)]
pub struct MemberCommand {
    #[command(subcommand)]
    pub action: MemberAction,
}

/// Member subcommands.
#[derive(Debug, Subcommand)]
pub enum MemberAction {
    /// Remove a member and advance the roster epoch.
    Remove {
        /// Principal ID of the member.
        #[arg(allow_hyphen_values = true)]
        id: String,
    },
    /// Record what this installation calls a member.
    ///
    /// Local only: nothing is published and the member is never told. The
    /// name appears in the inbox, in notifications, and on the approval
    /// screen, in place of the principal ID.
    Name {
        /// Principal ID of the member.
        #[arg(allow_hyphen_values = true)]
        id: String,
        /// The name to show. Omit with `--clear` to forget the one on record.
        #[arg(required_unless_present = "clear")]
        display_name: Option<String>,
        /// Forget the name on record and go back to the principal ID.
        #[arg(long, conflicts_with = "display_name")]
        clear: bool,
    },
}

/// `hrc device ...`
#[derive(Debug, Args)]
pub struct DeviceCommand {
    #[command(subcommand)]
    pub action: DeviceAction,
}

/// Device subcommands.
#[derive(Debug, Subcommand)]
pub enum DeviceAction {
    /// List the devices of the local principal.
    List,
    /// Revoke a device and advance the roster epoch.
    Revoke {
        /// Device ID to revoke.
        id: String,
    },
    /// Rotate the local device keys.
    Rotate,
}

/// Arguments of `hrc send`.
#[derive(Debug, Args)]
pub struct SendArgs {
    /// Recipient principal.
    #[arg(allow_hyphen_values = true)]
    pub recipient: String,
    /// Message text.
    pub message: String,
    /// Lifetime after which the message expires, for example `24h`.
    #[arg(long, value_name = "LIFETIME")]
    pub expires: Option<String>,
}

/// Arguments of `hrc ask`.
#[derive(Debug, Args)]
pub struct AskArgs {
    /// Recipient principal, optionally `principal/endpoint`.
    ///
    /// An endpoint is advisory. The receiver decides whether any content
    /// reaches a local agent.
    #[arg(allow_hyphen_values = true)]
    pub recipient: String,
    /// Question text.
    pub question: String,
    /// Lifetime after which the question expires, for example `24h`.
    #[arg(long, value_name = "LIFETIME")]
    pub expires: Option<String>,
}

/// Arguments of `hrc reply`.
#[derive(Debug, Args)]
pub struct ReplyArgs {
    /// Message being replied to.
    pub message_id: String,
    /// Reply text. Read from standard input when omitted.
    pub message: Option<String>,
}

/// Arguments of `hrc delegate`.
#[derive(Debug, Args)]
pub struct DelegateArgs {
    /// Recipient principal.
    #[arg(allow_hyphen_values = true)]
    pub recipient: String,
    /// What is being asked for.
    ///
    /// Positional, like the message of `hrc send`. Section 22.4 sketches this
    /// command with only `--title`, but `TaskBody` requires a description and
    /// a receiver deciding whether to accept work needs more than a summary
    /// line.
    pub description: String,
    /// Short task title.
    #[arg(long, value_name = "TITLE")]
    pub title: String,
    /// How the requester will judge the result. Repeatable.
    #[arg(long = "criterion", value_name = "TEXT")]
    pub criteria: Vec<String>,
    /// A context package already shared, named by its identifier.
    ///
    /// Naming one discloses nothing: the package travels by `hrc context
    /// send`, which is a trusted operation of its own. This only points at
    /// something the recipient may already hold.
    #[arg(long, value_name = "ID")]
    pub context: Option<String>,
    /// When the requester stops waiting, for example `7d`.
    #[arg(long, value_name = "DURATION")]
    pub due: Option<String>,
}

/// Arguments of `hrc result`.
#[derive(Debug, Args)]
pub struct TaskResultArgs {
    /// The task being reported on, by the message identifier it arrived as.
    pub task_id: String,
    /// What happened, for the person who asked.
    pub summary: String,
    /// Report that the task failed rather than that a result is ready.
    #[arg(long)]
    pub failed: bool,
    /// A commit the result claims exists, so the requester can check it.
    /// Repeatable. Anything `git rev-parse` resolves in the current checkout
    /// is accepted, such as `HEAD`; the full name is what is sent.
    #[arg(long = "commit", value_name = "REV")]
    pub commits: Vec<String>,
    /// A context package already shared, named by its identifier.
    #[arg(long, value_name = "ID")]
    pub context: Option<String>,
}

/// `hrc context ...`
#[derive(Debug, Args)]
pub struct ContextCommand {
    #[command(subcommand)]
    pub action: ContextAction,
}

/// Context-package commands.
#[derive(Debug, Subcommand)]
pub enum ContextAction {
    /// Store an explicit context package described by a JSON manifest.
    Draft {
        /// JSON file containing a ContextPackage and its selected items.
        manifest: String,
        /// Git repository that owns excerpt paths in the package.
        #[arg(long, value_name = "PATH")]
        repository: Option<String>,
    },
    /// Open the trusted human preview for a locally stored context package.
    Preview {
        /// Locally stored context package identifier.
        id: String,
    },
    /// Send a stored context package through the trusted human interface.
    Send {
        /// Recipient principal, optionally `principal/endpoint`.
        #[arg(allow_hyphen_values = true)]
        recipient: String,
        /// Locally stored context package identifier.
        id: String,
    },
}

/// Arguments of `hrc inbox`.
#[derive(Debug, Args)]
pub struct InboxArgs {
    /// Show only unread entries.
    #[arg(long, conflicts_with = "pending")]
    pub unread: bool,
    /// Show only entries awaiting a local approval decision.
    #[arg(long)]
    pub pending: bool,
    /// Never include bodies, even for approved entries.
    #[arg(long)]
    pub metadata_only: bool,
}

/// A single message ID argument.
#[derive(Debug, Args)]
pub struct MessageIdArgs {
    /// Message ID.
    pub message_id: String,
}

/// Arguments of `hrc thread`.
#[derive(Debug, Args)]
pub struct ThreadArgs {
    /// Thread ID.
    pub thread_id: String,
}

/// Arguments of `hrc wait`.
#[derive(Debug, Args)]
pub struct WaitArgs {
    /// Message ID to watch.
    pub message_id: String,
    /// State to wait for, for example `delivered`.
    #[arg(long, value_name = "STATE")]
    pub until: Option<String>,
    /// Give up after this duration, for example `5m`.
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
}

/// Arguments of `hrc sync`.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Run one synchronization pass instead of staying resident.
    #[arg(long)]
    pub once: bool,
}

/// Arguments of `hrc audit`.
#[derive(Debug, Args)]
pub struct AuditArgs {
    /// Only show records at or after this RFC 3339 time.
    #[arg(long, value_name = "TIME")]
    pub since: Option<String>,
}

/// `hrc herdr ...`
#[derive(Debug, Args)]
pub struct HerdrCommand {
    #[command(subcommand)]
    pub action: HerdrAction,
}

/// Herdr integration entry points (PRD section 13.2).
#[derive(Debug, Subcommand)]
pub enum HerdrAction {
    /// Startup entry point registered in the Herdr manifest.
    Startup,
    /// Run a named Herdr action.
    Action {
        /// Action name, for example `inbox`.
        name: String,
    },
    /// Handle a Herdr event named in `HERDR_PLUGIN_EVENT`.
    Event,
    /// Print the `herdr-plugin.toml` this build advertises.
    Manifest,
    /// Render a named Herdr pane.
    Pane {
        /// Pane name, for example `inbox`.
        name: String,
    },
}
