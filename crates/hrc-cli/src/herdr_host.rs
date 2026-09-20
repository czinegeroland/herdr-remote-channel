//! The connection to the Herdr server.
//!
//! `hrc-herdr` builds the request lines and reads the answers; this opens the
//! socket they travel over. Blocking rather than async on purpose: every
//! caller is a human sitting in a terminal screen waiting for one answer, and
//! a runtime would be machinery for a concurrency that does not exist here.
//!
//! The socket is named by `HERDR_SOCKET_PATH`, which Herdr sets on every
//! plugin process. Resolving the default path instead would talk to whichever
//! server owns the default socket, which in a named session is not the one
//! that launched this pane.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, ToFsName};

use hrc_herdr::HostError;
use hrc_herdr::host;

/// How long to wait for the server to answer one request.
///
/// Herdr answers `agent.list` and `agent.prompt` from memory, so a second is
/// already generous. What this bounds is the case that matters: a server that
/// accepted the connection and then stopped answering would otherwise hang a
/// modal popup the person cannot dismiss.
const TIMEOUT: Duration = Duration::from_secs(5);

/// What went wrong reaching Herdr.
#[derive(Debug)]
pub enum Unreachable {
    /// No `HERDR_SOCKET_PATH`, so this is not running under Herdr.
    NotUnderHerdr,
    /// The socket is named but could not be used.
    Io(std::io::Error),
    /// Herdr answered with something this build cannot use.
    Host(HostError),
}

impl std::fmt::Display for Unreachable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unreachable::NotUnderHerdr => write!(formatter, "not running under Herdr"),
            Unreachable::Io(error) => write!(formatter, "could not reach Herdr: {error}"),
            Unreachable::Host(error) => write!(formatter, "{error}"),
        }
    }
}

/// One connection to the Herdr server, carrying one request at a time.
pub struct Host {
    reader: BufReader<interprocess::local_socket::Stream>,
    /// Distinguishes this process's requests from anything else on the
    /// connection, and increments so two requests cannot share an `id`.
    next: u64,
}

impl Host {
    /// Connects to the server that launched this process.
    pub fn connect() -> Result<Self, Unreachable> {
        let path = std::env::var(host::SOCKET_ENV).map_err(|_| Unreachable::NotUnderHerdr)?;
        if path.is_empty() {
            return Err(Unreachable::NotUnderHerdr);
        }

        let name = path
            .as_str()
            .to_fs_name::<GenericFilePath>()
            .map_err(Unreachable::Io)?;
        let stream = interprocess::local_socket::Stream::connect(name).map_err(Unreachable::Io)?;
        stream
            .set_recv_timeout(Some(TIMEOUT))
            .map_err(Unreachable::Io)?;
        stream
            .set_send_timeout(Some(TIMEOUT))
            .map_err(Unreachable::Io)?;

        Ok(Self {
            reader: BufReader::new(stream),
            next: 0,
        })
    }

    /// The pane this process runs in, when Herdr set one.
    ///
    /// A popup does not get one, which is why the destination list falls back
    /// to whichever agent Herdr reports as focused.
    pub fn calling_pane() -> Option<String> {
        std::env::var(host::PANE_ENV)
            .ok()
            .filter(|id| !id.is_empty())
    }

    /// Every local agent a delivery could go to.
    pub fn destinations(&mut self) -> Result<Vec<hrc_herdr::LocalAgent>, Unreachable> {
        let id = self.identifier();
        let line = self.exchange(&id, host::agent_list(&id))?;
        host::agents(&line, &id, Self::calling_pane().as_deref()).map_err(Unreachable::Host)
    }

    /// Submits approved text to one agent, and says whether Herdr took it.
    pub fn deliver(&mut self, target: &str, text: &str) -> Result<(), Unreachable> {
        let id = self.identifier();
        let line = self.exchange(&id, host::agent_prompt(&id, target, text))?;
        host::accepted(&line, &id, "agent_prompted").map_err(Unreachable::Host)
    }

