//! Context package tests.
//!
//! Two properties matter most: what the preview reports is what leaves the
//! machine, and a package carrying a secret cannot be sent.

use super::*;

fn note(text: &str) -> ContextItem {
    ContextItem::Note {
        text: text.to_owned(),
    }
}

fn excerpt(path: &str, text: &str) -> ContextItem {
    ContextItem::Excerpt {
        path: path.to_owned(),
        first_line: 1,
        last_line: 3,
        commit_sha: Some("abc123".into()),
        text: text.to_owned(),
    }
}

#[test]
fn a_package_round_trips_and_reports_its_items() {
    let package = ContextPackage::new(
        "ctx-1",
        vec![
            note("the retry loop backs off too fast"),
            excerpt("src/retry.rs", "fn retry() {}"),
        ],
    );

    let preview = package.preview().unwrap();

    assert_eq!(preview.items.len(), 2);
    assert_eq!(preview.items[0].0, "note");
    assert_eq!(preview.items[1].0, "excerpt");
    assert!(preview.is_sendable());
}

#[test]
fn the_preview_reports_the_real_serialized_size() {
    // A user approving a byte count must be shown the size that actually
    // leaves, not the sum of the item texts, which is smaller.
    let package = ContextPackage::new("ctx-1", vec![note("x".repeat(500).as_str())]);
    let preview = package.preview().unwrap();

    let serialized = canonical::to_canonical_bytes(&package).unwrap().len();

    assert_eq!(preview.total_bytes, serialized);
    assert!(
        preview.total_bytes > preview.items[0].1,
        "the encoded size must exceed the raw text length"
    );
}

#[test]
fn every_supported_item_kind_is_representable() {
    // PRD section 20.1.
    let package = ContextPackage::new(
        "ctx-1",
        vec![
            note("a note"),
            excerpt("src/lib.rs", "fn main() {}"),
            ContextItem::Patch {
                range: "HEAD~1..HEAD".into(),
                diff: "--- a\n+++ b\n".into(),
            },
            ContextItem::Reference {
                reference: "abc123".into(),
                description: Some("the failing commit".into()),
            },
            ContextItem::Output {
                command: "cargo test".into(),
                text: "1 failed".into(),
            },
            ContextItem::Link {
                url: "https://example.invalid/run/1".into(),
                description: None,
            },
        ],
    );

    let kinds: Vec<String> = package
        .preview()
        .unwrap()
        .items
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();

    assert_eq!(
        kinds,
        vec!["note", "excerpt", "patch", "reference", "output", "link"]
    );
}

#[test]
fn the_digest_detects_any_alteration() {
    // A receiver verifies before displaying, so a changed package must not
    // verify (HRC-CTX-006).
    let package = ContextPackage::new("ctx-1", vec![note("original")]);
    let digest = package.digest().unwrap();

    package.verify_digest(&digest).unwrap();

    let altered = ContextPackage::new("ctx-1", vec![note("altered")]);
    assert!(matches!(
        altered.verify_digest(&digest),
        Err(CoreError::ContextDigestMismatch)
    ));

    let reordered = ContextPackage::new("ctx-1", vec![note("original"), note("extra")]);
    assert!(reordered.verify_digest(&digest).is_err());
}

#[test]
fn a_package_carrying_a_private_key_cannot_be_sent() {
    let package = ContextPackage::new(
        "ctx-1",
        vec![note(
            "here is the config\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow==\n",
        )],
    );

    let preview = package.preview().unwrap();
    assert!(!preview.is_sendable());
    assert_eq!(preview.secrets[0].rule, "private_key");

    assert!(matches!(
        package.ready_to_send(),
        Err(CoreError::ContextContainsSecrets { count: 1 })
    ));
}

#[test]
fn common_token_shapes_are_detected() {
    let cases = [
        ("AKIAIOSFODNN7EXAMPLE", "aws_access_key"),
        ("ghp_16CharactersOfTokenHere0000000000", "github_token"),
        ("xoxb-123456789012-abcdefghijklmnop", "slack_token"),
        ("AIzaSyA-ExampleKeyMaterialHere000000000", "google_api_key"),
        ("sk_live_ExampleStripeKeyMaterial", "stripe_key"),
        ("AGE-SECRET-KEY-1EXAMPLEEXAMPLE", "age_identity"),
        (
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.abc.def",
            "json_web_token",
        ),
    ];

    for (sample, expected) in cases {
        let findings = scan_for_secrets(0, sample);
        assert!(
            findings.iter().any(|finding| finding.rule == expected),
            "{sample} should trip the {expected} rule, got {findings:?}"
        );
    }
}

