//! The out-of-process adapter binding (PRD section 21).
//!
//! Section 21 specifies adapters as separate executables speaking JSON-RPC
//! 2.0 over standard input and output with explicit message framing. This
//! module is both halves of that: [`AdapterProcess`] is a [`Transport`] that
//! forwards every call to a child process, and [`serve`] is the loop an
//! adapter executable runs to answer them.
//!
//! Having both here is what makes the binding testable. The conformance
//! suite of [`crate::conformance`] runs against any `Transport`, so pointing
//! it at an [`AdapterProcess`] wrapping the in-memory reference adapter runs
//! all fifteen checks *through a real process boundary*. A binding that
//! dropped an anomaly, reordered a page, or turned a conflict into a generic
//! failure would fail those checks rather than pass quietly.
//!
//! # Framing
//!
//! A four-byte big-endian length followed by that many bytes of JSON, the
//! same frame `hrc-ipc` uses. The duplication is deliberate: `hrc-ipc` is
//! async because the daemon's local interfaces are, and this trait is
//! synchronous because asynchrony belongs to the daemon that drives it
//! (decision DEC-026). Sharing the code would mean forcing a runtime into
//! every adapter executable.
//!
//! # What crosses the boundary
//!
//! Object bytes travel base64url-encoded, because JSON has no byte type.
//! They are opaque ciphertext on both sides (PRD section 21.3): this module
//! never inspects them, and the adapter is not given a key with which it
//! could.

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use hrc_protocol::canonical;
use serde::{Deserialize, Serialize};

use crate::{
    AdapterCapabilities, FetchPage, GroupState, LINEAR_APPEND_ONLY, ObjectClass, ObjectRecord,
    Publication, PublicationClass, PublishObject, PublishRequest, Result, Transport,
    TransportError,
};

/// The largest frame either side will read.
///
/// Checked before anything is allocated, for the same reason `hrc-ipc`
/// checks it: the point of a limit is that a peer never gets to decide how
/// much memory this process reserves.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Method names from PRD section 21.1.
pub mod method {
    /// Report adapter capabilities.
    pub const INITIALIZE: &str = "initialize";
    /// Create the channel and publish genesis.
    pub const GROUP_CREATE: &str = "group.create";
    /// Report the channel head.
    pub const GROUP_OPEN: &str = "group.open";
    /// Publish atomically against an expected revision.
    pub const PUBLISH: &str = "publish";
    /// Return publications after a cursor.
    pub const FETCH: &str = "fetch";
    /// Retrieve one object by name and expected hash.
    pub const GET_OBJECT: &str = "get_object";
    /// Report provider reachability.
    pub const HEALTH: &str = "health";
    /// Ask the adapter to exit.
    pub const SHUTDOWN: &str = "shutdown";
}

/// JSON-RPC error codes this binding uses.
///
/// The application codes are above the reserved `-32000` range and are
/// stable: a caller distinguishes a conflict from a provider failure by the
/// code, never by parsing the message.
pub mod code {
    /// The method is not one this adapter implements.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// The parameters were absent or the wrong shape.
    pub const INVALID_PARAMS: i32 = -32602;
    /// The expected revision was stale; nothing was published.
    pub const CONFLICT: i32 = 1;
    /// The channel does not exist.
    pub const NO_SUCH_GROUP: i32 = 2;
    /// The channel already exists.
    pub const GROUP_EXISTS: i32 = 3;
    /// The named object was not found.
    pub const NO_SUCH_OBJECT: i32 = 4;
    /// A fetched object did not match its expected hash.
    pub const OBJECT_HASH_MISMATCH: i32 = 5;
    /// The publication broke a structural rule.
    pub const INVALID_PUBLICATION: i32 = 6;
    /// An object exceeded a declared limit.
    pub const OBJECT_TOO_LARGE: i32 = 7;
    /// The underlying provider failed.
    pub const PROVIDER: i32 = 8;
}

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcResponse {
    jsonrpc: String,
    id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcError {
    code: i32,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

/// An object as it crosses the boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireObject {
    name: String,
    class: String,
    size: u64,
    sha256: String,
    /// Present on publish and on `get_object`, absent in fetched metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes: Option<String>,
}

/// A publication as it crosses the boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WirePublication {
    revision: String,
    #[serde(rename = "parentRevision")]
    parent_revision: Option<String>,
    #[serde(rename = "publicationClass")]
    class: String,
    #[serde(rename = "transportTime")]
    transport_time: String,
    objects: Vec<WireObject>,
}

