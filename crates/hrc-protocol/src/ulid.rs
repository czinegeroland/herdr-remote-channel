//! Sortable unique identifiers.
//!
//! PRD section 13.2 calls for ULIDs. The property that matters here is the
//! one in the name: identifiers generated later sort later as plain strings,
//! so a message store can order by ID without parsing a timestamp out of it
//! and without trusting a separate field.
//!
//! This is an encoding, not a cryptographic primitive, so implementing it
//! here does not touch the section 14.2 rule against writing our own
//! cryptography. The randomness comes from the operating system.

use crate::error::{ProtocolError, Result};

/// Crockford base32, which excludes `I`, `L`, `O`, and `U` so that a
/// transcribed identifier cannot be confused with a digit or with itself.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The length of a ULID in characters.
pub const ULID_LENGTH: usize = 26;

/// Generates a ULID from a millisecond timestamp and fresh randomness.
///
/// Taking the timestamp as an argument rather than reading the clock keeps
/// this testable: the ordering property is the thing worth proving, and a
/// function that read the clock itself could only be tested by sleeping.
pub fn ulid_at(milliseconds: u64) -> Result<String> {
    let mut randomness = [0u8; 10];
    getrandom::fill(&mut randomness)
        .map_err(|_| ProtocolError::DerivedMismatch { field: "messageId" })?;

    Ok(encode(milliseconds, &randomness))
}

/// Encodes a timestamp and randomness as a ULID.
fn encode(milliseconds: u64, randomness: &[u8; 10]) -> String {
    // 48 bits of time then 80 bits of randomness, most significant first, so
    // that byte order and character order agree and lexicographic comparison
    // is chronological comparison.
    let mut bits = [0u8; 16];
    bits[..6].copy_from_slice(&milliseconds.to_be_bytes()[2..]);
    bits[6..].copy_from_slice(randomness);

    let mut out = String::with_capacity(ULID_LENGTH);
    // 128 bits into 26 characters of 5 bits each leaves 2 bits over, so the
    // first character carries only the top 2 bits.
    let value = u128::from_be_bytes(bits);

    for index in (0..ULID_LENGTH).rev() {
        let chunk = ((value >> (index * 5)) & 0x1f) as usize;
        out.push(ALPHABET[chunk] as char);
    }

    out
}

/// Whether a string is a well-formed ULID.
pub fn is_ulid(value: &str) -> bool {
    value.len() == ULID_LENGTH
        && value
            .bytes()
            .all(|byte| ALPHABET.contains(&byte.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ulid_has_the_documented_shape() {
        let ulid = ulid_at(1_757_721_600_000).unwrap();

        assert_eq!(ulid.len(), ULID_LENGTH);
        assert!(is_ulid(&ulid), "{ulid}");
    }

    #[test]
    fn later_identifiers_sort_later_as_strings() {
        // The whole reason for using a ULID. Without this a message store
        // would have to parse a timestamp out of the identifier, or trust a
        // separate field that a sender controls.
        let mut previous = ulid_at(0).unwrap();

        for milliseconds in [1, 1_000, 1_757_721_600_000, 281_474_976_710_655] {
            let next = ulid_at(milliseconds).unwrap();
            assert!(
                next > previous,
                "{next} did not sort after {previous} at {milliseconds}"
            );
            previous = next;
        }
    }

    #[test]
    fn two_identifiers_at_the_same_instant_differ() {
        let first = ulid_at(1_757_721_600_000).unwrap();
        let second = ulid_at(1_757_721_600_000).unwrap();

        assert_ne!(first, second);
        // ...and they still agree on the time, which is the first ten
        // characters.
        assert_eq!(first[..10], second[..10]);
    }

    #[test]
    fn the_alphabet_excludes_the_confusable_letters() {
        // Crockford base32 leaves out I, L, O, and U so a transcribed
        // identifier cannot be read back as a different one.
        let alphabet = std::str::from_utf8(ALPHABET).unwrap();

        for excluded in ['I', 'L', 'O', 'U'] {
            assert!(
                !alphabet.contains(excluded),
                "{excluded} is in the alphabet"
            );
        }
        assert_eq!(alphabet.len(), 32);
    }

    #[test]
    fn a_known_timestamp_encodes_to_a_known_prefix() {
        // Pinned so a change to the bit layout shows up as a failing test
        // rather than as silently reordered identifiers. The value is the
        // ULID specification's own example timestamp.
        let ulid = encode(1_469_918_176_385, &[0; 10]);

        assert_eq!(&ulid[..10], "01ARYZ6S41");
        assert_eq!(&ulid[10..], "0".repeat(16));
    }

    #[test]
    fn malformed_identifiers_are_rejected() {
        for bad in [
            "",
            "01ARYZ6S41",
            "01ARYZ6S41000000000000000000",
            "01ARYZ6S41I000000000000000",
            "01ARYZ6S41U000000000000000",
        ] {
            assert!(!is_ulid(bad), "{bad:?} was accepted");
        }
    }
}
