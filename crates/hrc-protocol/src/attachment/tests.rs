//! Attachment limit and naming tests.
//!
//! Two themes: a size limit is only a limit if it is checked before the
//! expensive operation and again against what actually arrived, and a
//! sender-chosen file name is hostile text until proven otherwise.

use super::*;

fn blob(bytes: u64) -> Attachment {
    let content = vec![0u8; bytes as usize];

    Attachment {
        name: "findings.txt".into(),
        media_type: "text/plain".into(),
        ciphertext_sha256: canonical::sha256_hex(&content),
        ciphertext_bytes: bytes,
        plaintext_bytes: bytes.saturating_sub(64),
    }
}

#[test]
fn an_ordinary_attachment_validates() {
    blob(1024).validate().unwrap();
    validate_attachments(&[blob(1024)]).unwrap();
}

#[test]
fn a_declaration_above_the_hard_ceiling_is_refused() {
    // Refused on the declaration, which is the whole point: nothing has been
    // fetched or decrypted at this stage.
    let mut huge = blob(1024);
    huge.ciphertext_bytes = MAX_ATTACHMENT_CIPHERTEXT_BYTES + 1;

    assert!(matches!(
        huge.validate().unwrap_err(),
        ProtocolError::MessageTooLarge { .. }
    ));
}

#[test]
fn the_per_message_total_is_enforced_across_attachments() {
    // Each one under the single-attachment ceiling, the set over the total.
    let mut attachments = Vec::new();
    for index in 0..6 {
        let mut attachment = blob(5 * 1024 * 1024);
        attachment.ciphertext_sha256 = canonical::sha256_hex(&[index as u8]);
        attachments.push(attachment);
    }

    assert!(matches!(
        validate_attachments(&attachments).unwrap_err(),
        ProtocolError::MessageTooLarge { .. }
    ));
}

#[test]
fn an_unbounded_attachment_count_is_refused() {
    let attachments: Vec<Attachment> = (0..MAX_ATTACHMENTS + 1)
        .map(|index| {
            let mut attachment = blob(64);
            attachment.ciphertext_sha256 = canonical::sha256_hex(&index.to_le_bytes());
            attachment
        })
        .collect();

    assert!(validate_attachments(&attachments).is_err());
}

#[test]
fn one_blob_cannot_be_referenced_twice() {
    // Otherwise the total would depend on whether you counted references or
    // distinct objects.
    assert!(validate_attachments(&[blob(1024), blob(1024)]).is_err());
}

#[test]
fn a_bigger_object_than_declared_is_refused_on_arrival() {
    // The attack the second check exists for: declare a kilobyte, attach
    // twenty megabytes.
    let attachment = blob(1024);
    let actual = vec![0u8; 20 * 1024 * 1024];

    assert!(matches!(
        attachment.verify_fetched(&actual).unwrap_err(),
        ProtocolError::DerivedMismatch {
            field: "ciphertextBytes"
        }
    ));
}

#[test]
fn a_substituted_object_of_the_right_size_is_refused() {
    let attachment = blob(1024);
    let substituted = vec![7u8; 1024];

    assert!(matches!(
        attachment.verify_fetched(&substituted).unwrap_err(),
        ProtocolError::DerivedMismatch {
            field: "ciphertextSha256"
        }
    ));
}

#[test]
fn the_declared_object_passes_both_checks() {
    let content = vec![0u8; 1024];
    let attachment = blob(1024);

    attachment.validate().unwrap();
    attachment.verify_fetched(&content).unwrap();
}

#[test]
fn a_plaintext_larger_than_its_ciphertext_is_refused() {
    // Ciphertext is never smaller than its plaintext under age. A
    // declaration claiming otherwise describes an expansion step that does
    // not exist here, which is what a decompression bomb would need.
    let mut expanding = blob(1024);
    expanding.plaintext_bytes = 64 * 1024 * 1024;

    assert!(matches!(
        expanding.validate().unwrap_err(),
        ProtocolError::DerivedMismatch {
            field: "plaintextBytes"
        }
    ));
}

#[test]
fn a_zero_length_attachment_is_refused() {
    let mut empty = blob(1024);
    empty.ciphertext_bytes = 0;

    assert!(empty.validate().is_err());
}

#[test]
fn a_malformed_digest_is_refused() {
    let mut attachment = blob(1024);
    attachment.ciphertext_sha256 = attachment.ciphertext_sha256.to_uppercase();

    assert!(matches!(
        attachment.validate().unwrap_err(),
        ProtocolError::Hex { .. }
    ));
}

// --- Malicious file names (PRD section 28.3) ---

#[test]
fn traversal_is_reduced_to_a_plain_name() {
    for hostile in [
        "../../etc/passwd",
        "..\\..\\windows\\system32\\config\\sam",
        "/etc/shadow",
        "C:\\Windows\\notepad.exe",
        "subdir/../../../root/.ssh/id_rsa",
    ] {
        let safe = safe_file_name(hostile);

        assert!(!safe.contains('/'), "{hostile:?} kept a separator: {safe}");
        assert!(!safe.contains('\\'), "{hostile:?} kept a separator: {safe}");
        assert!(!safe.contains(".."), "{hostile:?} kept a traversal: {safe}");
        assert!(!safe.is_empty());
    }
}

#[test]
fn control_characters_and_device_names_do_not_survive() {
    assert_eq!(safe_file_name("re\0port.txt"), "report.txt");
    assert_eq!(safe_file_name("note\n.txt"), "note.txt");

    for device in ["CON", "con.txt", "NUL", "lpt1.log", "AUX"] {
        assert_eq!(
            safe_file_name(device),
            "attachment",
            "{device} survived as a file name"
        );
    }
}

#[test]
fn a_name_that_is_only_dots_or_spaces_becomes_a_real_name() {
    for empty in ["...", "..", ".", "   ", "", "/", "  . "] {
        assert_eq!(safe_file_name(empty), "attachment", "{empty:?}");
    }
}

#[test]
fn characters_that_confuse_a_shell_or_a_filesystem_are_replaced() {
    let safe = safe_file_name("re:port*?\"<>|.txt");

    for forbidden in [':', '*', '?', '"', '<', '>', '|'] {
        assert!(!safe.contains(forbidden), "{forbidden} survived in {safe}");
    }
}

#[test]
fn an_ordinary_name_is_left_alone() {
    // A sanitizer that mangles legitimate names gets worked around.
    for ordinary in [
        "findings.txt",
        "retry-backoff.patch",
        "Screenshot 2026-09-13.png",
        "notes_v2.final.md",
    ] {
        assert_eq!(safe_file_name(ordinary), ordinary);
    }
}

#[test]
fn a_very_long_name_is_truncated_rather_than_rejected() {
    let long = format!("{}.txt", "a".repeat(1000));
    let safe = safe_file_name(&long);

    assert!(safe.len() <= MAX_NAME_BYTES);
    assert!(!safe.is_empty());
}

#[test]
fn compressed_content_is_carried_as_opaque_bytes() {
    // HRC does not extract archives, so there is no expansion step for a
    // bomb to exploit. What bounds it is the ordinary attachment limit,
    // applied to the bytes as they are.
    let mut archive = blob(1024);
    archive.media_type = "application/zip".into();
    archive.name = "logs.zip".into();

    archive.validate().unwrap();
    assert_eq!(safe_file_name(&archive.name), "logs.zip");
}