fn class_name(class: PublicationClass) -> &'static str {
    class.as_str()
}

fn parse_publication_class(value: &str) -> Option<PublicationClass> {
    match value {
        "genesis" => Some(PublicationClass::Genesis),
        "control" => Some(PublicationClass::Control),
        "data" => Some(PublicationClass::Data),
        _ => None,
    }
}

fn parse_object_class(value: &str) -> Option<ObjectClass> {
    [
        ObjectClass::Control,
        ObjectClass::Protocol,
        ObjectClass::Join,
        ObjectClass::Message,
        ObjectClass::Blob,
        ObjectClass::Snapshot,
    ]
    .into_iter()
    .find(|class| class.as_str() == value)
}

impl From<&ObjectRecord> for WireObject {
    fn from(record: &ObjectRecord) -> Self {
        Self {
            name: record.name.clone(),
            class: record.class.as_str().to_owned(),
            size: record.size,
            sha256: record.sha256.clone(),
            bytes: None,
        }
    }
}

impl WireObject {
    fn into_record(self) -> Result<ObjectRecord> {
        let class = parse_object_class(&self.class).ok_or_else(|| {
            TransportError::Provider(format!(
                "adapter reported unknown object class {}",
                self.class
            ))
        })?;

        Ok(ObjectRecord {
            name: self.name,
            class,
            size: self.size,
            sha256: self.sha256,
        })
    }
}

impl WirePublication {
    fn from_publication(publication: &Publication) -> Self {
        Self {
            revision: publication.revision.clone(),
            parent_revision: publication.parent_revision.clone(),
            class: class_name(publication.class).to_owned(),
            transport_time: publication.transport_time.clone(),
            objects: publication.objects.iter().map(WireObject::from).collect(),
        }
    }

    fn into_publication(self) -> Result<Publication> {
        let class = parse_publication_class(&self.class).ok_or_else(|| {
            TransportError::Provider(format!(
                "adapter reported unknown publication class {}",
                self.class
            ))
        })?;

        let objects = self
            .objects
            .into_iter()
            .map(WireObject::into_record)
            .collect::<Result<Vec<_>>>()?;

        Ok(Publication {
            revision: self.revision,
            parent_revision: self.parent_revision,
            class,
            transport_time: self.transport_time,
            objects,
        })
    }
}

/// Writes one length-prefixed JSON frame.
fn write_frame<W: Write>(writer: &mut W, value: &impl Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

    let length = u32::try_from(bytes.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "frame exceeds u32 length")
    })?;

    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

/// Reads one length-prefixed JSON frame.
///
/// Returns `Ok(None)` at a clean end of stream, which is how the server loop
/// learns the caller is gone.
fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(reader: &mut R) -> std::io::Result<Option<T>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }

    let declared = u32::from_be_bytes(header) as usize;
    if declared > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame of {declared} bytes exceeds the {MAX_FRAME_BYTES} byte limit"),
        ));
    }

    let mut body = vec![0u8; declared];
    reader.read_exact(&mut body)?;

    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Maps a transport error onto its JSON-RPC error object.
///
/// A conflict carries the current revision in `data`, because the caller's
/// whole response to one is to rebuild on that revision and retry. Losing it
/// would turn a normal race into an unrecoverable failure.
fn error_object(error: &TransportError) -> RpcError {
    let (code, data) = match error {
        TransportError::Conflict { current } => (
            code::CONFLICT,
            Some(serde_json::json!({ "current": current })),
        ),
        TransportError::NoSuchGroup => (code::NO_SUCH_GROUP, None),
        TransportError::GroupExists => (code::GROUP_EXISTS, None),
        TransportError::NoSuchObject { name } => (
            code::NO_SUCH_OBJECT,
            Some(serde_json::json!({ "name": name })),
        ),
        TransportError::ObjectHashMismatch { name } => (
            code::OBJECT_HASH_MISMATCH,
            Some(serde_json::json!({ "name": name })),
        ),
        TransportError::InvalidPublication { reason } => (
            code::INVALID_PUBLICATION,
            Some(serde_json::json!({ "reason": reason })),
        ),
        TransportError::ObjectTooLarge { name, size, limit } => (
            code::OBJECT_TOO_LARGE,
            Some(serde_json::json!({ "name": name, "size": size, "limit": limit })),
        ),
        TransportError::Provider(_) => (code::PROVIDER, None),
    };

    RpcError {
        code,
        message: error.to_string(),
        data,
    }
}

