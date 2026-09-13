//! The daemon's two local interfaces.
//!
//! PRD section 19.5 requires the daemon to expose an agent-safe interface and
//! a trusted human interface, and section 22.7 lists the operations that may
//! only happen on the second. The obvious implementation is one request enum
//! with a permission check somewhere in the handler. That is exactly the
//! shape that leaks: a check can be forgotten, a new method can be added on
//! the wrong side of it, and nothing fails until someone reads the code.
//!
//! So the split here is in the types. There are two request enums and two
//! response enums, and [`AgentResponse`] has no variant capable of carrying a
//! body — the same reasoning that produced the agent view in decision
//! DEC-036. An agent-safe caller cannot ask for a pending body because there
//! is no request that means it, and could not receive one because there is no
//! response that holds it.
//!
//! [`Request`] exists only for the wire, where a caller can send anything.
//! Decoding one on the agent-safe surface with [`dispatch_agent`] refuses
//! every trusted operation with the stable `authorization_required` error.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::gate::{AgentView, Authorization, AuthorizationLedger, Decision};
use crate::message::QuarantinedMessage;

/// What an agent-safe caller may ask for.
///
/// Unknown fields are rejected rather than ignored. On this boundary a
/// silently dropped parameter is the worst outcome: a caller that sent
/// `include_bodies` and got a normal answer would reasonably conclude the
/// daemon honored it.
///
/// Everything here is metadata, a draft, or content the human already
/// approved. Note what is absent: there is no request for a pending body, and
/// no flag on any of these that would produce one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AgentRequest {
    /// Channel, synchronization, and queue state.
    Status,
    /// Configured channels.
    Channels,
    /// This device's public identity.
    Whoami,
    /// Inbox entries as the closed metadata set of section 19.1.
    Inbox {
        /// Only entries still awaiting a human decision.
        #[serde(default)]
        pending_only: bool,
    },
    /// Content the human approved, or that this installation wrote.
    ShowApproved {
        /// Which message.
        message_id: String,
    },
    /// Local health checks.
    Doctor,
    /// The local audit log.
    Audit,
    /// Propose a message. Composing is not sending.
    Draft {
        /// Who it is for.
        recipient: String,
        /// The proposed body.
        text: String,
        /// The advisory endpoint, if the caller suggested one.
        #[serde(default)]
        endpoint: Option<String>,
    },
    /// Block until a message reaches a state, or the timeout elapses.
    Wait {
        /// Which message.
        message_id: String,
        /// The state being waited for.
        until: String,
    },
}

/// What only the trusted human interface may ask for.
///
/// This is PRD section 22.7's list, as a type. A request that belongs here
/// and is sent to the agent-safe surface is refused by [`dispatch_agent`]
/// before any handler sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TrustedRequest {
    /// Show an outbound context package to a human before they approve its
    /// disclosure. This request is unavailable on the agent-safe interface.
    PreviewContext {
        /// Recipient principal and optional endpoint selected by the human.
        recipient: String,
        /// Locally stored package.
        package_id: String,
    },
    /// Consume a preview-issued authorization and send its exact snapshot.
    SendContext {
        /// Recipient principal and optional endpoint.
        recipient: String,
        /// Locally stored package.
        package_id: String,
        /// Opaque one-use authorization returned only by trusted preview.
        authorization: String,
    },
    /// Decrypt and display a pending body to the human.
    PreviewPending {
        /// Which message.
        message_id: String,
    },
    /// Record a decision and issue a one-use authorization.
    Approve {
        /// Which message.
        message_id: String,
        /// What the human chose.
        decision: WireDecision,
        /// RFC 3339 UTC expiry of the authorization.
        expires_at: String,
    },
    /// Admit a pending joiner.
    ApproveJoin {
        /// Which request.
        request_id: String,
    },
    /// Refuse a pending joiner.
    RejectJoin {
        /// Which request.
        request_id: String,
    },
    /// Remove a member from the channel.
    RemoveMember {
        /// Which principal.
        principal_id: String,
    },
    /// Revoke one device.
    RevokeDevice {
        /// Which device.
        device_id: String,
    },
    /// Grant a capability to a member.
    GrantCapability {
        /// Which principal.
        principal_id: String,
        /// Which capability.
        capability: String,
    },
    /// Make the backing repository publicly readable.
    ///
    /// Carries the disclosure the trusted interface displayed and the phrase
    /// the human typed, because PRD section 16.4 requires both and a request
    /// that could arrive without them would make the requirement optional.
    ///
    /// `shown` is self-attested: the daemon cannot see a screen. What it buys
    /// is that an interface which did not display the consequences has to
    /// claim it did, in code, rather than simply omit them — an act a
    /// reviewer can see.
    MakeRepositoryPublic {
        /// The consequences the trusted interface displayed.
        shown: Vec<crate::visibility::PublicDisclosure>,
        /// What the human typed.
        typed: String,
    },
    /// Move the channel to a fresh repository.
    Rollover,
}

