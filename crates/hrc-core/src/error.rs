//! Errors produced while evaluating channel state.

/// Something went wrong building or advancing channel state.
///
/// Every variant names the specific thing that was wrong. A caller showing a
/// tamper alert needs to say what was detected, and a test asserting
/// fail-closed behavior needs to distinguish "unauthorized" from "malformed".
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// An entry belongs to a different channel.
    #[error("control entry names channel {found}, expected {expected}")]
    WrongChannel {
        /// The channel this roster tracks.
        expected: String,
        /// The channel the entry claimed.
        found: String,
    },

    /// An entry did not directly follow the current head.
    ///
    /// A gap means missing history; a repeat means a competing successor.
    /// Neither may be applied.
    #[error("control sequence {found} does not follow {expected}")]
    SequenceOutOfOrder {
        /// The sequence that was required.
        expected: u64,
        /// The sequence that arrived.
        found: u64,
    },

    /// An entry named the wrong predecessor hash.
    ///
    /// This is the signal for a rewritten or forked control history.
    #[error("control entry names predecessor {found}, expected {expected}")]
    BrokenChain {
        /// The head hash the entry had to name.
        expected: String,
        /// The hash it named instead.
        found: String,
    },

    /// An entry declared an epoch inconsistent with its operation.
    #[error("control entry declares epoch {found}, expected {expected}")]
    EpochOutOfOrder {
        /// The epoch the operation implies.
        expected: u64,
        /// The epoch the entry declared.
        found: u64,
    },

    /// The signer was not an active administrator.
    #[error("principal {principal_id} is not authorized to sign control entries")]
    UnauthorizedSigner {
        /// The rejected principal.
        principal_id: String,
    },

    /// The signing device had already been revoked.
    #[error("device {device_id} was revoked before it signed this entry")]
    RevokedSigner {
        /// The revoked device.
        device_id: String,
    },

    /// A referenced device is not in the roster.
    #[error("device {device_id} is not known to this channel")]
    UnknownDevice {
        /// The unknown device.
        device_id: String,
    },

    /// A referenced principal is not a member.
    #[error("principal {principal_id} is not a member of this channel")]
    UnknownMember {
        /// The unknown principal.
        principal_id: String,
    },

    /// A principal was admitted twice.
    #[error("principal {principal_id} is already a member")]
    DuplicateMember {
        /// The repeated principal.
        principal_id: String,
    },

    /// A device was added twice.
    #[error("device {device_id} is already registered")]
    DuplicateDevice {
        /// The repeated device.
        device_id: String,
    },

    /// A device was revoked twice.
    #[error("device {device_id} is already revoked")]
    AlreadyRevoked {
        /// The device in question.
        device_id: String,
    },

    /// The sole administrator cannot be removed.
    ///
    /// The MVP has one administrator (decision DEC-009); removing them would
    /// leave the channel permanently unable to change its own membership.
    #[error("the sole administrator cannot be removed while one administrator is supported")]
    CannotRemoveAdministrator,

    /// A device certificate named a different principal than the one
    /// vouching for it.
    #[error("device certificate names principal {found}, expected {expected}")]
    CertificatePrincipalMismatch {
        /// The principal that signed.
        expected: String,
        /// The principal the descriptor named.
        found: String,
    },

    /// An admission named an invite the control log never opened.
    #[error("invite {invite_id} was never created in this channel")]
    UnknownInvite {
        /// The unknown invite.
        invite_id: String,
    },

    /// An invite identifier was created twice.
    #[error("invite {invite_id} was already created")]
    DuplicateInvite {
        /// The repeated identifier.
        invite_id: String,
    },

    /// An invite was redeemed more than once.
    ///
    /// Invites are single use (PRD section 15.1), so a second admission
    /// under one is a replayed join request rather than a second guest.
    #[error("invite {invite_id} has already been used")]
    InviteAlreadyUsed {
        /// The spent invite.
        invite_id: String,
    },

    /// An invite was withdrawn before it was redeemed.
    #[error("invite {invite_id} was revoked")]
    InviteRevoked {
        /// The withdrawn invite.
        invite_id: String,
    },

    /// An invite passed its expiry before it was redeemed.
    #[error("invite {invite_id} expired at {expires_at}")]
    InviteExpired {
        /// The expired invite.
        invite_id: String,
        /// When it expired.
        expires_at: String,
    },

    /// A join request was not well formed.
    #[error("malformed join request: {reason}")]
    MalformedJoinRequest {
        /// What was wrong.
        reason: String,
    },

    /// A join request named a principal that is already a member.
    #[error("principal {principal_id} is already a member of this channel")]
    AlreadyEnrolled {
        /// The principal that asked again.
        principal_id: String,
    },

    /// This installation holds no administrator device for the channel.
    ///
    /// Reviewing a join means decrypting a request addressed to the
    /// administrator, so a non-administrator cannot do it even locally.
    #[error("this installation is not an administrator of this channel")]
    NotAnAdministrator,

    /// The operation crosses the section 22.7 human authorization boundary.
    ///
    /// Carries the method name so the CLI can render the stable
    /// `authorization_required` shape of section 22.8 without the daemon and
    /// the CLI keeping separate lists of which operations those are.
    #[error("`{operation}` must be completed in the trusted local interface")]
    AuthorizationRequired {
        /// The refused operation.
        operation: &'static str,
    },

    /// No approved or locally authored content exists for this message.
    /// A repository publication was requested without clearing both gates.
    #[error("refusing to make the repository public: {reason}")]
    PublicationRefused {
        /// Which gate was not cleared.
        reason: String,
    },

    /// A proposed local display name was not one this build will store.
    #[error("refusing to store that display name: {reason}")]
    AliasRefused {
        /// What was wrong with it, in fixed local wording.
        reason: String,
    },

    #[error("no approved content for message {message_id}")]
    NoApprovedContent {
        /// The message asked about.
        message_id: String,
    },

    /// No pending message with this identifier is held locally.
    #[error("no pending message {message_id}")]
    NoPendingMessage {
        /// The message asked about.
        message_id: String,
    },

    /// A receipt reported on a message this installation did not send.
    #[error("receipt references message {message_id}, which this installation did not send")]
    ReceiptForUnknownMessage {
        /// The referenced message.
        message_id: String,
    },

    /// A receipt came from a device the message was never addressed to.
    ///
    /// Refused rather than recorded and discounted: a stored claim tends to
    /// be read as a fact.
    #[error("device {reporter_device} was not a recipient of message {message_id}")]
    ReceiptFromNonRecipient {
        /// The message reported on.
        message_id: String,
        /// The device that reported on it.
        reporter_device: String,
    },

    /// Correlation was attempted on a message that is not an answer.
    #[error("message kind `{kind}` is not an answer")]
    NotAnAnswer {
        /// The kind that was passed.
        kind: String,
    },

    /// An answer could not be tied to a question this installation asked.
    #[error("the answer cannot be correlated: {reason}")]
    UncorrelatedAnswer {
        /// Which check failed.
        reason: &'static str,
    },

    /// A principal reported on a delegation it is not part of.
    #[error("principal {principal_id} is not part of delegation {task_id}")]
    NotADelegationParty {
        /// The principal that reported.
        principal_id: String,
        /// The delegation reported on.
        task_id: String,
    },

    /// A reported state cannot follow the current one.
    #[error("delegation {task_id} cannot move from `{from}` to `{to}`")]
    IllegalDelegationTransition {
        /// The delegation.
        task_id: String,
        /// Where it stands.
        from: &'static str,
        /// What was reported.
        to: &'static str,
    },

    /// The reporter is not the party entitled to report that state.
    #[error("principal {principal_id} may not report `{state}` for delegation {task_id}")]
    WrongDelegationParty {
        /// The delegation.
        task_id: String,
        /// The principal that reported.
        principal_id: String,
        /// The state they claimed.
        state: &'static str,
    },

    /// A message addressed no active device.
    #[error("no active recipient device matches this message's addressing")]
    NoRecipients,

    /// The message plaintext was not well formed.
    #[error("malformed message: {reason}")]
    MalformedMessage {
        /// What was wrong.
        reason: String,
    },

    /// The signed recipient set does not match what the roster implies.
    ///
    /// Either the sender addressed a different audience than the channel's
    /// membership defines, or this device is not among the recipients it
    /// decrypted. Both are HRC-SEC-016 violations.
    #[error("the signed recipient device set does not match the roster")]
    RecipientSetMismatch,

    /// This installation's device is not in the roster.
    #[error("the local device is not a member of this channel")]
    LocalDeviceNotInRoster,

    /// The message expired before it was opened.
    #[error("message expired at {expires_at}")]
    MessageExpired {
        /// The expiry the message declared.
        expires_at: String,
    },

    /// Protected outbox material disagreed with its durable allocation.
    #[error("protected outbox material for message {message_id} changed {field}")]
    OutboxMessageMismatch {
        /// The affected message.
        message_id: String,
        /// Stable field that did not match.
        field: &'static str,
    },

    /// A channel is not registered in local state.
    #[error("channel {channel_id} has no local state")]
    UnknownChannelState {
        /// The channel with no local record.
        channel_id: String,
    },

    /// Synchronization is halted for this channel.
    ///
    /// Sticky by design (PRD sections 17.5.2 and 26). The safe response to
    /// "the record changed underneath me" is never to keep reading, so this
    /// clears only by explicit human action.
    #[error("synchronization is halted: {reason}")]
    SynchronizationHalted {
        /// Why synchronization stopped.
        reason: String,
    },

    /// The transport failed or reported an anomaly.
    #[error("transport: {0}")]
    Transport(String),

    /// Local state could not be read or written.
    #[error(transparent)]
    Storage(#[from] hrc_storage::StorageError),

    /// An authorization does not match the message it was presented with.
    ///
    /// Either it names a different message, or the ciphertext digest has
    /// changed since the human approved it. The second case is the
    /// time-of-check to time-of-use gap the digest binding closes.
    #[error("this authorization does not match the message presented with it")]
    AuthorizationMismatch,

    /// An authorization has already been consumed.
    #[error("this authorization has already been used")]
    AuthorizationAlreadyUsed,

    /// An authorization expired before it was used.
    #[error("this authorization expired at {expires_at}")]
    AuthorizationExpired {
        /// When it expired.
        expires_at: String,
    },

    /// The authorized decision does not deliver content to an agent.
    #[error("the `{decision}` decision does not deliver content to an agent")]
    DecisionDoesNotDeliver {
        /// The decision that was authorized.
        decision: &'static str,
    },

    /// Delivered content does not match the edit the human approved.
    #[error("the content to deliver does not match the edit that was approved")]
    EditedContentMismatch,

    /// A context package did not match its announced digest.
    #[error("the context package does not match its announced digest")]
    ContextDigestMismatch,

    /// A context package contains material that looks like a secret.
    ///
    /// The send is blocked rather than the material stripped: a package
    /// silently reduced to something else is not what the sender approved.
    #[error("the context package contains {count} suspected secret(s); remove them and try again")]
    ContextContainsSecrets {
        /// How many findings the scan produced.
        count: usize,
    },

    /// A context package includes a path excluded by default.
    #[error("`{path}` is excluded from context packages by default")]
    ContextContainsExcludedPath {
        /// The refused path.
        path: String,
    },

    /// A protocol object was malformed.
    #[error(transparent)]
    Protocol(#[from] hrc_protocol::ProtocolError),

    /// A signature or key was invalid.
    #[error(transparent)]
    Crypto(#[from] hrc_crypto::CryptoError),
}

/// Convenience alias for core results.
pub type Result<T> = std::result::Result<T, CoreError>;