/// Rebuilds a transport error from its JSON-RPC error object.
///
/// The inverse of [`error_object`]. An unrecognized code becomes a provider
/// error rather than being guessed at: an adapter reporting something this
/// build does not understand is a provider that failed in a new way, and
/// silently mapping it onto `Conflict` would make the core retry forever.
fn error_from_object(error: RpcError) -> TransportError {
    let field = |key: &str| -> Option<String> {
        error
            .data
            .as_ref()
            .and_then(|data| data.get(key))
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    };

    match error.code {
        code::CONFLICT => TransportError::Conflict {
            current: error
                .data
                .as_ref()
                .and_then(|data| data.get("current"))
                .and_then(|value| value.as_str())
                .map(str::to_owned),
        },
        code::NO_SUCH_GROUP => TransportError::NoSuchGroup,
        code::GROUP_EXISTS => TransportError::GroupExists,
        code::NO_SUCH_OBJECT => TransportError::NoSuchObject {
            name: field("name").unwrap_or_default(),
        },
        code::OBJECT_HASH_MISMATCH => TransportError::ObjectHashMismatch {
            name: field("name").unwrap_or_default(),
        },
        code::INVALID_PUBLICATION => TransportError::InvalidPublication {
            reason: field("reason").unwrap_or_else(|| error.message.clone()),
        },
        code::OBJECT_TOO_LARGE => {
            let number = |key: &str| {
                error
                    .data
                    .as_ref()
                    .and_then(|data| data.get(key))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default()
            };
            TransportError::ObjectTooLarge {
                name: field("name").unwrap_or_default(),
                size: number("size"),
                limit: number("limit"),
            }
        }
        _ => TransportError::Provider(error.message),
    }
}

/// Runs the adapter side of the protocol until the caller disconnects.
///
/// An adapter executable's `main` is expected to be little more than
/// constructing its [`Transport`] and calling this.
///
/// A method that fails answers with a JSON-RPC error and the loop continues:
/// a conflict or a missing object is an ordinary answer, not a reason to
/// tear down the process the core is depending on.
pub fn serve<T: Transport, R: Read, W: Write>(
    transport: &mut T,
    input: R,
    mut output: W,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(input);

    while let Some(request) = read_frame::<_, RpcRequest>(&mut reader)? {
        if request.method == method::SHUTDOWN {
            write_frame(
                &mut output,
                &RpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(serde_json::json!({})),
                    error: None,
                },
            )?;
            return Ok(());
        }

        let response = match answer(transport, &request) {
            Ok(result) => RpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: Some(result),
                error: None,
            },
            Err(error) => RpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: None,
                error: Some(error),
            },
        };

        write_frame(&mut output, &response)?;
    }

    Ok(())
}