#[test]
fn a_finding_never_quotes_the_secret() {
    // An error that echoes the secret puts it in logs and scrollback, which
    // is exactly where it was not supposed to go.
    let secret = "ghp_16CharactersOfTokenHere0000000000";
    let findings = scan_for_secrets(0, &format!("token: {secret}"));

    assert!(!findings.is_empty());
    for finding in findings {
        assert!(
            !finding.detail.contains(secret),
            "the finding quoted the secret: {}",
            finding.detail
        );
        assert!(!finding.detail.contains("ghp_"));
    }
}

#[test]
fn credential_assignments_with_real_values_are_flagged() {
    let findings = scan_for_secrets(0, "DATABASE_PASSWORD=hunter2IsNotGreat");

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == "credential_assignment"),
        "got {findings:?}"
    );
}

#[test]
fn secret_scanning_covers_package_and_item_metadata() {
    for package in [
        ContextPackage::new("ghp_16CharactersOfTokenHere0000000000", vec![note("safe")]),
        ContextPackage::new(
            "ctx-1",
            vec![ContextItem::Reference {
                reference: "abc123".into(),
                description: Some("ghp_16CharactersOfTokenHere0000000000".into()),
            }],
        ),
        ContextPackage::new(
            "ctx-1",
            vec![ContextItem::Link {
                url: "https://example.invalid".into(),
                description: Some("ghp_16CharactersOfTokenHere0000000000".into()),
            }],
        ),
    ] {
        assert!(!package.preview().unwrap().is_sendable());
    }
}

#[test]
fn placeholders_and_short_values_do_not_trip_the_scanner() {
    // A blocking check that cries wolf gets disabled, and then it protects
    // nothing. These are the shapes that appear in documentation and
    // examples.
    for benign in [
        "password=",
        "password: changeme",
        "API_KEY=your_key_here",
        "client_secret: <your-secret>",
        "secret: REDACTED",
        "password: null",
        "# password: example-value-here",
        "api_key=TODO",
    ] {
        let findings = scan_for_secrets(0, benign);
        assert!(
            findings.is_empty(),
            "{benign:?} should not be flagged, got {findings:?}"
        );
    }
}

#[test]
fn ordinary_code_and_prose_are_not_flagged() {
    for benign in [
        "fn retry(attempts: u32) -> Duration { Duration::from_secs(1) }",
        "The staging deploy failed after three retries.",
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "commit abc123def456",
        "https://example.invalid/actions/runs/12345",
    ] {
        assert!(
            scan_for_secrets(0, benign).is_empty(),
            "{benign:?} should not be flagged"
        );
    }
}

#[test]
fn excluded_paths_block_a_send() {
    // PRD section 20.2.
    let cases = [
        ".env",
        ".env.production",
        "config/.env",
        ".ssh/id_rsa",
        "home/user/.ssh/config",
        "certs/server.pem",
        "keys/service.key",
        "id_ed25519",
        ".netrc",
        "deploy/secrets.yaml",
        ".git",
        ".git/config",
        "linked-worktree/.git",
        "repo/.git/HEAD",
    ];

    for path in cases {
        assert!(
            excluded_path_reason(path).is_some(),
            "{path} should be excluded"
        );

        let package = ContextPackage::new("ctx-1", vec![excerpt(path, "contents")]);
        assert!(matches!(
            package.ready_to_send(),
            Err(CoreError::ContextContainsExcludedPath { .. })
        ));
    }
}

#[test]
fn exclusion_is_case_insensitive_and_handles_windows_separators() {
    assert!(excluded_path_reason("Config\\.ENV").is_some());
    assert!(excluded_path_reason("Certs\\Server.PEM").is_some());
    assert!(excluded_path_reason("repo\\.git\\config").is_some());
}

