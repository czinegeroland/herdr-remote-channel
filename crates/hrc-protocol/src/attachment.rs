//! Attachment references and the limits they are checked against.
//!
//! An attachment is a reference to an encrypted blob, not the blob itself.
//! That is what makes PRD requirement HRC-SEC-006 possible: the declared size
//! travels in the signed envelope, so a receiver can refuse an attachment
//! *before* fetching or decrypting anything. A limit applied after decryption
//! is not a limit, it is a postmortem.
//!
//! The declaration is sender-controlled, so it is never trusted on its own.
//! It is the first of two checks: the declared size must be within the
//! limits, and the bytes that actually arrive must match what was declared.
//! A small declaration attached to a large object is the interesting attack,
//! and it fails the second check.
//!
//! HRC does not decompress attachments. It moves opaque ciphertext and hands
//! the plaintext to a human. There is no archive extraction, so there is no
//! expansion step for a decompression bomb to exploit — see decision DEC-045.

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::error::{ProtocolError, Result};

/// Default ceiling for one attachment's ciphertext (PRD section 20.3).
pub const DEFAULT_ATTACHMENT_CIPHERTEXT_BYTES: u64 = 5 * 1024 * 1024;

/// Hard ceiling for one attachment's ciphertext (PRD section 20.3).
pub const MAX_ATTACHMENT_CIPHERTEXT_BYTES: u64 = 25 * 1024 * 1024;

/// Default ceiling for all attachments on one message (PRD section 20.3).
pub const DEFAULT_MESSAGE_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

/// Hard ceiling for all attachments on one message (PRD section 20.3).
pub const MAX_MESSAGE_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// The most attachments one message may carry.
///
/// Each one costs a fetch and a decryption, so an unbounded count is a way
/// to make a receiver do a great deal of work for one small envelope.
pub const MAX_ATTACHMENTS: usize = 32;

/// The longest attachment name that will be stored.
pub const MAX_NAME_BYTES: usize = 255;

/// A reference to an encrypted blob carried with a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// The sender's name for it.
    ///
    /// Sender-controlled text, so it is quarantined with the body and is
    /// never placed on an agent-safe surface (PRD section 19.1). It is also
    /// never used as a path: see [`safe_file_name`].
    pub name: String,
    /// The sender's declared media type.
    #[serde(rename = "mediaType")]
    pub media_type: String,
    /// Digest of the encrypted blob, which is also how it is addressed.
    #[serde(rename = "ciphertextSha256")]
    pub ciphertext_sha256: String,
    /// Declared size of the encrypted blob.
    #[serde(rename = "ciphertextBytes")]
    pub ciphertext_bytes: u64,
    /// Declared size after decryption.
    #[serde(rename = "plaintextBytes")]
    pub plaintext_bytes: u64,
}

impl Attachment {
    /// Checks one attachment's declaration against the section 20.3 limits.
    pub fn validate(&self) -> Result<()> {
        canonical::expect_hex_digest("ciphertextSha256", &self.ciphertext_sha256, 32)?;

        if self.name.is_empty() {
            return Err(ProtocolError::MissingField { field: "name" });
        }

        if self.name.len() > MAX_NAME_BYTES {
            return Err(ProtocolError::MessageTooLarge {
                size: self.name.len(),
                limit: MAX_NAME_BYTES,
            });
        }

        if self.media_type.is_empty() {
            return Err(ProtocolError::MissingField { field: "mediaType" });
        }

        if self.ciphertext_bytes == 0 {
            return Err(ProtocolError::MissingField {
                field: "ciphertextBytes",
            });
        }

        if self.ciphertext_bytes > MAX_ATTACHMENT_CIPHERTEXT_BYTES {
            return Err(ProtocolError::MessageTooLarge {
                size: self.ciphertext_bytes as usize,
                limit: MAX_ATTACHMENT_CIPHERTEXT_BYTES as usize,
            });
        }

        // Ciphertext is never smaller than its plaintext: age adds a header
        // and per-chunk overhead. A declaration claiming otherwise is either
        // wrong or is describing an expansion step that does not exist here.
        if self.plaintext_bytes > self.ciphertext_bytes {
            return Err(ProtocolError::DerivedMismatch {
                field: "plaintextBytes",
            });
        }

        Ok(())
    }

    /// Checks the bytes that actually arrived against what was declared.
    ///
    /// The second half of HRC-SEC-006. Without it the declared size would be
    /// a promise rather than a limit: a sender could declare one kilobyte
    /// and attach twenty megabytes.
    pub fn verify_fetched(&self, ciphertext: &[u8]) -> Result<()> {
        if ciphertext.len() as u64 != self.ciphertext_bytes {
            return Err(ProtocolError::DerivedMismatch {
                field: "ciphertextBytes",
            });
        }

        let digest = canonical::sha256_hex(ciphertext);
        if digest != self.ciphertext_sha256 {
            return Err(ProtocolError::DerivedMismatch {
                field: "ciphertextSha256",
            });
        }

        Ok(())
    }
}

/// Checks a whole message's attachment set.
pub fn validate_attachments(attachments: &[Attachment]) -> Result<()> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(ProtocolError::MessageTooLarge {
            size: attachments.len(),
            limit: MAX_ATTACHMENTS,
        });
    }

    let mut total: u64 = 0;
    let mut seen = std::collections::BTreeSet::new();

    for attachment in attachments {
        attachment.validate()?;

        // Two references to one blob would be counted twice against the
        // total, or once, depending on where you looked. Refusing the repeat
        // removes the question.
        if !seen.insert(attachment.ciphertext_sha256.as_str()) {
            return Err(ProtocolError::DerivedMismatch {
                field: "ciphertextSha256",
            });
        }

        total = total.checked_add(attachment.ciphertext_bytes).ok_or(
            ProtocolError::MessageTooLarge {
                size: usize::MAX,
                limit: MAX_MESSAGE_ATTACHMENT_BYTES as usize,
            },
        )?;
    }

    if total > MAX_MESSAGE_ATTACHMENT_BYTES {
        return Err(ProtocolError::MessageTooLarge {
            size: total as usize,
            limit: MAX_MESSAGE_ATTACHMENT_BYTES as usize,
        });
    }

    Ok(())
}

/// The name an attachment may be saved under locally.
///
/// A sender chooses the name, so it is treated as hostile text rather than
/// as a path. Everything that could make it escape a directory, address a
/// device, or confuse a shell is removed, and a name left with nothing is
/// replaced rather than rejected — a receiver who asked to save a file
/// should get a file (PRD section 28.3, malicious file names).
pub fn safe_file_name(name: &str) -> String {
    /// Names Windows refuses to use for an ordinary file, in any case and
    /// with any extension.
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    // Take the last component, so `../../etc/passwd` and `C:\\windows\\x`
    // both reduce to a plain name rather than to a traversal.
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();

    let cleaned: String = last
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| match character {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            other => other,
        })
        .collect();

    // A name that is only dots addresses a directory rather than a file.
    let cleaned = cleaned.trim_matches(|character: char| character == ' ' || character == '.');

    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();

    if cleaned.is_empty() || RESERVED.contains(&stem.as_str()) {
        return "attachment".to_owned();
    }

    let mut safe = cleaned.to_owned();
    safe.truncate(MAX_NAME_BYTES);
    safe
}

#[cfg(test)]
mod tests;