/// A decision as it crosses the wire.
///
/// Separate from [`Decision`] because this one is deserializable and that one
/// must not be: the trusted interface builds a `Decision` from a human's
/// choice, and nothing should be able to parse one out of input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireDecision {
    /// Deliver the original content to a local agent.
    DeliverToAgent {
        /// The agent the human picked.
        agent: String,
    },
    /// Deliver content the human edited.
    ///
    /// Carries the edited text rather than only its digest, because the
    /// daemon has to deliver that exact text and cannot reconstruct it from
    /// a hash. The digest the authorization binds to is computed here from
    /// this text, so the two cannot disagree.
    DeliverEdited {
        /// The agent the human picked.
        agent: String,
        /// What the human wants delivered.
        text: String,
    },
    /// Leave it in the human inbox.
    KeepInInbox,
    /// Decline it.
    Decline {
        /// An optional reason for the sender.
        #[serde(default)]
        reason: Option<String>,
    },
}

impl From<WireDecision> for Decision {
    fn from(wire: WireDecision) -> Self {
        match wire {
            WireDecision::DeliverToAgent { agent } => Decision::DeliverToAgent { agent },
            WireDecision::DeliverEdited { agent, text } => Decision::DeliverEdited {
                agent,
                edited_sha256: hrc_protocol::canonical::sha256_hex(text.as_bytes()),
            },
            WireDecision::KeepInInbox => Decision::KeepInInbox,
            WireDecision::Decline { reason } => Decision::Decline { reason },
        }
    }
}

/// Anything a local caller can send.
///
/// Untagged so the wire form is one flat object with a `method` field, rather
/// than an envelope that names which surface the caller believes it is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Request {
    /// A request the agent-safe surface accepts.
    Agent(AgentRequest),
    /// A request only the trusted interface accepts.
    Trusted(TrustedRequest),
}

impl Request {
    /// The method name, for audit records and error messages.
    pub fn method(&self) -> &'static str {
        match self {
            Request::Agent(request) => request.method(),
            Request::Trusted(request) => request.method(),
        }
    }

    /// Whether this request crosses the section 22.7 boundary.
    pub fn requires_trusted_surface(&self) -> bool {
        matches!(self, Request::Trusted(_))
    }
}

impl AgentRequest {
    /// The method name.
    pub fn method(&self) -> &'static str {
        match self {
            AgentRequest::Status => "status",
            AgentRequest::Channels => "channels",
            AgentRequest::Whoami => "whoami",
            AgentRequest::Inbox { .. } => "inbox",
            AgentRequest::ShowApproved { .. } => "show_approved",
            AgentRequest::Doctor => "doctor",
            AgentRequest::Audit => "audit",
            AgentRequest::Draft { .. } => "draft",
            AgentRequest::Wait { .. } => "wait",
        }
    }
}

impl TrustedRequest {
    /// The method name.
    pub fn method(&self) -> &'static str {
        match self {
            TrustedRequest::PreviewContext { .. } => "preview_context",
            TrustedRequest::SendContext { .. } => "send_context",
            TrustedRequest::PreviewPending { .. } => "preview_pending",
            TrustedRequest::Approve { .. } => "approve",
            TrustedRequest::ApproveJoin { .. } => "approve_join",
            TrustedRequest::RejectJoin { .. } => "reject_join",
            TrustedRequest::RemoveMember { .. } => "remove_member",
            TrustedRequest::RevokeDevice { .. } => "revoke_device",
            TrustedRequest::GrantCapability { .. } => "grant_capability",
            TrustedRequest::MakeRepositoryPublic { .. } => "make_repository_public",
            TrustedRequest::Rollover => "rollover",
        }
    }
}

