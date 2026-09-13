//! Errors produced while encoding, decoding, or validating protocol objects.

/// Something went wrong handling a protocol object.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// A value could not be canonicalized under RFC 8785.
    ///
    /// JCS cannot represent non-finite numbers or non-string object keys, so
    /// this is reachable for otherwise valid Rust values.
    #[error("value cannot be canonicalized as RFC 8785 JSON: {0}")]
    Canonicalization(#[source] serde_json::Error),

    /// JSON did not parse, or did not match the expected shape.
    #[error("malformed protocol JSON: {0}")]
    Json(#[source] serde_json::Error),

    /// A JSON object repeated a key.
    ///
    /// Duplicate keys let a signer and a verifier read different values from
    /// the same bytes, so they are rejected before anything else looks at the
    /// object (PRD section 18.0, validation step 4).
    #[error("protocol JSON contains the duplicate key `{key}`")]
    DuplicateKey {
        /// The key that appeared more than once.
        key: String,
    },

    /// A base64url field was not valid unpadded base64url.
    #[error("field `{field}` is not unpadded base64url")]
    Base64 {
        /// The offending field.
        field: &'static str,
    },

    /// A hex digest field was not lowercase hex of the expected length.
    #[error("field `{field}` is not a lowercase {expected_bytes}-byte hex digest")]
    Hex {
        /// The offending field.
        field: &'static str,
        /// How many bytes the digest should decode to.
        expected_bytes: usize,
    },

    /// An object declared a protocol version this build does not implement.
    #[error("unsupported protocol version {found}, expected {expected}")]
    UnsupportedVersion {
        /// The version carried by the object.
        found: u32,
        /// The version this build implements.
        expected: u32,
    },

    /// A message addressed nobody.
    ///
    /// An empty recipient set would produce ciphertext no one can read while
    /// still looking like a valid message, so it is rejected at construction.
    #[error("a message must have at least one intended recipient device")]
    EmptyRecipientSet,

    /// A derived identifier did not match the one carried by the object.
    ///
    /// A device ID is a hash of its own descriptor, so a mismatch means the
    /// descriptor was altered after the ID was assigned.
    #[error("`{field}` does not match the value derived from the object contents")]
    DerivedMismatch {
        /// The identifier that failed to reproduce.
        field: &'static str,
    },
}

/// Convenience alias for protocol results.
pub type Result<T> = std::result::Result<T, ProtocolError>;
