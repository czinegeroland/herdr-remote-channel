use super::*;

use hrc_protocol::{DelegationState, Reference, ResultBody};

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A checkout with two commits on `main`, and one on a side branch that
/// `HEAD` does not contain.
fn checkout() -> (tempfile::TempDir, String, String) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "--quiet", "--initial-branch=main"]);
    git(root, &["commit", "--quiet", "--allow-empty", "-m", "one"]);
    git(root, &["checkout", "--quiet", "-b", "side"]);
    git(root, &["commit", "--quiet", "--allow-empty", "-m", "side"]);
    let side = git(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "--quiet", "main"]);
    git(root, &["commit", "--quiet", "--allow-empty", "-m", "two"]);
    let head = git(root, &["rev-parse", "HEAD"]);
    (directory, head, side)
}

fn result_body(references: Vec<Reference>) -> String {
    canonical::to_canonical_json(&ResultBody {
        task_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        state: DelegationState::ResultPendingReview,
        summary: "the fix is on main".into(),
        context_id: None,
        references,
    })
    .unwrap()
}

#[test]
fn the_checkout_says_whether_it_holds_a_claimed_commit() {
    let (directory, head, side) = checkout();
    let root = directory.path();

    assert_eq!(check_commit(root, &head), CommitCheck::InHistory);
    assert_eq!(check_commit(root, &side), CommitCheck::Present);
    assert_eq!(check_commit(root, &"0".repeat(40)), CommitCheck::Missing);

    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(
        check_commit(elsewhere.path(), &head),
        CommitCheck::NoCheckout
    );
}

#[test]
fn a_sender_names_a_commit_however_they_like_and_the_full_name_travels() {
    let (directory, head, _) = checkout();
    let root = directory.path();

    assert_eq!(resolve_commit(root, "HEAD").unwrap(), head);
    assert_eq!(resolve_commit(root, &head[..8]).unwrap(), head);
    assert_eq!(resolve_commit(root, "main").unwrap(), head);

    // Nothing that is not a commit here, and nothing shaped like an option.
    for revision in ["no-such-branch", "", "--all", "-h"] {
        assert!(
            matches!(
                resolve_commit(root, revision),
                Err(CliError::UnresolvedCommit { .. })
            ),
            "{revision:?} resolved"
        );
    }
}

#[test]
fn the_review_reports_each_reference_in_words() {
    let found = "a".repeat(40);
    let elsewhere = "b".repeat(40);
    let missing = "c".repeat(40);
    let body = result_body(vec![
        Reference::commit(&found),
        Reference::commit(&elsewhere),
        Reference::commit(&missing),
        Reference {
            kind: "ci_run".into(),
            id: "12345".into(),
        },
        Reference::commit("abc123"),
    ]);

    let lines = local_checks_with("result", &body, |id| match id {
        id if id == found => CommitCheck::InHistory,
        id if id == elsewhere => CommitCheck::Present,
        _ => CommitCheck::Missing,
    });

    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(lines[0].contains("aaaaaaaaaaaa") && lines[0].contains("HEAD contains it"));
    assert!(lines[1].contains("not in your HEAD"));
    assert!(lines[2].contains("CLAIMED BUT NOT FOUND"));
    assert!(lines[3].contains("was not checked"));
    assert!(lines[4].contains("malformed"));
}

#[test]
fn a_result_that_names_nothing_says_so() {
    let lines = local_checks_with("result", &result_body(Vec::new()), |_| {
        panic!("nothing to check")
    });
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("sender's word"), "{lines:?}");
}

#[test]
fn only_a_result_is_checked() {
    for kind in ["note", "question", "answer", "task", "progress"] {
        assert!(
            local_checks_with(kind, "{}", |_| panic!("checked a {kind}")).is_empty(),
            "{kind}"
        );
    }

    let unreadable = local_checks_with("result", "not json", |_| panic!("checked"));
    assert!(
        unreadable[0].contains("could not be read"),
        "{unreadable:?}"
    );
}

#[test]
fn a_malformed_reference_never_reaches_git() {
    // The id is the one thing a sender controls that is passed to Git, so an
    // id that fails validation must not be handed to the checker at all.
    let body = result_body(vec![Reference::commit("--output=/tmp/x")]);
    let lines = local_checks_with("result", &body, |id| panic!("{id} reached git"));
    assert!(lines[0].contains("malformed"), "{lines:?}");
}