/// What an agent-safe caller can be told.
///
/// No variant carries message content that a human has not approved. That is
/// the invariant of PRD section 19.1, and here it is a property of the type
/// rather than of the handlers: there is no field to put a pending body in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AgentResponse {
    /// Channel and queue state.
    Status {
        /// Per-channel summaries.
        channels: Vec<ChannelStatus>,
    },
    /// The configured channels.
    Channels {
        /// Local names.
        names: Vec<String>,
    },
    /// This device's public identity.
    Whoami {
        /// The local principal.
        principal_id: String,
        /// The local device.
        device_id: String,
    },
    /// Inbox metadata, one closed view per entry.
    Inbox {
        /// The entries.
        entries: Vec<AgentView>,
    },
    /// Content the human already approved.
    Approved {
        /// The message it belongs to.
        message_id: String,
        /// The approved text, banner included.
        text: String,
    },
    /// Health checks, each with a stable name.
    Doctor {
        /// Check name and whether it passed.
        checks: Vec<(String, bool)>,
    },
    /// Audit entries, most recent last.
    Audit {
        /// Rendered entries.
        entries: Vec<String>,
    },
    /// A draft was recorded for the human to send.
    Drafted {
        /// The draft's local identifier.
        draft_id: String,
    },
    /// The waited-for state was reached, or the wait timed out.
    Waited {
        /// The state observed.
        state: String,
    },
}

/// Per-channel state, as the agent-safe surface reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelStatus {
    /// Locally chosen channel name.
    pub local_name: String,
    /// Current roster epoch.
    pub roster_epoch: u64,
    /// How many entries await a human decision.
    pub pending: usize,
    /// Whether synchronization has halted for this channel.
    pub halted: bool,
}

/// What the trusted human interface can be told.
///
/// This is where content and authorizations live. Nothing here is reachable
/// from [`dispatch_agent`].
#[derive(Debug, Clone, Serialize)]
pub enum TrustedResponse {
    /// Metadata for a context snapshot shown only on the trusted surface.
    ContextPreview {
        /// Locally stored package.
        package_id: String,
        /// Digest of the complete, source-derived snapshot.
        digest: String,
        /// Exact canonical package content for trusted human review.
        content: String,
        /// Opaque one-use authorization bound to this preview and recipient.
        authorization: String,
        /// One entry for every item, with its serialized contribution.
        items: Vec<(String, usize)>,
        /// Exact serialized package size.
        total_bytes: usize,
    },
    /// The decrypted body, for display to the human only.
    Pending {
        /// The message it belongs to.
        message_id: String,
        /// The quarantined text.
        body: String,
    },
    /// Approved content, framed and handed to the chosen local agent.
    ///
    /// The authorization that permitted this is spent by the time the
    /// variant exists, and it never appears in a response at all: an
    /// authorization a caller could hold is an authorization a caller could
    /// replay, so it lives and dies inside one call.
    Delivered {
        /// The agent the human chose.
        agent: String,
        /// The content, with its provenance banner.
        framed: String,
    },
    /// The operation completed and needs no further action.
    Done,
}

