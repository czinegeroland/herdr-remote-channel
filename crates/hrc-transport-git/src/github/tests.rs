use super::*;

use std::cell::RefCell;

/// A `gh` that returns whatever a test scripted, and records what it was
/// asked. No network, no token, no `gh` installed.
struct FakeGh {
    responses: RefCell<Vec<std::io::Result<GhOutput>>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeGh {
    fn new(responses: Vec<std::io::Result<GhOutput>>) -> Self {
        Self {
            responses: RefCell::new(responses),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn ok(stdout: &str) -> std::io::Result<GhOutput> {
        Ok(GhOutput {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    fn failed(stderr: &str) -> std::io::Result<GhOutput> {
        Ok(GhOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        })
    }
}

impl GhRunner for FakeGh {
    fn run(&self, arguments: &[String]) -> std::io::Result<GhOutput> {
        self.calls.borrow_mut().push(arguments.to_vec());
        match self.responses.borrow_mut().pop() {
            Some(response) => response,
            None => panic!("the watcher made more calls than the test scripted"),
        }
    }
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn changed_response(sha: &str, etag: &str) -> String {
    format!("HTTP/2.0 200 OK\r\nETag: {etag}\r\nContent-Type: text/plain\r\n\r\n{sha}\n")
}

fn unchanged_response(etag: &str) -> String {
    format!("HTTP/2.0 304 Not Modified\r\nETag: {etag}\r\n\r\n")
}

#[test]
fn every_shape_of_github_locator_is_recognized() {
    let expected = GitHubRemote {
        owner: "alice".into(),
        repo: "channel".into(),
    };

    for locator in [
        "https://github.com/alice/channel",
        "https://github.com/alice/channel.git",
        "https://github.com/alice/channel/",
        "http://github.com/alice/channel",
        "git@github.com:alice/channel.git",
        "git@github.com:alice/channel",
        "ssh://git@github.com/alice/channel.git",
        "https://someone@github.com/alice/channel",
    ] {
        assert_eq!(
            GitHubRemote::parse(locator).as_ref(),
            Some(&expected),
            "`{locator}` should be recognized"
        );
    }
}

#[test]
fn anything_that_is_not_github_gets_the_generic_path() {
    // Not failures. A channel on a shared folder or another forge is
    // ordinary, and PRD section 28 requires it to work without any GitHub
    // API at all.
    for locator in [
        "https://gitlab.com/alice/channel",
        "/srv/channels/alice.git",
        "C:\\channels\\alice.git",
        "git@gitlab.com:alice/channel.git",
        "https://github.example.com/alice/channel",
        "https://notgithub.com/alice/channel",
        "",
    ] {
        assert_eq!(
            GitHubRemote::parse(locator),
            None,
            "`{locator}` should not be treated as GitHub"
        );
    }
}

#[test]
fn a_github_enterprise_host_is_not_treated_as_github_com() {
    // Enterprise uses a different API root. Sending a request built for
    // github.com would address a repository somewhere it does not exist, so
    // these installations get the generic path until someone implements the
    // enterprise root deliberately.
    assert_eq!(
        GitHubRemote::parse("https://github.mycorp.com/alice/channel"),
        None
    );
}

#[test]
fn a_locator_with_extra_path_segments_is_refused_rather_than_guessed() {
    assert_eq!(GitHubRemote::parse("https://github.com/alice"), None);
    assert_eq!(
        GitHubRemote::parse("https://github.com/alice/channel/tree/main"),
        None
    );
}

#[test]
fn a_first_poll_reports_the_head_and_remembers_the_validator() {
    let runner = FakeGh::new(vec![FakeGh::ok(&changed_response(SHA, "\"abc123\""))]);
    let mut watcher = HeadWatcher::for_locator("https://github.com/alice/channel", "hrc", runner)
        .expect("github");

    assert_eq!(
        watcher.poll(),
        Probe::Changed {
            head: SHA.to_owned(),
            etag: Some("\"abc123\"".to_owned()),
        }
    );
    assert_eq!(watcher.etag(), Some("\"abc123\""));
}

#[test]
fn the_second_poll_is_conditional_on_the_remembered_validator() {
    // The entire point of the optimization: GitHub answers 304 cheaply and
    // it does not count against the primary rate limit.
    let runner = FakeGh::new(vec![
        FakeGh::ok(&unchanged_response("\"abc123\"")),
        FakeGh::ok(&changed_response(SHA, "\"abc123\"")),
    ]);
    let mut watcher = HeadWatcher::for_locator("https://github.com/alice/channel", "hrc", runner)
        .expect("github");

    assert!(matches!(watcher.poll(), Probe::Changed { .. }));
    assert_eq!(watcher.poll(), Probe::Unchanged);
}

#[test]
fn a_conditional_hit_is_read_from_the_status_not_the_exit_code() {
    // `gh` exits non-zero on 304. Treating that as a failure would discard
    // the cheap answer the conditional request exists to get.
    let runner = FakeGh::new(vec![Ok(GhOutput {
        success: false,
        stdout: unchanged_response("\"abc123\""),
        stderr: "gh: exit status 1".to_owned(),
    })]);
    let mut watcher = HeadWatcher::for_locator("https://github.com/alice/channel", "hrc", runner)
        .expect("github");

    assert_eq!(watcher.poll(), Probe::Unchanged);
}

#[test]
fn gh_not_being_installed_falls_back_rather_than_failing() {
    let runner = FakeGh::new(vec![Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no such file or directory",
    ))]);
    let mut watcher = HeadWatcher::for_locator("https://github.com/alice/channel", "hrc", runner)
        .expect("github");

    assert!(matches!(watcher.poll(), Probe::Unavailable(_)));
}

#[test]
fn every_provider_failure_lands_on_unavailable() {
    // The caller's response to all of these is identical and correct: use
    // `ls-remote`. Distinguishing them would only create ways to get it
    // wrong.
    for stderr in [
        "gh: To get started with GitHub CLI, please run: gh auth login",
        "HTTP 403: API rate limit exceeded",
        "HTTP 404: Not Found",
        "error connecting to api.github.com",
    ] {
        let runner = FakeGh::new(vec![FakeGh::failed(stderr)]);
        let mut watcher =
            HeadWatcher::for_locator("https://github.com/alice/channel", "hrc", runner)
                .expect("github");

        assert!(
            matches!(watcher.poll(), Probe::Unavailable(_)),
            "`{stderr}` should fall back"
        );
    }
}

#[test]
fn a_successful_response_with_no_sha_is_unavailable_rather_than_a_wrong_head() {
    // Reporting a head this code invented would make the daemon skip a real
    // change. Falling back costs one `ls-remote`.
    let runner = FakeGh::new(vec![FakeGh::ok("HTTP/2.0 200 OK\r\n\r\nnot a sha\n")]);
    let mut watcher = HeadWatcher::for_locator("https://github.com/alice/channel", "hrc", runner)
        .expect("github");

    assert!(matches!(watcher.poll(), Probe::Unavailable(_)));
}

#[test]
fn a_non_github_locator_has_no_watcher_at_all() {
    // Not a watcher that always falls back — no watcher. The generic path is
    // not a degraded mode, it is the normal one.
    assert!(HeadWatcher::for_locator("/srv/channels/alice.git", "hrc", GhCli).is_none());
}

#[test]
fn the_request_names_the_branch_and_asks_for_the_smallest_representation() {
    let runner = FakeGh::new(vec![FakeGh::ok(&changed_response(SHA, "\"abc\""))]);
    let mut watcher = HeadWatcher::for_locator("git@github.com:alice/channel.git", "hrc", runner)
        .expect("github");
    watcher.poll();

    let calls = watcher.runner().calls.borrow();
    let flat = calls[0].join(" ");
    assert!(flat.contains("repos/alice/channel/commits/hrc"), "{flat}");
    assert!(flat.contains("application/vnd.github.sha"), "{flat}");
}

#[test]
fn the_optimization_never_carries_channel_content() {
    // `AC-GIT-ADAPTER` requires the GitHub optimization to preserve
    // identical protocol behavior. It does so by construction: the only
    // thing it can return is a commit SHA or a reason to fall back. There is
    // no variant that could hold an object, a publication, or a byte of
    // ciphertext, so a faster way to learn whether to fetch cannot change
    // what a fetch returns.
    let probes = [
        Probe::Unchanged,
        Probe::Changed {
            head: SHA.to_owned(),
            etag: None,
        },
        Probe::Unavailable("offline".to_owned()),
    ];

    for probe in probes {
        match probe {
            Probe::Unchanged | Probe::Unavailable(_) => {}
            Probe::Changed { head, .. } => {
                assert_eq!(head.len(), 40, "a head is a commit SHA and nothing else");
            }
        }
    }
}

#[test]
fn setup_steps_lead_with_making_the_repository_private() {
    let remote = GitHubRemote::parse("https://github.com/alice/channel").expect("github");
    let steps = setup_steps(&remote);

    assert!(steps[0].contains("private"));
    assert!(steps.iter().all(|step| !step.is_empty()));
    assert!(
        steps.iter().any(|step| step.contains("force pushes")),
        "branch protection should be advised"
    );
    assert!(
        steps.iter().any(|step| step.contains("safety phrase")),
        "collaborator access should follow verification"
    );
}

#[test]
fn setup_advice_never_hands_out_a_token() {
    let remote = GitHubRemote::parse("https://github.com/alice/channel").expect("github");
    let advice = setup_steps(&remote).join(" ").to_lowercase();

    assert!(advice.contains("hrc never stores a token"));
    for forbidden in ["ghp_", "personal access token to hrc", "paste your token"] {
        assert!(
            !advice.contains(forbidden),
            "advice must not ask for a token"
        );
    }
}
