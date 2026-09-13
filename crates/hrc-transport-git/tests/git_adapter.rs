//! Git adapter tests.
//!
//! These run against a real bare repository acting as the remote, so pushes,
//! fetches, and non-fast-forward rejections are genuine Git behavior rather
//! than a simulation of it. Nothing here touches the network.
//!
//! PRD requirement HRC-TR-005 and acceptance suite `AC-GIT-ADAPTER`: the Git
//! transport must work through plain Git, with no GitHub API dependency.

use std::path::Path;
use std::process::Command;

use hrc_transport::memory::MemoryTransport;
use hrc_transport::{
    ObjectClass, PublicationClass, PublishObject, PublishRequest, Transport, TransportError,
    conformance,
};
use hrc_transport_git::GitTransport;

/// Creates a bare repository to act as the shared remote.
fn bare_remote(path: &Path) {
    let status = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(path)
        .status()
        .expect("git should be installed");
    assert!(status.success(), "could not create the bare remote");
}

/// A peer with its own local repository pointing at `remote`.
fn peer(root: &Path, name: &str, remote: &Path) -> GitTransport {
    GitTransport::open(root.join(name), remote.to_str().expect("utf-8 path"))
        .expect("adapter should open")
}

/// Builds a control object.
fn control(sequence: u32) -> PublishObject {
    PublishObject {
        name: format!("control/log/{sequence:08}.json"),
        class: ObjectClass::Control,
        bytes: format!("control-entry-{sequence}").into_bytes(),
    }
}

/// Builds a message object.
fn message(name: &str) -> PublishObject {
    PublishObject {
        name: format!("messages/2026/09/{name}.age"),
        class: ObjectClass::Message,
        bytes: format!("ciphertext-{name}").into_bytes(),
    }
}

#[test]
fn the_git_adapter_conforms() {
    // The same suite the reference adapter passes. Two implementations
    // sharing no code, held to one contract, is what makes transport
    // independence a property rather than a claim.
    let directory = tempfile::tempdir().unwrap();
    let mut index = 0;

    conformance::run_suite(|| {
        index += 1;
        let remote = directory.path().join(format!("remote-{index}.git"));
        bare_remote(&remote);
        peer(directory.path(), &format!("peer-{index}"), &remote)
    })
    .expect("the Git adapter should conform");
}

#[test]
fn the_reference_and_git_adapters_agree_on_the_contract() {
    // A regression guard for divergence: whatever the suite accepts, both
    // adapters must accept identically.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut git = peer(directory.path(), "peer", &remote);
    let mut memory = MemoryTransport::new("reference");

    let git_genesis = git.create_group(vec![control(0)]).unwrap();
    let memory_genesis = memory.create_group(vec![control(0)]).unwrap();

    assert_eq!(git_genesis.class, memory_genesis.class);
    assert_eq!(git_genesis.parent_revision, memory_genesis.parent_revision);
    assert_eq!(
        git_genesis.objects[0].sha256, memory_genesis.objects[0].sha256,
        "both adapters must hash the same bytes to the same digest"
    );
}

#[test]
fn genesis_is_a_root_commit_and_the_branch_is_pushed() {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut git = peer(directory.path(), "peer", &remote);
    let genesis = git.create_group(vec![control(0)]).unwrap();

    assert!(genesis.parent_revision.is_none());
    assert_eq!(genesis.class, PublicationClass::Genesis);

    // The remote really has it, at the same commit.
    assert_eq!(
        git.remote_head().unwrap().as_deref(),
        Some(genesis.revision.as_str())
    );
}

#[test]
fn a_second_peer_sees_what_the_first_published() {
    // The core offline-delivery scenario: one peer publishes, another later
    // fetches and reads the same bytes.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();
    alice
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let bob = peer(directory.path(), "bob", &remote);
    bob.sync_from_remote().unwrap();

    let page = bob.fetch(None, 100).unwrap();
    assert_eq!(page.publications.len(), 2);
    assert_eq!(page.publications[0].class, PublicationClass::Genesis);
    assert_eq!(page.publications[1].class, PublicationClass::Data);

    let record = &page.publications[1].objects[0];
    let bytes = bob.get_object(&record.name, &record.sha256).unwrap();
    assert_eq!(bytes, b"ciphertext-msg-1");
}

