//! The resident daemon: its loop, its two local endpoints, and the broker
//! that answers the requests arriving on them.
//!
//! Moved out of `commands.rs` by `docs/REFACTOR.md` R5. Everything here runs
//! inside `hrc daemon`; the rest of `commands` is one-shot commands, which
//! share storage and helpers with this but not a lifecycle.

use super::*;

/// Carries a CLI or storage failure across the broker boundary.
///
/// The broker implements `hrc_core::rpc::Broker`, whose errors are
/// `CoreError`, and everything it calls returns `CliError` or
/// `StorageError`. This conversion was written out at thirteen call sites
/// in this module alone.
fn transport(error: impl std::fmt::Display) -> hrc_core::CoreError {
    hrc_core::CoreError::Transport(error.to_string())
}

/// `hrc daemon`: stay resident and keep synchronizing in the background.
pub fn daemon_tick(context: &Context) -> Result<Value> {
    let mut database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let expired = sweep_expired(&database, &now)?;
    let mut channels = Vec::new();
    let mut healthy = true;

    for channel in database.channels()? {
        match sync_git_channel(context, &mut database, &channel, &now) {
            Ok(value) => channels.push(value),
            Err(error) => {
                healthy = false;
                channels.push(json!({
                    "channelId": channel.channel_id,
                    "transport": channel.transport_kind,
                    "locator": channel.transport_locator,
                    "status": "error",
                    "code": error.code(),
                    "message": error.to_string(),
                }));
            }
        }
    }

    let next_poll = daemon_poll_interval(context)?;
    Ok(json!({
        "status": if healthy { "ok" } else { "degraded" },
        "syncedAt": now,
        "nextPollSeconds": next_poll.as_secs(),
        "expired": expired,
        "channels": channels,
    }))
}

/// Runs the resident daemon loop until the process is stopped.
pub async fn daemon(context: &Context, announce: impl FnOnce(Value) -> Result<()>) -> Result<()> {
    // Armed before anything is announced, and before the first
    // synchronization pass runs. `hrc daemon` prints a startup line that a
    // supervisor, a test, or a person reads as "it is up"; if the signal
    // handlers were installed after that line, a SIGTERM arriving in the gap
    // would kill the process outright under the default disposition. The
    // daemon would have reported success and then died to the very signal it
    // advertises handling. Arming first makes the line mean what it says.
    let shutdown = arm_shutdown();
    tokio::pin!(shutdown);

    let agent_endpoint = daemon_endpoint(&context.paths, Interface::AgentSafe)?;
    let trusted_endpoint = daemon_endpoint(&context.paths, Interface::TrustedHuman)?;
    let agent_context = context.clone();
    let trusted_context = context.clone();
    let loop_context = context.clone();
    let context_authorizations =
        Arc::new(Mutex::new(hrc_core::rpc::ContextAuthorizationLedger::new()));
    let trusted_context_authorizations = Arc::clone(&context_authorizations);

    // The first synchronization pass, whose result is the startup line. It
    // runs here rather than before the runtime so that the daemon is already
    // stoppable while it happens: on a channel with a slow remote this pass
    // is the longest part of starting up.
    announce(daemon_tick(context)?)?;

    let served = async {
        tokio::try_join!(
            async {
                serve(&agent_endpoint, move |request: Request| {
                    handle_agent_request(&agent_context, request)
                })
                .await
                .map_err(CliError::from)
            },
            async {
                serve(&trusted_endpoint, move |request: TrustedRequest| {
                    handle_trusted_request(
                        &trusted_context,
                        &trusted_context_authorizations,
                        request,
                    )
                })
                .await
                .map_err(CliError::from)
            },
            daemon_sync_loop(&loop_context),
        )
    };
    tokio::pin!(served);

    let outcome = tokio::select! {
        // Biased so a shutdown that arrives at the same moment as an error
        // is still a shutdown. Reporting a failure caused by tearing the
        // daemon down would make a clean stop look like a crash.
        biased;

        () = &mut shutdown => {
            // The listeners stop accepting when this future is dropped, and
            // an in-flight synchronization tick is synchronous, so it has
            // already finished or has not started.
            Ok(())
        }
        result = &mut served => result.map(|_| ()),
    };

    // Endpoints are removed on the way out rather than left for the next
    // start to probe and reclaim. On Unix a stale socket file makes the next
    // bind do extra work to prove nobody is behind it; on Windows the pipe
    // name simply goes when the process does.
    release_endpoint(&agent_endpoint);
    release_endpoint(&trusted_endpoint);

    outcome
}

