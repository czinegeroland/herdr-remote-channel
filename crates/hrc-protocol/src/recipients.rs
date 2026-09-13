//! The signed intended-recipient device set.
//!
//! PRD section 17.5 requires every message to commit to the exact set of
//! devices its sender encrypted to, as a deterministically sorted
//! `recipientDeviceIds` array plus a `recipientDevicesHash` over the JCS
//! encoding of that array.
//!
//! This does not prove that every listed device actually received a working
//! age stanza — age recipient stanzas carry no portable identity mapping, so
//! no receiver can check that on another's behalf. What it does provide is
//! authenticated *intent*: a receiver can tell whether the sender claimed to
//! address the same set the roster implies, and can confirm its own presence
//! in that set. A sender that quietly drops or adds a recipient is detectable
//! by comparison against the roster, which is the guarantee the PRD claims
//! and the limit it acknowledges.

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::error::{ProtocolError, Result};

/// A sorted, deduplicated set of device IDs with its commitment hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientDevices {
    /// Device IDs, sorted ascending and free of duplicates.
    #[serde(rename = "recipientDeviceIds")]
    pub device_ids: Vec<String>,
    /// SHA-256 over the JCS encoding of `device_ids`.
    #[serde(rename = "recipientDevicesHash")]
    pub devices_hash: String,
}

impl RecipientDevices {
    /// Builds a recipient set from an unordered collection of device IDs.
    ///
    /// Sorting and deduplication happen here so that two senders addressing
    /// the same devices always produce the same commitment, whatever order
    /// their local roster happened to yield.
    pub fn new<I, S>(device_ids: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut ids: Vec<String> = device_ids.into_iter().map(Into::into).collect();
        ids.sort_unstable();
        ids.dedup();

        if ids.is_empty() {
            return Err(ProtocolError::EmptyRecipientSet);
        }

        for id in &ids {
            canonical::expect_hex_digest("recipientDeviceIds", id, 32)?;
        }

        let devices_hash = canonical::canonical_sha256_hex(&ids)?;
        Ok(Self {
            device_ids: ids,
            devices_hash,
        })
    }

    /// Checks that the carried hash and ordering describe the carried list.
    ///
    /// A receiver runs this before trusting any field of the set: an
    /// unsorted or duplicated list would let the same logical recipient set
    /// produce several different commitments.
    pub fn verify(&self) -> Result<()> {
        if self.device_ids.is_empty() {
            return Err(ProtocolError::EmptyRecipientSet);
        }

        let sorted_and_unique = self
            .device_ids
            .windows(2)
            .all(|pair| pair[0].as_str() < pair[1].as_str());
        if !sorted_and_unique {
            return Err(ProtocolError::DerivedMismatch {
                field: "recipientDeviceIds",
            });
        }

        for id in &self.device_ids {
            canonical::expect_hex_digest("recipientDeviceIds", id, 32)?;
        }
        canonical::expect_hex_digest("recipientDevicesHash", &self.devices_hash, 32)?;

        if canonical::canonical_sha256_hex(&self.device_ids)? == self.devices_hash {
            Ok(())
        } else {
            Err(ProtocolError::DerivedMismatch {
                field: "recipientDevicesHash",
            })
        }
    }

    /// Whether `device_id` is one of the intended recipients.
    pub fn contains(&self, device_id: &str) -> bool {
        self.device_ids
            .binary_search_by(|id| id.as_str().cmp(device_id))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a distinct, well-formed device ID for tests.
    fn device(seed: &str) -> String {
        canonical::sha256_hex(seed.as_bytes())
    }

    #[test]
    fn recipients_are_sorted_and_deduplicated() {
        let set = RecipientDevices::new([device("b"), device("a"), device("b")]).unwrap();

        let mut expected = vec![device("a"), device("b")];
        expected.sort();
        assert_eq!(set.device_ids, expected);
    }

    #[test]
    fn input_order_does_not_change_the_commitment() {
        let forward = RecipientDevices::new([device("a"), device("b"), device("c")]).unwrap();
        let reversed = RecipientDevices::new([device("c"), device("b"), device("a")]).unwrap();
        assert_eq!(forward, reversed);
    }

    #[test]
    fn a_different_membership_changes_the_commitment() {
        let two = RecipientDevices::new([device("a"), device("b")]).unwrap();
        let three = RecipientDevices::new([device("a"), device("b"), device("c")]).unwrap();
        assert_ne!(two.devices_hash, three.devices_hash);
    }

    #[test]
    fn a_well_formed_set_verifies() {
        RecipientDevices::new([device("a"), device("b")])
            .unwrap()
            .verify()
            .unwrap();
    }

    #[test]
    fn an_added_recipient_breaks_the_commitment() {
        let mut set = RecipientDevices::new([device("a"), device("b")]).unwrap();
        set.device_ids.push(device("z"));
        set.device_ids.sort();

        assert!(matches!(
            set.verify().unwrap_err(),
            ProtocolError::DerivedMismatch {
                field: "recipientDevicesHash"
            }
        ));
    }

    #[test]
    fn a_removed_recipient_breaks_the_commitment() {
        let mut set = RecipientDevices::new([device("a"), device("b")]).unwrap();
        set.device_ids.remove(0);

        assert!(matches!(
            set.verify().unwrap_err(),
            ProtocolError::DerivedMismatch {
                field: "recipientDevicesHash"
            }
        ));
    }

    #[test]
    fn an_unsorted_list_is_rejected() {
        let mut set = RecipientDevices::new([device("a"), device("b")]).unwrap();
        set.device_ids.reverse();

        assert!(matches!(
            set.verify().unwrap_err(),
            ProtocolError::DerivedMismatch {
                field: "recipientDeviceIds"
            }
        ));
    }

    #[test]
    fn a_duplicated_entry_is_rejected() {
        // Duplicates would otherwise let one logical set have two valid
        // encodings, and therefore two different commitments.
        let mut set = RecipientDevices::new([device("a")]).unwrap();
        set.device_ids.push(device("a"));

        assert!(matches!(
            set.verify().unwrap_err(),
            ProtocolError::DerivedMismatch {
                field: "recipientDeviceIds"
            }
        ));
    }

    #[test]
    fn an_empty_recipient_set_is_rejected() {
        let empty: [String; 0] = [];
        assert!(matches!(
            RecipientDevices::new(empty).unwrap_err(),
            ProtocolError::EmptyRecipientSet
        ));
    }

    #[test]
    fn malformed_device_ids_are_rejected() {
        assert!(matches!(
            RecipientDevices::new(["not-a-digest"]).unwrap_err(),
            ProtocolError::Hex { .. }
        ));
    }

    #[test]
    fn membership_is_reported_accurately() {
        let set = RecipientDevices::new([device("a"), device("b")]).unwrap();
        assert!(set.contains(&device("a")));
        assert!(set.contains(&device("b")));
        assert!(!set.contains(&device("c")));
    }
}