/// Answers one request against the adapter's transport.
fn answer<T: Transport>(
    transport: &mut T,
    request: &RpcRequest,
) -> std::result::Result<serde_json::Value, RpcError> {
    let invalid = |reason: &str| RpcError {
        code: code::INVALID_PARAMS,
        message: reason.to_owned(),
        data: None,
    };

    let params = || -> std::result::Result<&serde_json::Value, RpcError> {
        request
            .params
            .as_ref()
            .ok_or_else(|| invalid("this method requires parameters"))
    };

    match request.method.as_str() {
        method::INITIALIZE => {
            let capabilities = transport.capabilities();
            Ok(serde_json::json!({
                "adapter": capabilities.adapter,
                "protocol": capabilities.protocol,
                "maxObjectBytes": capabilities.max_object_bytes,
                "maxObjectsPerPublication": capabilities.max_objects_per_publication,
                "durable": capabilities.durable,
                "historyModel": capabilities.history_model,
                "supportsWait": capabilities.supports_wait,
                "minPollIntervalSeconds": capabilities.min_poll_interval_seconds,
                "supportsLazyObjects": capabilities.supports_lazy_objects,
                "supportsGroupCreation": capabilities.supports_group_creation,
            }))
        }

        method::GROUP_OPEN => transport
            .open_group()
            .map(|state| {
                serde_json::json!({
                    "groupId": state.group_id,
                    "revision": state.revision,
                    "historyModel": state.history_model,
                })
            })
            .map_err(|error| error_object(&error)),

        method::GROUP_CREATE => {
            let objects = publish_objects(params()?).map_err(|reason| invalid(&reason))?;
            transport
                .create_group(objects)
                .map(|publication| {
                    serde_json::to_value(WirePublication::from_publication(&publication))
                        .unwrap_or_default()
                })
                .map_err(|error| error_object(&error))
        }

        method::PUBLISH => {
            let value = params()?;
            let class = value
                .get("publicationClass")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_publication_class)
                .ok_or_else(|| invalid("publicationClass is missing or unknown"))?;

            let publish = PublishRequest {
                expected_revision: value
                    .get("expectedRevision")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                class,
                objects: publish_objects(value).map_err(|reason| invalid(&reason))?,
            };

            transport
                .publish(publish)
                .map(|publication| {
                    serde_json::to_value(WirePublication::from_publication(&publication))
                        .unwrap_or_default()
                })
                .map_err(|error| error_object(&error))
        }

        method::FETCH => {
            let value = params()?;
            let after = value
                .get("afterRevision")
                .and_then(serde_json::Value::as_str);
            let limit = value
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| invalid("limit is required"))? as usize;

            transport
                .fetch(after, limit)
                .map(|page| {
                    serde_json::json!({
                        "publications": page
                            .publications
                            .iter()
                            .map(WirePublication::from_publication)
                            .collect::<Vec<_>>(),
                        "cursor": page.cursor,
                        "more": page.more,
                        "anomalies": page.anomalies,
                    })
                })
                .map_err(|error| error_object(&error))
        }

        method::GET_OBJECT => {
            let value = params()?;
            let name = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("name is required"))?;
            let sha256 = value
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("sha256 is required"))?;

            transport
                .get_object(name, sha256)
                .map(|bytes| serde_json::json!({ "bytes": canonical::encode_base64url(&bytes) }))
                .map_err(|error| error_object(&error))
        }

        method::HEALTH => transport
            .health()
            .map(|()| serde_json::json!({}))
            .map_err(|error| error_object(&error)),

        other => Err(RpcError {
            code: code::METHOD_NOT_FOUND,
            message: format!("{other} is not a method of hrc.transport/1"),
            data: None,
        }),
    }
}

/// Decodes the `objects` array shared by `group.create` and `publish`.
fn publish_objects(value: &serde_json::Value) -> std::result::Result<Vec<PublishObject>, String> {
    let objects = value
        .get("objects")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "objects is required".to_owned())?;

    objects
        .iter()
        .map(|object| {
            let wire: WireObject = serde_json::from_value(object.clone())
                .map_err(|error| format!("malformed object: {error}"))?;

            let class = parse_object_class(&wire.class)
                .ok_or_else(|| format!("unknown object class {}", wire.class))?;

            let encoded = wire
                .bytes
                .ok_or_else(|| format!("object {} carries no bytes", wire.name))?;
            let bytes = canonical::decode_base64url("bytes", &encoded)
                .map_err(|error| format!("object {} has undecodable bytes: {error}", wire.name))?;

            Ok(PublishObject {
                name: wire.name,
                class,
                bytes,
            })
        })
        .collect()
}