    /// Raises one notification through Herdr.
    ///
    /// The answer says whether it was shown and why not when it was not, but
    /// a notification that did not appear is not a reason to fail anything:
    /// the message is in the inbox either way, and that is the durable
    /// record. So this reports success or failure and the caller decides,
    /// which for the side view means carrying on.
    pub fn notify(&mut self, title: &str, urgent: bool) -> Result<(), Unreachable> {
        let id = self.identifier();
        let line = self.exchange(&id, host::notify(&id, title, urgent))?;
        host::accepted(&line, &id, "notification_show").map_err(Unreachable::Host)
    }

    /// Places the inbox as a split, returning the pane Herdr created.
    pub fn open_inbox(&mut self) -> Result<String, Unreachable> {
        let id = self.identifier();
        let line = self.exchange(&id, host::open_inbox(&id))?;
        host::opened_pane(&line, &id).map_err(Unreachable::Host)
    }

    /// Narrows a pane to a quarter of its split.
    ///
    /// A failure is the pane staying the size Herdr chose, which is usable.
    /// Reporting it rather than raising keeps a startup hook from failing
    /// over a layout preference.
    pub fn narrow(&mut self, pane_id: &str, share: f64) -> Result<(), Unreachable> {
        let id = self.identifier();
        let line = self.exchange(&id, host::narrow(&id, pane_id, share))?;
        host::result(&line, &id)
            .map(|_| ())
            .map_err(Unreachable::Host)
    }

    /// Reports, or clears, the count beside a pane in Herdr's sidebar.
    ///
    /// `None` clears it. The token carries a short time to live, so a pane
    /// that stops reporting -- because its process died rather than because
    /// the count reached zero -- loses its claim on the host's sidebar
    /// without anything having to notice and clean up after it.
    pub fn report_token(&mut self, pane_id: &str, value: Option<&str>) -> Result<(), Unreachable> {
        let id = self.identifier();
        let line = self.exchange(&id, host::pane_token(&id, pane_id, value))?;
        host::result(&line, &id)
            .map(|_| ())
            .map_err(Unreachable::Host)
    }

    /// Sets, or restores, the terminal window title.
    pub fn set_window_title(&mut self, title: Option<&str>) -> Result<(), Unreachable> {
        let id = self.identifier();
        let request = match title {
            Some(title) => host::window_title(&id, title),
            None => host::clear_window_title(&id),
        };

        let line = self.exchange(&id, request)?;
        host::result(&line, &id)
            .map(|_| ())
            .map_err(Unreachable::Host)
    }

    /// Whether a pane Herdr once gave us is still open.
    ///
    /// A missing pane answers with an error rather than a negative, so the
    /// refusal is read as "gone" and anything else as "there". Treating an
    /// unreadable answer as "gone" would reopen the inbox beside one that is
    /// already on screen, which is the failure this check exists to prevent.
    pub fn pane_is_open(&mut self, pane_id: &str) -> bool {
        let id = self.identifier();
        let Ok(line) = self.exchange(&id, host::pane(&id, pane_id)) else {
            return true;
        };

        host::result(&line, &id).is_ok()
    }

    /// An identifier no other request on this connection uses.
    fn identifier(&mut self) -> String {
        self.next += 1;
        format!("hrc-{}-{}", std::process::id(), self.next)
    }

    /// Writes one request line and reads lines until the matching answer.
    ///
    /// A subscription event can arrive on a connection between a request and
    /// its reply, so a line that is not this request's answer is skipped
    /// rather than treated as one. Bounded, because a server that only ever
    /// sends events would otherwise keep this loop running inside a modal
    /// popup.
    fn exchange(&mut self, id: &str, request: String) -> Result<String, Unreachable> {
        self.reader
            .get_mut()
            .write_all(request.as_bytes())
            .map_err(Unreachable::Io)?;
        self.reader.get_mut().flush().map_err(Unreachable::Io)?;

        for _ in 0..MAX_LINES {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).map_err(Unreachable::Io)?;
            if read == 0 {
                return Err(Unreachable::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Herdr closed the connection",
                )));
            }

            if !matches!(host::result(&line, id), Err(HostError::Mismatched)) {
                return Ok(line);
            }
        }

        Err(Unreachable::Host(HostError::Mismatched))
    }
}

/// How many unrelated lines to skip before giving up on an answer.
const MAX_LINES: usize = 64;