#[test]
fn concurrent_publication_is_resolved_by_conflict_and_retry() {
    // PRD section 17.3 and acceptance criterion AC-GIT-SYNC: two peers
    // publish against the same tip; the loser is told to rebuild, and the
    // history stays linear with both messages present.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();

    let mut bob = peer(directory.path(), "bob", &remote);
    bob.sync_from_remote().unwrap();

    // Both believe genesis is the tip.
    alice
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message("from-alice")],
        })
        .unwrap();

    let conflict = bob.publish(PublishRequest {
        expected_revision: Some(genesis.revision.clone()),
        class: PublicationClass::Data,
        objects: vec![message("from-bob")],
    });

    let current = match conflict {
        Err(TransportError::Conflict { current }) => current.expect("a revision to rebuild on"),
        other => panic!("expected a conflict, got {other:?}"),
    };
    assert_ne!(
        current, genesis.revision,
        "the conflict reports the new tip"
    );

    // Rebuilding on the reported tip succeeds without manual intervention.
    bob.publish(PublishRequest {
        expected_revision: Some(current),
        class: PublicationClass::Data,
        objects: vec![message("from-bob")],
    })
    .unwrap();

    alice.sync_from_remote().unwrap();
    let page = alice.fetch(None, 100).unwrap();
    assert_eq!(
        page.publications.len(),
        3,
        "history is linear, nothing lost"
    );

    let names: Vec<&str> = page
        .publications
        .iter()
        .flat_map(|p| p.objects.iter().map(|o| o.name.as_str()))
        .collect();
    assert!(names.iter().any(|n| n.contains("from-alice")));
    assert!(names.iter().any(|n| n.contains("from-bob")));
}

#[test]
fn a_rejected_push_leaves_nothing_behind_in_the_channel() {
    // The losing peer's objects must not appear in the published history.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();

    let mut bob = peer(directory.path(), "bob", &remote);
    bob.sync_from_remote().unwrap();

    alice
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message("from-alice")],
        })
        .unwrap();

    let _ = bob.publish(PublishRequest {
        expected_revision: Some(genesis.revision),
        class: PublicationClass::Data,
        objects: vec![message("orphan")],
    });

    alice.sync_from_remote().unwrap();
    let page = alice.fetch(None, 100).unwrap();
    let names: Vec<&str> = page
        .publications
        .iter()
        .flat_map(|p| p.objects.iter().map(|o| o.name.as_str()))
        .collect();

    assert!(
        !names.iter().any(|name| name.contains("orphan")),
        "a rejected publication must not reach the channel: {names:?}"
    );
}

#[test]
fn a_merge_commit_in_the_history_is_rejected() {
    // PRD section 17.5.1: a merge commit has no single predecessor, so a
    // client cannot say which roster state governs what it introduces. This
    // is how a removed device would try to reintroduce an old-epoch message.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();
    let branch_a = alice
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message("a")],
        })
        .unwrap();

    // Forge a side branch and merge it, using raw Git.
    let git_dir = alice.repository().git_dir().to_owned();
    let run = |args: &[&str]| -> String {
        let output = Command::new("git")
            .env("GIT_DIR", &git_dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@localhost")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@localhost")
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    let tree = run(&["rev-parse", &format!("{}^{{tree}}", genesis.revision)]);
    let side = run(&["commit-tree", &tree, "-p", &genesis.revision, "-m", "side"]);
    let merge = run(&[
        "commit-tree",
        &tree,
        "-p",
        &branch_a.revision,
        "-p",
        &side,
        "-m",
        "merge",
    ]);
    run(&["update-ref", "refs/heads/hrc", &merge]);

    let result = alice.fetch(None, 100);
    assert!(
        matches!(result, Err(TransportError::InvalidPublication { .. })),
        "a merge commit must be rejected, got {result:?}"
    );
}