/// The pipes to a running adapter.
///
/// Behind a `RefCell` because half the [`Transport`] trait takes `&self`
/// while every call here writes to a pipe. That asymmetry is not a flaw in
/// the trait: `fetch` and `health` really are reads as far as the *channel*
/// is concerned, and an in-process adapter needs no mutation to serve them.
/// Talking over a pipe is this adapter's implementation detail, so it is
/// this adapter that carries the interior mutability rather than every
/// implementation paying for it in the signature.
struct Pipes {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

/// A [`Transport`] backed by a separate adapter executable.
///
/// The child is owned: dropping this sends `shutdown`, closes the pipes, and
/// waits, so an adapter does not outlive the core that started it.
pub struct AdapterProcess {
    child: Child,
    pipes: Option<std::cell::RefCell<Pipes>>,
    capabilities: AdapterCapabilities,
}

impl AdapterProcess {
    /// Spawns `command` and completes the `initialize` handshake.
    ///
    /// The handshake is not optional. An adapter that will not say what it
    /// guarantees cannot be used, because the core's batching and fetching
    /// decisions are made from those declarations rather than from an
    /// assumption about Git.
    pub fn spawn(mut command: Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Adapter diagnostics stay on the adapter's own stderr.
            // Capturing it here would mean either draining it on every call
            // or letting a chatty adapter fill a pipe and deadlock.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| {
                TransportError::Provider(format!("could not start adapter: {error}"))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::Provider("adapter stdin was not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::Provider("adapter stdout was not piped".into()))?;

        let mut adapter = Self {
            child,
            pipes: Some(std::cell::RefCell::new(Pipes {
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 1,
            })),
            // Replaced by the handshake below.
            capabilities: AdapterCapabilities {
                adapter: String::new(),
                protocol: String::new(),
                max_object_bytes: 0,
                max_objects_per_publication: 0,
                durable: false,
                history_model: LINEAR_APPEND_ONLY,
                supports_wait: false,
                min_poll_interval_seconds: 0,
                supports_lazy_objects: false,
                supports_group_creation: false,
            },
        };

        let announced = adapter.call(method::INITIALIZE, None)?;
        let capabilities = capabilities_from(&announced)?;

        if capabilities.protocol != hrc_protocol::TRANSPORT_PROTOCOL_ID {
            return Err(TransportError::Provider(format!(
                "adapter speaks {} but this build speaks {}",
                capabilities.protocol,
                hrc_protocol::TRANSPORT_PROTOCOL_ID
            )));
        }

        if capabilities.history_model != LINEAR_APPEND_ONLY {
            return Err(TransportError::Provider(
                "adapter does not declare a linear append-only history, so it cannot host a \
                 channel with mutable membership"
                    .into(),
            ));
        }

        adapter.capabilities = capabilities;
        Ok(adapter)
    }

    /// Sends one request and waits for its answer.
    fn call(&self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value> {
        let pipes = self
            .pipes
            .as_ref()
            .ok_or_else(|| TransportError::Provider("adapter has been shut down".into()))?;
        let mut pipes = pipes.borrow_mut();

        let id = pipes.next_id;
        pipes.next_id += 1;

        let request = RpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.to_owned(),
            params,
        };

        write_frame(&mut pipes.stdin, &request).map_err(|error| {
            TransportError::Provider(format!("could not send {method}: {error}"))
        })?;

        let response: RpcResponse = read_frame(&mut pipes.stdout)
            .map_err(|error| {
                TransportError::Provider(format!("could not read the answer to {method}: {error}"))
            })?
            .ok_or_else(|| {
                TransportError::Provider(format!("adapter exited without answering {method}"))
            })?;

        // A mismatched id means the stream is out of step, and every later
        // answer would be attributed to the wrong call.
        if response.id != id {
            return Err(TransportError::Provider(format!(
                "adapter answered request {} while {id} was outstanding",
                response.id
            )));
        }

        if let Some(error) = response.error {
            return Err(error_from_object(error));
        }

        response.result.ok_or_else(|| {
            TransportError::Provider(format!("{method} returned neither result nor error"))
        })
    }
}

impl Drop for AdapterProcess {
    fn drop(&mut self) {
        if self.pipes.is_some() {
            let _ = self.call(method::SHUTDOWN, None);
        }

        // Closing the pipes ends the adapter's read loop even if it ignored
        // the shutdown call, so a misbehaving adapter still goes away.
        self.pipes = None;
        let _ = self.child.wait();
    }
}

/// Reads the capability declaration returned by `initialize`.
fn capabilities_from(value: &serde_json::Value) -> Result<AdapterCapabilities> {
    let missing = |field: &str| TransportError::Provider(format!("initialize omitted {field}"));

    let text = |field: &'static str| -> Result<String> {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| missing(field))
    };
    let number = |field: &'static str| -> Result<u64> {
        value
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| missing(field))
    };
    let flag = |field: &'static str| -> Result<bool> {
        value
            .get(field)
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| missing(field))
    };

    // `history_model` is a `&'static str` in the trait, so a declared value
    // is matched against the models this build knows rather than passed
    // through. Anything else becomes a label the caller refuses.
    let history_model = match text("historyModel")?.as_str() {
        LINEAR_APPEND_ONLY => LINEAR_APPEND_ONLY,
        _ => "unsupported",
    };

    Ok(AdapterCapabilities {
        adapter: text("adapter")?,
        protocol: text("protocol")?,
        max_object_bytes: number("maxObjectBytes")?,
        max_objects_per_publication: number("maxObjectsPerPublication")? as usize,
        durable: flag("durable")?,
        history_model,
        supports_wait: flag("supportsWait")?,
        min_poll_interval_seconds: number("minPollIntervalSeconds")? as u32,
        supports_lazy_objects: flag("supportsLazyObjects")?,
        supports_group_creation: flag("supportsGroupCreation")?,
    })
}