/// Resolves when the operating system asks this process to stop.
///
/// Ctrl-C everywhere, and SIGTERM as well on Unix, because that is what a
/// service manager and a container runtime send. A daemon that handled only
/// Ctrl-C would shut down cleanly when a developer stopped it by hand and be
/// killed outright by every automated stop, which is the case that matters.
#[cfg(unix)]
pub(super) fn arm_shutdown() -> impl std::future::Future<Output = ()> {
    use tokio::signal::unix::{SignalKind, signal};

    // Registered here rather than inside the returned future, and this is
    // the whole point of splitting "arm" from "wait": an `async fn` body
    // does not run until something polls it, so a handler installed there
    // would not exist until the select loop first ran. Installing it while
    // building the future means the process stops trusting the default
    // disposition from this line onward.
    //
    // Without SIGTERM handling, Ctrl-C alone is still better than nothing,
    // so a failure to register degrades rather than refusing to start.
    let terminate = signal(SignalKind::terminate()).ok();

    async move {
        match terminate {
            Some(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            None => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
}

#[cfg(not(unix))]
pub(super) fn arm_shutdown() -> impl std::future::Future<Output = ()> {
    async {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Removes a local endpoint's filesystem entry, if it has one.
pub(super) fn release_endpoint(endpoint: &Endpoint) {
    if let Some(path) = endpoint.path() {
        let _ = std::fs::remove_file(path);
    }
}

pub(super) async fn daemon_sync_loop(context: &Context) -> Result<()> {
    loop {
        if let Err(error) = daemon_tick(context) {
            eprintln!("error: {error}");
        }

        let delay = daemon_poll_interval(context)?;
        tokio::time::sleep(delay).await;
    }
}

pub(super) fn daemon_poll_interval(context: &Context) -> Result<Duration> {
    let database = Database::open(context.paths.database())?;
    let mut next = poll_interval(PollActivity::Background, Duration::ZERO);

    for channel in database.channels()? {
        if channel.transport_kind != "git" {
            continue;
        }

        let git_dir = context.paths.channel_transport(&channel.channel_id);
        if let Some(parent) = git_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
                action: "create the local transport directory",
                source,
            })?;
        }

        let transport = GitTransport::open(&git_dir, &channel.transport_locator)?;
        let minimum =
            Duration::from_secs(transport.capabilities().min_poll_interval_seconds as u64);
        next = next.min(poll_interval(PollActivity::Background, minimum));
    }

    Ok(next)
}

pub(super) fn daemon_endpoint(paths: &Paths, interface: Interface) -> Result<Endpoint> {
    let runtime = paths.runtime();
    prepare_runtime_dir(&runtime)?;
    Ok(Endpoint::new(&runtime, interface)?)
}

pub(super) fn handle_agent_request(context: &Context, request: Request) -> Value {
    match request {
        request @ Request::Agent(AgentRequest::Draft { .. })
        | request @ Request::Agent(AgentRequest::Wait { .. }) => json!({
            "status": "error",
            "code": "unimplemented",
            "message": format!(
                "the `{}` agent-safe daemon method is not implemented yet",
                request.method()
            ),
        }),
        other => {
            let mut broker = match DaemonBroker::new(context) {
                Ok(broker) => broker,
                Err(error) => {
                    return json!({
                        "status": "error",
                        "code": error.code(),
                        "message": error.to_string(),
                    });
                }
            };

            match dispatch_agent(&mut broker, other) {
                Ok(response) => json!({
                    "status": "ok",
                    "response": response,
                }),
                Err(error) => json!({
                    "status": "error",
                    "code": core_error_code(&error),
                    "message": error.to_string(),
                }),
            }
        }
    }
}

pub(super) fn handle_trusted_request(
    context: &Context,
    context_authorizations: &Mutex<hrc_core::rpc::ContextAuthorizationLedger>,
    request: TrustedRequest,
) -> Value {
    let method = request.method();

    let mut broker = match DaemonBroker::new(context) {
        Ok(broker) => broker,
        Err(error) => {
            return json!({
                "status": "error",
                "code": error.code(),
                "message": error.to_string(),
            });
        }
    };

    let mut ledger = hrc_core::AuthorizationLedger::new();
    let mut context_ledger = match context_authorizations.lock() {
        Ok(ledger) => ledger,
        Err(error) => {
            return json!({
                "status": "error",
                "code": "internal_error",
                "message": format!("context authorization ledger is unavailable: {error}"),
            });
        }
    };
    let now = match broker.now() {
        Ok(now) => now,
        Err(error) => {
            return json!({
                "status": "error",
                "code": error.code(),
                "message": error.to_string(),
            });
        }
    };

    match hrc_core::rpc::dispatch_trusted(
        &mut broker,
        &mut ledger,
        &mut context_ledger,
        request,
        &now,
    ) {
        Ok(hrc_core::rpc::TrustedResponse::ContextPreview {
            package_id,
            digest,
            content,
            authorization,
            items,
            total_bytes,
        }) => json!({
            "status": "ok",
            "method": method,
            "contextId": package_id,
            "digest": digest,
            "content": content,
            "authorization": authorization,
            "items": items,
            "totalBytes": total_bytes,
        }),
        Ok(hrc_core::rpc::TrustedResponse::Delivered { agent, framed }) => json!({
            "status": "ok",
            "method": method,
            "agent": agent,
            "framed": framed,
        }),
        Ok(hrc_core::rpc::TrustedResponse::Pending { message_id, body }) => json!({
            "status": "ok",
            "method": method,
            "messageId": message_id,
            "body": body,
        }),
        Ok(hrc_core::rpc::TrustedResponse::Done) => json!({
            "status": "ok",
            "method": method,
        }),
        Err(error) => json!({
            "status": "error",
            "code": core_error_code(&error),
            "message": error.to_string(),
        }),
    }
}

pub(super) fn core_error_code(error: &hrc_core::CoreError) -> &'static str {
    match error {
        hrc_core::CoreError::AuthorizationRequired { .. } => "authorization_required",
        _ => "core_error",
    }
}

pub(super) struct DaemonBroker {
    channel_status: Vec<ChannelStatus>,
    channel_names: Vec<String>,
    /// What each channel is called, by channel identifier, in the form every
    /// other surface shows it.
    ///
    /// The provenance banner on an approved delivery names the channel the
    /// body came from. It used to be the literal `local channel` on every
    /// delivery, because nothing here could resolve a message to its channel
    /// — the one seam no test crossed.
    channel_display_names: HashMap<String, String>,
    principal_id: String,
    device_id: String,
    checks: Vec<(String, bool)>,
    audit: Vec<String>,
    /// Pending data reconstructed from the durable, trusted-only quarantine.
    pending: Vec<hrc_core::message::QuarantinedMessage>,
    pending_bodies: HashMap<String, String>,
    /// The section 19.1 metadata set for every stored inbox row.
    ///
    /// Held as `AgentView` rather than as database rows so there is no
    /// column here that could hold a body: the agent-safe inbox answer is
    /// assembled from a type that has no field for one.
    inbox_views: Vec<hrc_core::gate::AgentView>,
    /// Content a human already released, keyed by message.
    ///
    /// Read from the append-only decision record rather than from the inbox
    /// row, because the decision is what a human authorized. A row whose
    /// disposition says `approved` with no decision behind it releases
    /// nothing.
    approved: HashMap<String, String>,
    /// Where the database lives, so a decision can be appended durably
    /// rather than held in memory until the daemon exits.
    database_path: std::path::PathBuf,
    /// Everything a trusted operation needs to publish a control entry.
    ///
    /// The broker performs membership changes itself rather than handing a
    /// plan back to a caller. A trusted operation that returned instructions
    /// would put the decision and its execution in two places, and only one
    /// of them is behind the human interface.
    context: Context,
}

impl DaemonBroker {
    pub(super) fn new(context: &Context) -> Result<Self> {
        let database = Database::open(context.paths.database())?;
        let channel_records = database.channels()?;
        let channel_status = channel_records
            .iter()
            .map(|channel| {
                let counts = database.channel_counts(&channel.channel_id)?;
                Ok(ChannelStatus {
                    local_name: channel.local_name.clone(),
                    roster_epoch: channel.roster_epoch,
                    pending: counts.pending_approval as usize,
                    halted: channel.halted_reason.is_some(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let channel_names = channel_records
            .iter()
            .map(|channel| channel.local_name.clone())
            .collect();
        let channel_display_names = channel_records
            .iter()
            .map(|channel| {
                (
                    channel.channel_id.clone(),
                    hrc_herdr::channel_display_name(&channel.local_name),
                )
            })
            .collect();

        // The principal, not the device's signing key: an agent asking the
        // daemon who it is used to be told the device key under the
        // principal's name (docs/REFACTOR.md R4).
        let identity = local_identity(context)?;
        let principal_id = identity.principal_id;
        let device_id = identity.encryption_recipient;

        let checks = diagnose(context)?
            .checks
            .into_iter()
            .map(|check| (check.name.to_owned(), check.ok))
            .collect();

        let audit = database
            .audit_entries(None)?
            .into_iter()
            .map(|entry| format_audit_entry(&entry))
            .collect();
        let pending_rows = database.pending_inbound()?;
        let pending_bodies = pending_rows
            .iter()
            .map(|row| (row.message_id.clone(), row.body.clone()))
            .collect();
        let pending = pending_rows
            .into_iter()
            .map(|row| {
                Ok(hrc_core::message::QuarantinedMessage {
                    envelope: MessageEnvelope {
                        version: hrc_protocol::PROTOCOL_VERSION,
                        channel_id: row.channel_id,
                        roster_epoch: row.roster_epoch,
                        message_id: row.message_id,
                        device_sequence: 0,
                        previous_chain_id: None,
                        created_at: row.created_at,
                        expires_at: row.expires_at,
                        to: hrc_protocol::Addressing {
                            principals: Vec::new(),
                            endpoint: row.endpoint,
                        },
                        recipients: hrc_protocol::RecipientDevices::new([row
                            .sender_device
                            .clone()])?,
                        recipient_previous_chain_ids: None,
                        thread_id: String::new(),
                        in_reply_to: None,
                        kind: row.kind,
                        requested_capability: None,
                        body: Value::Null,
                        attachments: Vec::new(),
                        padding: String::new(),
                    },
                    sender_principal: row.sender_principal,
                    sender_device: row.sender_device,
                    ciphertext_sha256: row.ciphertext_sha256,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut inbox_views = Vec::new();
        let mut approved = HashMap::new();
        for channel in &channel_records {
            for entry in database.plugin_inbox(&channel.channel_id)? {
                // No local alias store exists yet, so the verified principal
                // ID stands in for the display name, exactly as the plugin
                // pane does.
                inbox_views.push(hrc_herdr::agent_view(
                    &entry,
                    &entry.sender_principal,
                    &channel.local_name,
                ));

                if let Some(text) = released_content(&database, &entry.message_id)? {
                    approved.insert(entry.message_id.clone(), text);
                }
            }
        }

        Ok(Self {
            channel_status,
            channel_names,
            channel_display_names,
            principal_id,
            device_id,
            checks,
            audit,
            pending,
            pending_bodies,
            inbox_views,
            approved,
            database_path: context.paths.database(),
            context: context.clone(),
        })
    }
}

impl DaemonBroker {
    /// The local clock, from the database so every record agrees on it.
    fn now(&self) -> Result<String> {
        Ok(Database::open(&self.database_path)?.utc_now()?)
    }
}

impl Broker for DaemonBroker {
    fn channel_status(&self) -> Vec<ChannelStatus> {
        self.channel_status.clone()
    }

    fn channel_names(&self) -> Vec<String> {
        self.channel_names.clone()
    }

    fn local_identity(&self) -> (String, String) {
        (self.principal_id.clone(), self.device_id.clone())
    }

    fn inbox(&self, pending_only: bool) -> Vec<hrc_core::gate::AgentView> {
        // Built from the same stored row and the same mapping the Herdr
        // plugin uses, so the daemon's answer and the plugin's pane cannot
        // disagree about what section 19.1 admits.
        self.inbox_views
            .iter()
            .filter(|view| !pending_only || view.awaiting_decision)
            .cloned()
            .collect()
    }

    fn approved_content(&self, message_id: &str) -> Option<String> {
        self.approved.get(message_id).cloned()
    }

    fn checks(&self) -> Vec<(String, bool)> {
        self.checks.clone()
    }

    fn audit(&self) -> Vec<String> {
        self.audit.clone()
    }

    fn record_draft(&mut self, _recipient: &str, _text: &str, _endpoint: Option<&str>) -> String {
        "unavailable".into()
    }

    fn observe(&self, _message_id: &str, _until: &str) -> Option<String> {
        None
    }

    fn preview_context(
        &self,
        package_id: &str,
    ) -> hrc_core::Result<hrc_core::rpc::ContextPreviewSummary> {
        let (package, digest, repository_root) =
            load_context_draft(&self.context, package_id).map_err(context_core_error)?;
        let mut preview = package.preview()?;
        append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)
            .map_err(context_core_error)?;
        verify_excerpt_sources(&package, repository_root.as_deref()).map_err(context_core_error)?;
        if !preview.is_sendable() {
            return Err(hrc_core::CoreError::MalformedMessage {
                reason: "context package is blocked by source or secret checks".into(),
            });
        }
        let content =
            String::from_utf8_lossy(&hrc_protocol::canonical::to_canonical_bytes(&package)?)
                .into_owned();
        Ok(hrc_core::rpc::ContextPreviewSummary {
            package_id: package.id,
            digest,
            content,
            items: preview.items,
            total_bytes: preview.total_bytes,
        })
    }

    fn context_authorization_scope(
        &self,
        package_id: &str,
        recipient: &str,
    ) -> hrc_core::Result<hrc_core::rpc::ContextAuthorizationScope> {
        let (_, digest, _) =
            load_context_draft(&self.context, package_id).map_err(context_core_error)?;
        let database = Database::open(&self.database_path).map_err(transport)?;
        let channel = only_channel(&database).map_err(context_core_error)?;
        Ok(hrc_core::rpc::ContextAuthorizationScope {
            channel_id: channel.channel_id,
            package_id: package_id.to_owned(),
            digest,
            recipient: recipient.to_owned(),
            action: "send_context".into(),
        })
    }

    fn context_authorization_expiry(&self, now: &str) -> hrc_core::Result<String> {
        expiry_from(now, "5m").map_err(context_core_error)
    }

    fn send_context(
        &mut self,
        scope: &hrc_core::rpc::ContextAuthorizationScope,
    ) -> hrc_core::Result<()> {
        let (_, digest, _) =
            load_context_draft(&self.context, &scope.package_id).map_err(context_core_error)?;
        if digest != scope.digest {
            return Err(hrc_core::CoreError::AuthorizationMismatch);
        }
        let database = Database::open(&self.database_path).map_err(transport)?;
        if only_channel(&database)
            .map_err(context_core_error)?
            .channel_id
            != scope.channel_id
        {
            return Err(hrc_core::CoreError::AuthorizationMismatch);
        }
        context_send(&self.context, &scope.recipient, &scope.package_id)
            .map(|_| ())
            .map_err(context_core_error)
    }

    fn pending(&self, message_id: &str) -> Option<&hrc_core::message::QuarantinedMessage> {
        self.pending
            .iter()
            .find(|message| message.envelope.message_id == message_id)
    }

    fn pending_body(&self, message_id: &str) -> Option<String> {
        self.pending_bodies.get(message_id).cloned()
    }

    fn channel_local_name(&self, message_id: &str) -> String {
        // Resolved from the quarantined message itself, whose channel was
        // verified when it arrived, rather than from anything a caller
        // supplied. A message this broker does not hold cannot be delivered
        // at all, so the fallback is a fixed local phrase that says so rather
        // than a guess at which channel it might have been.
        self.pending(message_id)
            .and_then(|message| {
                self.channel_display_names
                    .get(&message.envelope.channel_id)
                    .cloned()
            })
            .unwrap_or_else(|| "an unknown channel".to_owned())
    }

    fn local_user(&self) -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "local human".into())
    }

    fn record_decision_record(
        &mut self,
        record: &hrc_core::DecisionRecord,
    ) -> hrc_core::Result<()> {
        // Written through to the append-only audit log rather than kept in
        // memory: a decision that a restart erased would not be evidence of
        // anything (PRD requirement HRC-GATE-004).
        let mut database = Database::open(&self.database_path).map_err(transport)?;

        database
            .commit_decision(&hrc_storage::DecisionRecord {
                channel_id: record.channel_id.clone(),
                message_id: record.message_id.clone(),
                action: record.action.to_owned(),
                original_content: record.original_content.clone(),
                edited_content: record.edited_content.clone(),
                content_hash: record.content_hash.clone(),
                edited_hash: record.edited_hash.clone(),
                agent: record.agent.clone(),
                decided_by: record.decided_by.clone(),
                occurred_at: record.occurred_at.clone(),
            })
            .map_err(transport)?;

        Ok(())
    }

    fn publication_channel_name(&self) -> hrc_core::Result<String> {
        let database = Database::open(self.context.paths.database()).map_err(transport)?;
        let channel = only_channel(&database).map_err(transport)?;

        Ok(channel.local_name)
    }

    fn make_repository_public(
        &mut self,
        confirmed: &hrc_core::visibility::ConfirmedPublication,
    ) -> hrc_core::Result<()> {
        make_repository_public(&self.context, confirmed).map_err(transport)
    }

    fn apply_trusted(&mut self, request: &TrustedRequest) -> hrc_core::Result<()> {
        let operation = match request {
            TrustedRequest::RemoveMember { principal_id } => ControlOperation::RemoveMember {
                principal_id: principal_id.clone(),
            },
            TrustedRequest::RevokeDevice { device_id } => ControlOperation::RevokeDevice {
                device_id: device_id.clone(),
            },
            TrustedRequest::ApproveJoin { request_id } => {
                return admit_join(&self.context, request_id).map_err(transport);
            }

            // Declining a join publishes nothing. A refusal that left a
            // record in the channel would tell everyone who was turned away,
            // which is the administrator's business and not the channel's.
            TrustedRequest::RejectJoin { .. } => return Ok(()),

            // Local only. An alias is what *this* installation calls
            // someone; publishing it would tell the channel what its members
            // think of each other, and would let a name one person chose
            // reach a screen belonging to someone who did not choose it.
            TrustedRequest::SetAlias {
                principal_id,
                display_name,
            } => {
                let database = Database::open(self.context.paths.database()).map_err(transport)?;
                let channel = only_channel(&database).map_err(transport)?;
                let now = database.utc_now().map_err(transport)?;

                match display_name {
                    Some(display_name) => database.set_principal_alias(
                        &channel.channel_id,
                        principal_id,
                        display_name,
                        &now,
                    ),
                    None => database
                        .clear_principal_alias(&channel.channel_id, principal_id)
                        .map(|_| ()),
                }
                .map_err(transport)?;

                return Ok(());
            }

            // The rest are not membership changes and have no control entry.
            _ => return Ok(()),
        };

        publish_membership_change(&self.context, operation)
            .map(|_| ())
            .map_err(transport)
    }
}

/// The trusted daemon boundary already hides detailed context source errors
/// from agent-safe callers. Preserve that boundary when the CLI helpers are
/// called through the trusted RPC implementation.
pub(super) fn context_core_error(error: CliError) -> hrc_core::CoreError {
    hrc_core::CoreError::Transport(error.to_string())
}

pub(super) fn format_audit_entry(entry: &hrc_storage::AuditEntry) -> String {
    let mut line = format!("{} {}", entry.occurred_at, entry.action);
    if let Some(channel_id) = &entry.channel_id {
        line.push_str(&format!(" channel={channel_id}"));
    }
    if let Some(message_id) = &entry.message_id {
        line.push_str(&format!(" message={message_id}"));
    }
    if let Some(detail) = &entry.detail {
        line.push_str(&format!(" detail={detail}"));
    }
    line
}