#[test]
fn a_commit_that_modifies_an_existing_object_is_rejected() {
    // History is append-only. A modification means an object the client may
    // already have validated changed underneath it.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();
    let published = alice
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let git_dir = alice.repository().git_dir().to_owned();
    let run_with_input = |args: &[&str], input: &[u8]| -> String {
        use std::io::Write as _;
        let mut child = Command::new("git")
            .env("GIT_DIR", &git_dir)
            .env("GIT_INDEX_FILE", git_dir.join("forge-index"))
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@localhost")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@localhost")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("git should run");
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(input)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let run = |args: &[&str]| -> String {
        let output = Command::new("git")
            .env("GIT_DIR", &git_dir)
            .env("GIT_INDEX_FILE", git_dir.join("forge-index"))
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@localhost")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@localhost")
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    // Rewrite the message blob in a new commit on top.
    let blob = run_with_input(&["hash-object", "-w", "--stdin"], b"substituted ciphertext");
    run(&["read-tree", &published.revision]);
    run(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{blob},messages/2026/09/msg-1.age"),
    ]);
    let tree = run(&["write-tree"]);
    let tampered = run(&[
        "commit-tree",
        &tree,
        "-p",
        &published.revision,
        "-m",
        "tamper",
    ]);
    run(&["update-ref", "refs/heads/hrc", &tampered]);

    let result = alice.fetch(None, 100);
    assert!(
        matches!(result, Err(TransportError::InvalidPublication { .. })),
        "a modifying commit must be rejected, got {result:?}"
    );
}

#[test]
fn an_object_outside_the_channel_layout_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();

    let stray = PublishObject {
        name: "somewhere/else.txt".into(),
        class: ObjectClass::Message,
        bytes: b"stray".to_vec(),
    };

    let result = alice.publish(PublishRequest {
        expected_revision: Some(genesis.revision),
        class: PublicationClass::Data,
        objects: vec![stray],
    });

    assert!(matches!(
        result,
        Err(TransportError::InvalidPublication { .. })
    ));
}

#[test]
fn a_path_that_escapes_the_layout_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();

    for path in [
        "../escape.age",
        "/absolute.age",
        "messages/../../escape.age",
    ] {
        let escaping = PublishObject {
            name: path.into(),
            class: ObjectClass::Message,
            bytes: b"escape".to_vec(),
        };

        let result = alice.publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![escaping],
        });

        assert!(
            matches!(result, Err(TransportError::InvalidPublication { .. })),
            "{path} should be refused, got {result:?}"
        );
    }
}

#[test]
fn substituting_an_object_under_an_existing_path_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();
    let published = alice
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let mut different = message("msg-1");
    different.bytes = b"different ciphertext".to_vec();

    let result = alice.publish(PublishRequest {
        expected_revision: Some(published.revision),
        class: PublicationClass::Data,
        objects: vec![different],
    });

    assert!(matches!(
        result,
        Err(TransportError::InvalidPublication { .. })
    ));
}

#[test]
fn the_remote_head_is_readable_without_fetching_objects() {
    // PRD section 17.1: the daemon watches the tip and only fetches when it
    // moves. This must work before anything has been fetched locally.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut alice = peer(directory.path(), "alice", &remote);
    let genesis = alice.create_group(vec![control(0)]).unwrap();

    let bob = peer(directory.path(), "bob", &remote);
    assert_eq!(
        bob.remote_head().unwrap().as_deref(),
        Some(genesis.revision.as_str())
    );
    // Bob has not fetched, so he has no local channel yet.
    assert!(matches!(bob.open_group(), Err(TransportError::NoSuchGroup)));
}

#[test]
fn an_empty_remote_reports_no_head() {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let alice = peer(directory.path(), "alice", &remote);
    assert_eq!(alice.remote_head().unwrap(), None);
    alice
        .sync_from_remote()
        .expect("an empty remote is not an error");
}

#[test]
fn the_adapter_reports_healthy_and_declares_durability() {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let alice = peer(directory.path(), "alice", &remote);
    alice.health().unwrap();
    assert!(alice.capabilities().durable);
    assert_eq!(alice.capabilities().adapter, "hrc-transport-git");
}