/// Whatever a request the daemon is holding needs in order to answer.
///
/// A trait rather than a concrete type so the surface split can be tested
/// against a fixture, and so the daemon's real state does not have to exist
/// before the boundary it enforces does.
pub trait Broker {
    /// Channel state for the agent-safe surface.
    fn channel_status(&self) -> Vec<ChannelStatus>;
    /// Configured channel names.
    fn channel_names(&self) -> Vec<String>;
    /// This device's public identity.
    fn local_identity(&self) -> (String, String);
    /// Inbox metadata views.
    fn inbox(&self, pending_only: bool) -> Vec<AgentView>;
    /// Approved or locally authored content, if the message has any.
    fn approved_content(&self, message_id: &str) -> Option<String>;
    /// Local health checks.
    fn checks(&self) -> Vec<(String, bool)>;
    /// Audit entries.
    fn audit(&self) -> Vec<String>;
    /// Records a draft and returns its local identifier.
    fn record_draft(&mut self, recipient: &str, text: &str, endpoint: Option<&str>) -> String;
    /// Observes the current state of a message.
    fn observe(&self, message_id: &str, until: &str) -> Option<String>;
    /// Returns the exact source-derived package snapshot for trusted preview.
    fn preview_context(&self, package_id: &str) -> Result<ContextPreviewSummary>;
    /// Returns the scope a one-use outbound context authorization must bind.
    fn context_authorization_scope(
        &self,
        package_id: &str,
        recipient: &str,
    ) -> Result<ContextAuthorizationScope>;
    /// Chooses the short server-controlled expiry for a context preview.
    fn context_authorization_expiry(&self, now: &str) -> Result<String>;
    /// Sends the package after [`dispatch_trusted`] has consumed the
    /// authorization for exactly this package and recipient.
    fn send_context(&mut self, scope: &ContextAuthorizationScope) -> Result<()>;
    /// The quarantined message with this ID, if the daemon holds one.
    fn pending(&self, message_id: &str) -> Option<&QuarantinedMessage>;
    /// The decrypted body of a pending message, for the human's eyes.
    fn pending_body(&self, message_id: &str) -> Option<String>;
    /// The locally chosen channel name, for the provenance banner.
    fn channel_local_name(&self, message_id: &str) -> String;
    /// The local human's name, for the provenance banner.
    fn local_user(&self) -> String;
    /// Appends one decision to the local audit log.
    ///
    /// Taking the record rather than its parts means a caller cannot write a
    /// decision that differs from the one the gate actually made.
    fn record_decision_record(&mut self, record: &crate::gate::DecisionRecord) -> Result<()>;
    /// The locally chosen name of the channel a publication would affect.
    fn publication_channel_name(&self) -> Result<String>;

    /// Publishes the repository, after both gates of section 16.4 cleared.
    ///
    /// Takes the confirmation rather than its parts, so an implementation
    /// cannot be handed something that did not pass the checks: the type has
    /// one constructor and it performs them.
    fn make_repository_public(
        &mut self,
        confirmed: &crate::visibility::ConfirmedPublication,
    ) -> Result<()>;

    /// Applies a trusted membership or repository operation.
    fn apply_trusted(&mut self, request: &TrustedRequest) -> Result<()>;
}

/// Trusted-only data needed to render an outbound context preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPreviewSummary {
    /// Package identifier.
    pub package_id: String,
    /// Digest of the package including source-derived excerpt bytes.
    pub digest: String,
    /// Exact canonical package content for trusted human review.
    pub content: String,
    /// Item kind and byte count.
    pub items: Vec<(String, usize)>,
    /// Exact canonical package size.
    pub total_bytes: usize,
}

/// Values a context authorization is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextAuthorizationScope {
    /// Channel carrying the encrypted message.
    pub channel_id: String,
    /// Package identifier.
    pub package_id: String,
    /// Canonical package digest.
    pub digest: String,
    /// Recipient requested by the human.
    pub recipient: String,
    /// The trusted operation this authorization can perform.
    pub action: String,
}

struct ContextAuthorization {
    scope: ContextAuthorizationScope,
    expires_at: String,
}

impl ContextAuthorization {
    fn issue(scope: ContextAuthorizationScope, expires_at: String) -> Self {
        Self { scope, expires_at }
    }

    fn validate(&self, scope: &ContextAuthorizationScope, now: &str) -> Result<()> {
        if self.scope != *scope || self.scope.action != "send_context" {
            return Err(CoreError::AuthorizationMismatch);
        }
        if self.expires_at.as_str() <= now {
            return Err(CoreError::AuthorizationExpired {
                expires_at: self.expires_at.clone(),
            });
        }
        Ok(())
    }
}

/// Short-lived, one-use outbound context approvals issued by trusted previews.
#[derive(Default)]
pub struct ContextAuthorizationLedger {
    pending: HashMap<String, ContextAuthorization>,
}

impl ContextAuthorizationLedger {
    /// Creates an empty authorization ledger.
    pub fn new() -> Self {
        Self::default()
    }

    fn issue(&mut self, scope: ContextAuthorizationScope, expires_at: String) -> Result<String> {
        let milliseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let token = loop {
            let token = format!("ctxauth_{}", hrc_protocol::ulid::ulid_at(milliseconds)?);
            if !self.pending.contains_key(&token) {
                break token;
            }
        };
        self.pending.insert(
            token.clone(),
            ContextAuthorization::issue(scope, expires_at),
        );
        Ok(token)
    }

