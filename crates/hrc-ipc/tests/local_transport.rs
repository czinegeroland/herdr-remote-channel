//! End-to-end IPC over the platform's real local transport.
//!
//! PRD acceptance criterion `AC-RUST-IPC` asks for framed requests over
//! Unix-domain sockets and Windows named pipes with reconnect and permission
//! tests. These run against whichever of those the host provides, so the same
//! file is the compatibility spike on all three CI platforms.

use std::time::Duration;

use hrc_ipc::endpoint::{Interface, prepare_runtime_dir};
use hrc_ipc::{Client, Endpoint, IpcError, MAX_FRAME_BYTES, serve};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
enum Request {
    Status,
    Echo { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum Response {
    Status { pending: u32 },
    Echo { text: String },
}

fn answer(request: Request) -> Response {
    match request {
        Request::Status => Response::Status { pending: 1 },
        Request::Echo { text } => Response::Echo { text },
    }
}

/// A runtime directory unique to one test, since two tests binding the same
/// endpoint name would interfere on Windows where the name is not a path.
struct Fixture {
    _dir: tempfile::TempDir,
    endpoint: Endpoint,
}

impl Fixture {
    fn new(interface: Interface) -> Self {
        let dir = tempfile::tempdir().unwrap();
        prepare_runtime_dir(dir.path()).unwrap();
        let endpoint = Endpoint::new(dir.path(), interface).unwrap();

        Self {
            _dir: dir,
            endpoint,
        }
    }
}

/// Starts a server and waits until it is accepting.
async fn start(endpoint: &Endpoint) -> tokio::task::JoinHandle<()> {
    let endpoint = endpoint.clone();
    let handle = tokio::spawn(async move {
        let _ = serve(&endpoint, answer as fn(Request) -> Response).await;
    });

    // Binding is not instantaneous, and a client that raced it would fail
    // for a reason unrelated to what each test is checking.
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle
}

#[tokio::test]
async fn a_request_and_its_answer_cross_the_local_transport() {
    let fixture = Fixture::new(Interface::AgentSafe);
    let server = start(&fixture.endpoint).await;

    let mut client = Client::connect_with_retry(&fixture.endpoint, 20, Duration::from_millis(25))
        .await
        .unwrap();

    let response: Response = client.call(&Request::Status).await.unwrap();
    assert_eq!(response, Response::Status { pending: 1 });

    client.close().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn one_connection_carries_many_requests_in_order() {
    // Framing has to hold across a conversation, not only for a single
    // exchange: the failure mode this catches is answers arriving shifted by
    // one, which a single round trip cannot see.
    let fixture = Fixture::new(Interface::AgentSafe);
    let server = start(&fixture.endpoint).await;

    let mut client = Client::connect_with_retry(&fixture.endpoint, 20, Duration::from_millis(25))
        .await
        .unwrap();

    for index in 0..25 {
        let text = format!("message {index}");
        let response: Response = client
            .call(&Request::Echo { text: text.clone() })
            .await
            .unwrap();
        assert_eq!(response, Response::Echo { text });
    }

    client.close().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn several_clients_are_served_concurrently() {
    let fixture = Fixture::new(Interface::AgentSafe);
    let server = start(&fixture.endpoint).await;

    let mut tasks = Vec::new();
    for index in 0..8 {
        let endpoint = fixture.endpoint.clone();
        tasks.push(tokio::spawn(async move {
            let mut client = Client::connect_with_retry(&endpoint, 20, Duration::from_millis(25))
                .await
                .unwrap();
            let text = format!("client {index}");
            let response: Response = client
                .call(&Request::Echo { text: text.clone() })
                .await
                .unwrap();
            assert_eq!(response, Response::Echo { text });
        }));
    }

    for task in tasks {
        task.await.unwrap();
    }

    server.abort();
}

#[tokio::test]
async fn a_client_reconnects_after_the_daemon_restarts() {
    // The case a user actually hits: the daemon is upgraded or restarted
    // while a client is idle. Reconnecting must work without the client
    // knowing anything about socket files or pipe names.
    let fixture = Fixture::new(Interface::AgentSafe);

    let first = start(&fixture.endpoint).await;
    let mut client = Client::connect_with_retry(&fixture.endpoint, 20, Duration::from_millis(25))
        .await
        .unwrap();
    let _: Response = client.call(&Request::Status).await.unwrap();

    first.abort();
    let _ = first.await;
    drop(client);

    // The restarted daemon has to be able to take the endpoint back even
    // though the previous process left a socket file behind.
    let second = start(&fixture.endpoint).await;
    let mut client = Client::connect_with_retry(&fixture.endpoint, 40, Duration::from_millis(25))
        .await
        .unwrap();
    let response: Response = client.call(&Request::Status).await.unwrap();
    assert_eq!(response, Response::Status { pending: 1 });

    client.close().await.unwrap();
    second.abort();
}

#[tokio::test]
async fn connecting_to_an_endpoint_with_no_daemon_fails_rather_than_hanging() {
    let fixture = Fixture::new(Interface::TrustedHuman);

    let error = Client::connect_with_retry(&fixture.endpoint, 2, Duration::from_millis(10))
        .await
        .unwrap_err();

    assert!(
        matches!(error, IpcError::Io(_)),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn the_two_interfaces_are_separate_listeners() {
    // The authority boundary of PRD section 19.5. Connecting to one must not
    // reach the other, and both must be able to run at once.
    let dir = tempfile::tempdir().unwrap();
    prepare_runtime_dir(dir.path()).unwrap();

    let agent = Endpoint::new(dir.path(), Interface::AgentSafe).unwrap();
    let trusted = Endpoint::new(dir.path(), Interface::TrustedHuman).unwrap();

    let agent_server = {
        let endpoint = agent.clone();
        tokio::spawn(async move {
            let _ = serve(
                &endpoint,
                (|_: Request| Response::Status { pending: 1 }) as fn(Request) -> Response,
            )
            .await;
        })
    };
    let trusted_server = {
        let endpoint = trusted.clone();
        tokio::spawn(async move {
            let _ = serve(
                &endpoint,
                (|_: Request| Response::Status { pending: 99 }) as fn(Request) -> Response,
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut on_agent = Client::connect_with_retry(&agent, 20, Duration::from_millis(25))
        .await
        .unwrap();
    let mut on_trusted = Client::connect_with_retry(&trusted, 20, Duration::from_millis(25))
        .await
        .unwrap();

    let from_agent: Response = on_agent.call(&Request::Status).await.unwrap();
    let from_trusted: Response = on_trusted.call(&Request::Status).await.unwrap();

    assert_eq!(from_agent, Response::Status { pending: 1 });
    assert_eq!(
        from_trusted,
        Response::Status { pending: 99 },
        "a connection reached the wrong interface"
    );

    agent_server.abort();
    trusted_server.abort();
}

#[tokio::test]
async fn the_client_refuses_to_send_an_oversized_frame() {
    // Refused locally, so the daemon never sees it and the connection stays
    // usable: a rejected write leaves nothing in the stream to desynchronize
    // the next frame.
    let fixture = Fixture::new(Interface::AgentSafe);
    let server = start(&fixture.endpoint).await;

    let mut client = Client::connect_with_retry(&fixture.endpoint, 20, Duration::from_millis(25))
        .await
        .unwrap();

    let hostile = Request::Echo {
        text: "x".repeat(MAX_FRAME_BYTES + 1),
    };
    let error = client.call::<_, Response>(&hostile).await.unwrap_err();
    assert!(matches!(error, IpcError::FrameTooLarge { .. }));

    let response: Response = client.call(&Request::Status).await.unwrap();
    assert_eq!(response, Response::Status { pending: 1 });

    client.close().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn a_frame_the_daemon_cannot_parse_drops_only_that_connection() {
    // A local process can send anything. The daemon has no way to find where
    // the next frame starts after a body it could not parse, so it drops
    // that connection — and only that one.
    let fixture = Fixture::new(Interface::AgentSafe);
    let server = start(&fixture.endpoint).await;

    let mut hostile = Client::connect_with_retry(&fixture.endpoint, 20, Duration::from_millis(25))
        .await
        .unwrap();
    let error = hostile
        .call::<_, Response>(&serde_json::json!({ "method": "not_a_method" }))
        .await
        .unwrap_err();
    assert!(
        matches!(error, IpcError::Disconnected | IpcError::Io(_)),
        "unexpected error: {error}"
    );

    let mut recovered =
        Client::connect_with_retry(&fixture.endpoint, 20, Duration::from_millis(25))
            .await
            .unwrap();
    let response: Response = recovered.call(&Request::Status).await.unwrap();
    assert_eq!(response, Response::Status { pending: 1 });

    recovered.close().await.unwrap();
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn the_socket_and_its_directory_are_owner_only() {
    // On Unix the filesystem is the access control. A socket another user
    // could connect to would make the trusted interface reachable by them.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    prepare_runtime_dir(dir.path()).unwrap();
    let endpoint = Endpoint::new(dir.path(), Interface::TrustedHuman).unwrap();

    let server = start(&endpoint).await;

    let socket = endpoint.path().unwrap();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the socket is reachable beyond its owner");

    let dir_mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
    assert_eq!(dir_mode & 0o077, 0, "the runtime directory is not private");

    server.abort();
}

#[cfg(windows)]
#[tokio::test]
async fn a_named_pipe_endpoint_serves_requests() {
    let fixture = Fixture::new(Interface::TrustedHuman);
    let server = start(&fixture.endpoint).await;

    let mut client = Client::connect_with_retry(&fixture.endpoint, 20, Duration::from_millis(25))
        .await
        .unwrap();
    let response: Response = client.call(&Request::Status).await.unwrap();
    assert_eq!(response, Response::Status { pending: 1 });

    client.close().await.unwrap();
    server.abort();
}
