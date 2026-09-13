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

    /// A protocol object was malformed.
    #[error(transparent)]
    Protocol(#[from] hrc_protocol::ProtocolError),

    /// A signature or key was invalid.
    #[error(transparent)]
    Crypto(#[from] hrc_crypto::CryptoError),
}

/// Convenience alias for core results.
pub type Result<T> = std::result::Result<T, CoreError>;