    fn consume(&mut self, token: &str, scope: &ContextAuthorizationScope, now: &str) -> Result<()> {
        let authorization = self
            .pending
            .get(token)
            .ok_or(CoreError::AuthorizationMismatch)?;
        authorization.validate(scope, now)?;
        self.pending.remove(token);
        Ok(())
    }
}

/// Answers a request that arrived on the agent-safe surface.
///
/// The signature is the enforcement: this function takes a [`Request`],
/// because that is what can arrive, but it can only return an
/// [`AgentResponse`], so there is nowhere for a pending body to go even if a
/// future handler tried to fetch one.
pub fn dispatch_agent(broker: &mut dyn Broker, request: Request) -> Result<AgentResponse> {
    let request = match request {
        Request::Agent(request) => request,
        Request::Trusted(request) => {
            // Refused before any state is read or written, so a rejected
            // attempt leaves nothing behind and reveals nothing beyond the
            // fact that the operation needs a human.
            return Err(CoreError::AuthorizationRequired {
                operation: request.method(),
            });
        }
    };

    Ok(match request {
        AgentRequest::Status => AgentResponse::Status {
            channels: broker.channel_status(),
        },
        AgentRequest::Channels => AgentResponse::Channels {
            names: broker.channel_names(),
        },
        AgentRequest::Whoami => {
            let (principal_id, device_id) = broker.local_identity();
            AgentResponse::Whoami {
                principal_id,
                device_id,
            }
        }
        AgentRequest::Inbox { pending_only } => AgentResponse::Inbox {
            entries: broker.inbox(pending_only),
        },
        AgentRequest::ShowApproved { message_id } => {
            // A pending message and an unknown one produce the same answer.
            // Distinguishing them would tell an agent that a body exists and
            // is being withheld, which is one bit more than it needs.
            match broker.approved_content(&message_id) {
                Some(text) => AgentResponse::Approved { message_id, text },
                None => {
                    return Err(CoreError::NoApprovedContent { message_id });
                }
            }
        }
        AgentRequest::Doctor => AgentResponse::Doctor {
            checks: broker.checks(),
        },
        AgentRequest::Audit => AgentResponse::Audit {
            entries: broker.audit(),
        },
        AgentRequest::Draft {
            recipient,
            text,
            endpoint,
        } => AgentResponse::Drafted {
            draft_id: broker.record_draft(&recipient, &text, endpoint.as_deref()),
        },
        AgentRequest::Wait { message_id, until } => AgentResponse::Waited {
            state: broker
                .observe(&message_id, &until)
                .unwrap_or_else(|| "timeout".to_owned()),
        },
    })
}