#[test]
fn ordinary_source_paths_are_not_excluded() {
    for path in [
        "src/lib.rs",
        "docs/PRD.md",
        "environment.rs",
        "src/env_config.rs",
        "tests/keychain_test.rs",
    ] {
        assert!(
            excluded_path_reason(path).is_none(),
            "{path} should be allowed"
        );
    }
}

#[test]
fn environmental_dumps_and_scrollback_are_explicitly_rejected() {
    // These are excluded by their declared source, not only when a token
    // scanner happens to recognize a value inside them.
    for command in [
        "env",
        "/usr/bin/env",
        "printenv",
        "sh -c env",
        "bash -lc printenv",
        "cmd /c set",
        "Get-ChildItem Env:",
        "pwsh -Command Get-ChildItem Env:",
        "terminal scrollback",
        "export prompt transcript",
        "history",
    ] {
        let package = ContextPackage::new(
            "ctx-1",
            vec![ContextItem::Output {
                command: command.into(),
                text: "ordinary-looking text without a scanner match".into(),
            }],
        );
        let preview = package.preview().unwrap();
        assert!(!preview.is_sendable(), "{command:?} was accepted");
        assert!(preview.secrets.is_empty());
        assert!(
            preview.excluded[0].reason.contains("HRC ran itself"),
            "{:?}",
            preview.excluded[0].reason
        );
    }
}

#[test]
fn caller_authored_patch_and_output_provenance_is_not_sendable() {
    for item in [
        ContextItem::Patch {
            range: String::new(),
            diff: "--- a/.env\n+++ b/.env\n+DATABASE_URL=postgres://example\n".into(),
        },
        ContextItem::Output {
            command: "cargo test".into(),
            text: "DATABASE_URL=postgres://example".into(),
        },
    ] {
        let preview = ContextPackage::new("ctx-untrusted-source", vec![item])
            .preview()
            .unwrap();
        assert!(!preview.is_sendable());
        assert!(
            preview.excluded[0].reason.contains("HRC captured")
                || preview.excluded[0].reason.contains("HRC ran itself"),
            "{:?}",
            preview.excluded[0].reason
        );
    }
}

#[test]
fn a_clean_package_is_ready_to_send() {
    let package = ContextPackage::new(
        "ctx-1",
        vec![
            note("the retry loop backs off too fast"),
            excerpt("src/retry.rs", "fn retry() { sleep(1) }"),
        ],
    );

    let preview = package.ready_to_send().unwrap();

    assert!(preview.is_sendable());
    assert!(preview.total_bytes > 0);
}

#[test]
fn the_offending_item_is_identified_by_index() {
    // Telling someone a package has a secret without saying where is not
    // actionable when the package has a dozen items.
    let package = ContextPackage::new(
        "ctx-1",
        vec![
            note("all clear"),
            note("token: ghp_16CharactersOfTokenHere0000000000"),
        ],
    );

    let preview = package.preview().unwrap();
    assert_eq!(preview.secrets.len(), 1);
    assert_eq!(preview.secrets[0].item_index, 1);
}

#[test]
fn an_empty_package_is_sendable_and_carries_no_content() {
    let package = ContextPackage::new("ctx-1", Vec::new());
    let preview = package.preview().unwrap();

    assert!(preview.items.is_empty());
    assert!(preview.is_sendable());
}

#[test]
fn received_context_requires_its_announced_digest() {
    let package = ContextPackage::new("ctx-1", vec![note("remote investigation notes")]);
    let digest = package.digest().unwrap();
    let body = serde_json::json!({
        "context": package,
        "contextDigest": digest,
    });

    let (received, announced) = ContextPackage::from_message_body(&body)
        .unwrap()
        .expect("context should be found");
    assert_eq!(received.id, "ctx-1");
    assert_eq!(announced, received.digest().unwrap());

    let mut altered = body;
    altered["context"]["items"][0]["text"] = serde_json::json!("altered after signing");
    assert!(matches!(
        ContextPackage::from_message_body(&altered),
        Err(CoreError::ContextDigestMismatch)
    ));
}

#[test]
fn a_context_without_a_digest_is_refused_before_storage() {
    let body = serde_json::json!({
        "context": ContextPackage::new("ctx-1", vec![note("remote notes")]),
    });

    assert!(matches!(
        ContextPackage::from_message_body(&body),
        Err(CoreError::MalformedMessage { .. })
    ));
}

