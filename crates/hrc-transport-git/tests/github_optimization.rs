//! The GitHub optimization must never change what the transport reports
//! (PRD requirement HRC-TR-006, acceptance criterion `AC-GIT-ADAPTER`).
//!
//! These run against a real bare repository, like the rest of the adapter
//! tests, with a scripted `gh` standing in for the provider. Nothing here
//! touches the network, and nothing needs `gh` installed.
//!
//! The claim being tested is the one the criterion makes: "GitHub
//! optimization preserves identical protocol behavior". So each case
//! compares the optimized answer against the generic `ls-remote` answer for
//! the same repository state, rather than checking the optimized answer in
//! isolation.

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;

use hrc_transport::{ObjectClass, PublishObject, Transport};
use hrc_transport_git::GitTransport;
use hrc_transport_git::github::{GhOutput, GhRunner, HeadWatcher};

fn bare_remote(path: &Path) {
    let status = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(path)
        .status()
        .expect("git should be installed");
    assert!(status.success(), "could not create the bare remote");
}

/// A `gh` that replays scripted responses.
struct ScriptedGh(RefCell<Vec<std::io::Result<GhOutput>>>);

impl ScriptedGh {
    fn new(mut responses: Vec<std::io::Result<GhOutput>>) -> Self {
        responses.reverse();
        Self(RefCell::new(responses))
    }
}

impl GhRunner for ScriptedGh {
    fn run(&self, _arguments: &[String]) -> std::io::Result<GhOutput> {
        self.0
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| panic!("the transport made an unscripted gh call"))
    }
}

fn ok(stdout: String) -> std::io::Result<GhOutput> {
    Ok(GhOutput {
        success: true,
        stdout,
        stderr: String::new(),
    })
}

fn genesis() -> Vec<PublishObject> {
    vec![PublishObject {
        name: "protocol.json".into(),
        class: ObjectClass::Protocol,
        bytes: b"{}".to_vec(),
    }]
}

#[test]
fn the_optimized_head_matches_the_generic_head() {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut transport =
        GitTransport::open(directory.path().join("local"), remote.to_str().unwrap()).unwrap();
    transport.create_group(genesis()).expect("genesis");

    let generic = transport
        .remote_head()
        .expect("ls-remote should work")
        .expect("the branch exists after genesis");

    let mut watcher = HeadWatcher::for_locator(
        "https://github.com/alice/channel",
        "hrc",
        ScriptedGh::new(vec![ok(format!(
            "HTTP/2.0 200 OK\r\nETag: \"v1\"\r\n\r\n{generic}\n"
        ))]),
    )
    .expect("a github locator");

    let optimized = transport
        .remote_head_optimized(Some(&mut watcher))
        .expect("the optimized path should work")
        .expect("the branch exists");

    assert_eq!(
        optimized, generic,
        "the optimization must report the same head as plain Git"
    );
}

#[test]
fn an_unchanged_answer_reports_the_head_already_fetched() {
    // A 304 means the remote has not moved, so the tip this adapter last
    // fetched is still current. Reading it locally is the saving: no network
    // round trip at all.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut transport =
        GitTransport::open(directory.path().join("local"), remote.to_str().unwrap()).unwrap();
    transport.create_group(genesis()).expect("genesis");
    let generic = transport.remote_head().unwrap().unwrap();

    let mut watcher = HeadWatcher::for_locator(
        "https://github.com/alice/channel",
        "hrc",
        ScriptedGh::new(vec![ok(
            "HTTP/2.0 304 Not Modified\r\nETag: \"v1\"\r\n\r\n".to_owned(),
        )]),
    )
    .expect("a github locator");

    assert_eq!(
        transport
            .remote_head_optimized(Some(&mut watcher))
            .unwrap()
            .unwrap(),
        generic
    );
}

#[test]
fn an_unusable_optimization_falls_back_to_an_identical_answer() {
    // The property that keeps the core from being GitHub-only: when the
    // optimization cannot answer, the result is exactly what a channel with
    // no GitHub involvement would have got.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut transport =
        GitTransport::open(directory.path().join("local"), remote.to_str().unwrap()).unwrap();
    transport.create_group(genesis()).expect("genesis");
    let generic = transport.remote_head().unwrap();

    for response in [
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "gh is not installed",
        )),
        Ok(GhOutput {
            success: false,
            stdout: String::new(),
            stderr: "gh auth login required".to_owned(),
        }),
        Ok(GhOutput {
            success: false,
            stdout: String::new(),
            stderr: "HTTP 403: API rate limit exceeded".to_owned(),
        }),
    ] {
        let mut watcher = HeadWatcher::for_locator(
            "https://github.com/alice/channel",
            "hrc",
            ScriptedGh::new(vec![response]),
        )
        .expect("a github locator");

        assert_eq!(
            transport.remote_head_optimized(Some(&mut watcher)).unwrap(),
            generic,
            "a failed optimization must give the generic answer"
        );
    }
}

#[test]
fn a_channel_with_no_watcher_behaves_exactly_as_before() {
    // Every non-GitHub channel takes this path, and it must be unchanged.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let mut transport =
        GitTransport::open(directory.path().join("local"), remote.to_str().unwrap()).unwrap();
    transport.create_group(genesis()).expect("genesis");

    let none: Option<&mut HeadWatcher<ScriptedGh>> = None;
    assert_eq!(
        transport.remote_head_optimized(none).unwrap(),
        transport.remote_head().unwrap()
    );
}

#[test]
fn a_channel_that_does_not_exist_yet_reports_nothing_either_way() {
    // Pre-genesis. Both paths must agree that there is no head rather than
    // one of them treating an absent branch as a failure.
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let transport =
        GitTransport::open(directory.path().join("local"), remote.to_str().unwrap()).unwrap();

    let mut watcher = HeadWatcher::for_locator(
        "https://github.com/alice/channel",
        "hrc",
        ScriptedGh::new(vec![Ok(GhOutput {
            success: false,
            stdout: String::new(),
            stderr: "HTTP 404: Not Found".to_owned(),
        })]),
    )
    .expect("a github locator");

    assert_eq!(transport.remote_head().unwrap(), None);
    assert_eq!(
        transport.remote_head_optimized(Some(&mut watcher)).unwrap(),
        None
    );
}