/// Encodes objects for `group.create` and `publish`.
fn wire_objects(objects: &[PublishObject]) -> Vec<serde_json::Value> {
    objects
        .iter()
        .map(|object| {
            serde_json::json!({
                "name": object.name,
                "class": object.class.as_str(),
                "size": object.bytes.len() as u64,
                "sha256": canonical::sha256_hex(&object.bytes),
                "bytes": canonical::encode_base64url(&object.bytes),
            })
        })
        .collect()
}

impl Transport for AdapterProcess {
    fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    fn create_group(&mut self, objects: Vec<PublishObject>) -> Result<Publication> {
        let params = serde_json::json!({ "objects": wire_objects(&objects) });
        let result = self.call(method::GROUP_CREATE, Some(params))?;

        serde_json::from_value::<WirePublication>(result)
            .map_err(|error| {
                TransportError::Provider(format!("malformed group.create answer: {error}"))
            })?
            .into_publication()
    }

    fn open_group(&self) -> Result<GroupState> {
        let result = self.call(method::GROUP_OPEN, None)?;

        let history_model = match result
            .get("historyModel")
            .and_then(serde_json::Value::as_str)
        {
            Some(LINEAR_APPEND_ONLY) => LINEAR_APPEND_ONLY,
            _ => {
                return Err(TransportError::Provider(
                    "group.open did not report a linear append-only history".into(),
                ));
            }
        };

        Ok(GroupState {
            group_id: result
                .get("groupId")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| TransportError::Provider("group.open omitted groupId".into()))?
                .to_owned(),
            revision: result
                .get("revision")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            history_model,
        })
    }

    fn publish(&mut self, request: PublishRequest) -> Result<Publication> {
        // Validated locally before the call as well as by the adapter. A
        // structural rule the core depends on should not be enforceable only
        // by the least trusted party in the exchange.
        crate::validate_publication(&request)?;

        let params = serde_json::json!({
            "expectedRevision": request.expected_revision,
            "publicationClass": request.class.as_str(),
            "objects": wire_objects(&request.objects),
        });
        let result = self.call(method::PUBLISH, Some(params))?;

        serde_json::from_value::<WirePublication>(result)
            .map_err(|error| {
                TransportError::Provider(format!("malformed publish answer: {error}"))
            })?
            .into_publication()
    }

    fn fetch(&self, after: Option<&str>, limit: usize) -> Result<FetchPage> {
        let params = serde_json::json!({
            "afterRevision": after,
            "limit": limit as u64,
        });
        let result = self.call(method::FETCH, Some(params))?;

        let publications = result
            .get("publications")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| TransportError::Provider("fetch omitted publications".into()))?
            .iter()
            .map(|value| {
                serde_json::from_value::<WirePublication>(value.clone())
                    .map_err(|error| {
                        TransportError::Provider(format!("malformed publication: {error}"))
                    })
                    .and_then(WirePublication::into_publication)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(FetchPage {
            publications,
            cursor: result
                .get("cursor")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            more: result
                .get("more")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            // Anomalies are how an adapter reports a rewrite or a deletion
            // (PRD section 21.3), so a missing list is treated as none
            // rather than as an error that would hide the ones that came.
            anomalies: result
                .get("anomalies")
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    fn get_object(&self, name: &str, expected_sha256: &str) -> Result<Vec<u8>> {
        let params = serde_json::json!({ "name": name, "sha256": expected_sha256 });
        let result = self.call(method::GET_OBJECT, Some(params))?;

        let encoded = result
            .get("bytes")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| TransportError::Provider("get_object omitted bytes".into()))?;

        let bytes = canonical::decode_base64url("bytes", encoded)
            .map_err(|error| TransportError::Provider(format!("undecodable object: {error}")))?;

        // Checked here as well as in the adapter. An adapter is the party
        // least entitled to be believed about whether it returned the bytes
        // it was asked for, and section 26 treats a modified object as a
        // security event rather than a retryable error.
        if canonical::sha256_hex(&bytes) != expected_sha256 {
            return Err(TransportError::ObjectHashMismatch {
                name: name.to_owned(),
            });
        }

        Ok(bytes)
    }

    fn health(&self) -> Result<()> {
        self.call(method::HEALTH, None).map(|_| ())
    }
}

#[cfg(test)]
mod tests;