#[test]
fn only_commands_hrc_runs_itself_are_accepted_as_output() {
    for allowed in AllowedCommand::ALL {
        let package = ContextPackage::new(
            "ctx-allowed",
            vec![ContextItem::Output {
                command: allowed.as_str().into(),
                text: "M src/lib.rs".into(),
            }],
        );

        let preview = package.preview().unwrap();
        assert!(
            preview.is_sendable(),
            "`{}` should be sendable: {:?}",
            allowed.as_str(),
            preview.excluded
        );
    }
}

#[test]
fn the_allowlist_closes_what_a_deny_list_could_not() {
    // The previous check recognized environment dumps and scrollback by
    // pattern. Every command below evades that kind of matching while doing
    // exactly what the requirement forbids, and an allowlist refuses all of
    // them without having to recognize any of them as dangerous.
    for command in [
        "sh -c 'cat ~/.bash_history'",
        "cat /proc/self/environ",
        "bash -lc 'declare -p'",
        "cat .env",
        "git config --list --show-origin",
        "aws configure list",
        "cat ~/.aws/credentials",
        "tmux capture-pane -p -S -",
        "screen -X hardcopy /tmp/out",
        "cargo test",
        "git status",              // right idea, not the exact spelling
        "git  status --porcelain", // doubled space
        "GIT STATUS --PORCELAIN",
        "",
    ] {
        let package = ContextPackage::new(
            "ctx-denied",
            vec![ContextItem::Output {
                command: command.into(),
                text: "ordinary-looking text without a scanner match".into(),
            }],
        );

        let preview = package.preview().unwrap();
        assert!(!preview.is_sendable(), "`{command}` should not be sendable");
    }
}

#[test]
fn a_patch_must_name_the_range_it_was_captured_over() {
    let without = ContextPackage::new(
        "ctx-patch",
        vec![ContextItem::Patch {
            range: "   ".into(),
            diff: "--- a\n+++ b\n".into(),
        }],
    )
    .preview()
    .unwrap();
    assert!(
        !without.is_sendable(),
        "a patch with no range is not a patch"
    );

    let with = ContextPackage::new(
        "ctx-patch",
        vec![ContextItem::Patch {
            range: "HEAD~2..HEAD".into(),
            diff: "--- a\n+++ b\n".into(),
        }],
    )
    .preview()
    .unwrap();
    assert!(with.is_sendable(), "{:?}", with.excluded);
}

#[test]
fn a_patch_range_is_scanned_for_secrets_like_any_other_caller_text() {
    // The range is caller-supplied, so it is one more field somebody could
    // put a token in.
    let package = ContextPackage::new(
        "ctx-patch",
        vec![ContextItem::Patch {
            range: "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB".into(),
            diff: "--- a\n+++ b\n".into(),
        }],
    );

    let preview = package.preview().unwrap();
    assert!(
        !preview.is_sendable(),
        "a token in the range must block the send"
    );
}

#[test]
fn every_allowed_command_round_trips_and_names_a_read_only_git_operation() {
    for allowed in AllowedCommand::ALL {
        assert_eq!(AllowedCommand::parse(allowed.as_str()), Some(allowed));

        let argv = allowed.argv();
        assert!(!argv.is_empty());
        // Read-only: none of these write to the repository or run project
        // code, which is why capturing them is a smaller decision than
        // running a build.
        assert!(
            matches!(argv[0], "status" | "log" | "diff"),
            "`{}` is not a read-only git operation",
            allowed.as_str()
        );
    }
}

#[test]
fn no_allowed_command_can_emit_an_environment_variable() {
    // The property section 12.5 actually asks for. Asserted against the
    // argument vector rather than the prose, so adding a command to the list
    // without thinking about it fails here.
    for allowed in AllowedCommand::ALL {
        let flat = format!("{} {}", allowed.as_str(), allowed.argv().join(" "));
        let lower = flat.to_lowercase();

        for forbidden in ["env", "printenv", "history", "config", "--show-origin"] {
            assert!(
                !lower.split_whitespace().any(|word| word == forbidden),
                "`{}` could emit environment or configuration data",
                allowed.as_str()
            );
        }
    }
}