/// Answers a request that arrived on the trusted human interface.
///
/// Takes a [`TrustedRequest`] rather than a [`Request`], so the trusted
/// listener has to have decided that is what it received. Agent-safe methods
/// are answered by [`dispatch_agent`]; there is no path where one call site
/// serves both and picks based on a flag.
pub fn dispatch_trusted(
    broker: &mut dyn Broker,
    ledger: &mut AuthorizationLedger,
    context_ledger: &mut ContextAuthorizationLedger,
    request: TrustedRequest,
    now: &str,
) -> Result<TrustedResponse> {
    Ok(match request {
        // PRD requirement HRC-CH-004 and section 16.4. Checked here rather
        // than in the interface, so a second interface — the Herdr plugin,
        // a future one — cannot ship its own weaker version of the gate.
        TrustedRequest::MakeRepositoryPublic { shown, typed } => {
            let channel_local_name = broker.publication_channel_name()?;
            let confirmed = crate::visibility::ConfirmedPublication::new(
                &channel_local_name,
                &shown,
                &typed,
                &broker.local_user(),
            )
            .map_err(|refusal| match refusal {
                crate::visibility::RefusedPublication::DisclosureIncomplete { missing } => {
                    CoreError::PublicationRefused {
                        reason: format!(
                            "{} of the section 16.4 consequences were not shown",
                            missing.len()
                        ),
                    }
                }
                crate::visibility::RefusedPublication::PhraseNotTyped { expected } => {
                    CoreError::PublicationRefused {
                        reason: format!("the confirmation phrase `{expected}` was not typed"),
                    }
                }
            })?;

            broker.make_repository_public(&confirmed)?;
            TrustedResponse::Done
        }

        TrustedRequest::PreviewContext {
            recipient,
            package_id,
        } => {
            let preview = broker.preview_context(&package_id)?;
            let scope = broker.context_authorization_scope(&package_id, &recipient)?;
            let expires_at = broker.context_authorization_expiry(now)?;
            let authorization = context_ledger.issue(scope, expires_at)?;
            TrustedResponse::ContextPreview {
                package_id: preview.package_id,
                digest: preview.digest,
                content: preview.content,
                authorization,
                items: preview.items,
                total_bytes: preview.total_bytes,
            }
        }

        TrustedRequest::SendContext {
            recipient,
            package_id,
            authorization,
        } => {
            let scope = broker.context_authorization_scope(&package_id, &recipient)?;
            context_ledger.consume(&authorization, &scope, now)?;
            broker.send_context(&scope)?;
            TrustedResponse::Done
        }

        TrustedRequest::PreviewPending { message_id } => {
            let body =
                broker
                    .pending_body(&message_id)
                    .ok_or_else(|| CoreError::NoPendingMessage {
                        message_id: message_id.clone(),
                    })?;
            TrustedResponse::Pending { message_id, body }
        }

        TrustedRequest::Approve {
            message_id,
            decision,
            expires_at,
        } => {
            let message =
                broker
                    .pending(&message_id)
                    .ok_or_else(|| CoreError::NoPendingMessage {
                        message_id: message_id.clone(),
                    })?;

            // Issuing from the quarantined message itself binds the
            // authorization to the ciphertext digest the human was shown,
            // rather than to identifiers a caller supplied.
            let edited = match &decision {
                WireDecision::DeliverEdited { text, .. } => Some(text.clone()),
                _ => None,
            };
            let decision = Decision::from(decision);
            let authorization = Authorization::issue(message, decision.clone(), expires_at);

            if !decision.reaches_an_agent() {
                // Keeping something in the inbox, declining it, or letting
                // it expire are decisions too. An audit that recorded only
                // approvals would show a channel in which nothing was ever
                // refused.
                let original = broker.pending_body(&message_id).unwrap_or_default();
                let decided_by = broker.local_user();
                let record =
                    crate::gate::record_decision(message, &decision, &original, &decided_by, now);
                broker.record_decision_record(&record)?;
                return Ok(TrustedResponse::Done);
            }

            let agent = match &decision {
                Decision::DeliverToAgent { agent } | Decision::DeliverEdited { agent, .. } => {
                    agent.clone()
                }
                _ => unreachable!("checked by reaches_an_agent"),
            };

            // Everything below has to be read before the borrow of `broker`
            // for delivery, and the content is the body the human approved.
            // The original is what arrived; the body is what the human
            // chose to deliver. The audit preserves both, so a reviewer can
            // see what was removed.
            let original =
                broker
                    .pending_body(&message_id)
                    .ok_or_else(|| CoreError::NoPendingMessage {
                        message_id: message_id.clone(),
                    })?;
            let body = edited.unwrap_or_else(|| original.clone());
            let channel_local_name = broker.channel_local_name(&message_id);
            let local_user = broker.local_user();
            let message =
                broker
                    .pending(&message_id)
                    .ok_or_else(|| CoreError::NoPendingMessage {
                        message_id: message_id.clone(),
                    })?;

            // Consumes the authorization. It is never returned, so there is
            // nothing for a later caller to replay.
            let delivered = crate::gate::deliver(
                authorization,
                ledger,
                message,
                crate::gate::Approval {
                    body: &body,
                    original: &original,
                    channel_local_name: &channel_local_name,
                    approved_by: &local_user,
                    now,
                },
            )?;

            // Recorded before the content is handed over. An approval that
            // reached an agent without leaving a trace is the one an audit
            // exists to catch.
            broker.record_decision_record(&delivered.record)?;

            TrustedResponse::Delivered {
                agent,
                framed: delivered.framed,
            }
        }

        other => {
            broker.apply_trusted(&other)?;
            TrustedResponse::Done
        }
    })
}

#[cfg(test)]
mod tests;
