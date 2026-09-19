//! Talking to the Herdr server, as data rather than as I/O.
//!
//! Herdr's socket API is newline-delimited JSON over a Unix domain socket or
//! a Windows named pipe: one request per line, one response carrying the same
//! `id`. This module builds those lines and reads the answers. It opens
//! nothing — the crate's rule is that every function here is a pure function
//! of what the caller passes in, and the socket lives in `hrc-cli` with the
//! rest of the I/O.
//!
//! Two things are load-bearing about going through the socket rather than
//! through `herdr agent prompt`:
//!
//! * **An approved body never reaches a command line.** `agent prompt` takes
//!   its text as an argument, and an argument is readable from the process
//!   table for as long as the process lives. A message a person decrypted
//!   behind a modal popup would then be readable by anything that can run
//!   `ps`. Over the socket the text is in a request body, between two
//!   processes, and never enumerable.
//! * **The response says what happened.** `agent.prompt` answers
//!   `agent_blocked` when the agent cannot take input, so a delivery that did
//!   not land is distinguishable from one that did, and the message can stay
//!   pending rather than being recorded as delivered.

use serde_json::{Value, json};

use crate::agent::LocalAgent;

/// The environment variable Herdr sets on every plugin process, naming the
/// socket of the server that launched it.
///
/// Used rather than resolving the default path, so a plugin running in a
/// named session talks to that session's server rather than to whichever one
/// owns the default socket.
pub const SOCKET_ENV: &str = "HERDR_SOCKET_PATH";

/// The environment variable naming the pane a plugin command runs in.
pub const PANE_ENV: &str = "HERDR_PANE_ID";

/// Something the host said that this build cannot use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// The line was not JSON.
    Unreadable,
    /// The response carried a different `id` than the request.
    ///
    /// Newline-delimited JSON on one connection can interleave a
    /// subscription event with a reply, so the `id` is checked rather than
    /// assumed. Acting on the wrong response is how a delivery gets reported
    /// against a message it never reached.
    Mismatched,
    /// Herdr refused, and said why.
    Refused {
        /// The host's own error code, such as `agent_blocked`.
        code: String,
    },
    /// The response was shaped in a way this build does not understand.
    Unexpected,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Unreadable => write!(formatter, "Herdr sent something that is not JSON"),
            HostError::Mismatched => write!(formatter, "Herdr answered a different request"),
            HostError::Refused { code } => write!(formatter, "Herdr refused: {code}"),
            HostError::Unexpected => write!(formatter, "Herdr sent an answer of an unknown shape"),
        }
    }
}

impl std::error::Error for HostError {}

/// One request line, newline included.
///
/// Every request is built here rather than formatted at the call site, so
/// the `id` a caller checks the response against is the one it sent.
pub fn request(id: &str, method: &str, params: Value) -> String {
    format!(
        "{}\n",
        json!({ "id": id, "method": method, "params": params })
    )
}

/// Asks Herdr for every agent it knows about.
pub fn agent_list(id: &str) -> String {
    request(id, "agent.list", json!({}))
}

/// Submits approved text to one agent.
///
/// No `wait`: this returns once Herdr has accepted the submission, because
/// the human is sitting in a modal popup and a coding agent's turn can run
/// for minutes. What must not happen is the popup reporting success before
/// Herdr accepted it, and that is what checking the response is for.
pub fn agent_prompt(id: &str, target: &str, text: &str) -> String {
    request(
        id,
        "agent.prompt",
        json!({ "target": target, "text": text }),
    )
}

/// Raises one notification through Herdr.
///
/// `sound` is `request` for something that needs the person now and `none`
/// otherwise, which is how an ordinary arrival avoids stealing attention
/// while a tamper halt does not go unheard. There is no body: the title
/// carries fixed local wording and Herdr caps it at 80 characters, and a
/// second line would be a second place for text to reach a screen.
pub fn notify(id: &str, title: &str, urgent: bool) -> String {
    request(
        id,
        "notification.show",
        json!({
            "title": title,
            "sound": if urgent { "request" } else { "none" },
        }),
    )
}

/// Reads one response line, returning its `result` when it succeeded.
pub fn result(line: &str, expected_id: &str) -> Result<Value, HostError> {
    let response: Value = serde_json::from_str(line).map_err(|_| HostError::Unreadable)?;

    if response.get("id").and_then(Value::as_str) != Some(expected_id) {
        return Err(HostError::Mismatched);
    }

    if let Some(error) = response.get("error") {
        return Err(HostError::Refused {
            code: error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        });
    }

    response.get("result").cloned().ok_or(HostError::Unexpected)
}

/// Confirms that a response is the successful result of the named kind.
pub fn accepted(line: &str, expected_id: &str, expected_type: &str) -> Result<(), HostError> {
    let result = result(line, expected_id)?;

    match result.get("type").and_then(Value::as_str) {
        Some(kind) if kind == expected_type => Ok(()),
        _ => Err(HostError::Unexpected),
    }
}

/// Reads an `agent.list` response into delivery destinations.
///
/// `current_pane` is this process's own `HERDR_PANE_ID`, which marks the
/// session the person is working out of. When Herdr did not set one — a
/// popup does not get a pane ID — the agent Herdr reports as focused takes
/// that place instead.
///
/// An entry without a usable pane is dropped rather than shown with a blank
/// name. A destination that cannot be addressed is not a destination, and
/// listing it would let a person confirm a delivery that had nowhere to go.
pub fn agents(
    line: &str,
    expected_id: &str,
    current_pane: Option<&str>,
) -> Result<Vec<LocalAgent>, HostError> {
    let result = result(line, expected_id)?;

    if result.get("type").and_then(Value::as_str) != Some("agent_list") {
        return Err(HostError::Unexpected);
    }

    let listed = result
        .get("agents")
        .and_then(Value::as_array)
        .ok_or(HostError::Unexpected)?;

    let mut destinations: Vec<LocalAgent> = listed
        .iter()
        .filter_map(|entry| destination(entry, current_pane))
        .collect();

    // The session the person is working in comes first. It is the one they
    // most often mean, and putting it at the top is not the same as choosing
    // it for them: the screen still opens with nothing proposed, and
    // delivering still takes a reveal, a proposal and a confirmation.
    destinations.sort_by_key(|agent| !agent.is_current());

    Ok(destinations)
}

/// Builds one destination from an `AgentInfo`, or nothing.
fn destination(entry: &Value, current_pane: Option<&str>) -> Option<LocalAgent> {
    let pane_id = text(entry, "pane_id")?;

    // `agent.prompt` resolves a unique live agent name or the pane hosting
    // it. The name is preferred because it follows the agent; the pane is
    // what an unnamed agent has.
    let name = text(entry, "name");
    let target = name.clone().unwrap_or_else(|| pane_id.clone());

    // The label is chosen from host-supplied fields only, and deliberately
    // not from `title`: a terminal title is whatever the program running in
    // the pane last wrote, which on this screen would mean remote output
    // choosing how a destination reads.
    let label = name
        .or_else(|| text(entry, "display_agent"))
        .or_else(|| text(entry, "agent"))
        .unwrap_or_else(|| pane_id.clone());

    let current = match current_pane {
        Some(current_pane) => pane_id == current_pane,
        None => entry
            .get("focused")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };

    // Herdr refuses a prompt to a blocked agent outright, and an agent that
    // is not interactive yet has nothing to receive it. Both are reported
    // rather than filtered out, so the screen can say why.
    let ready = entry
        .get("interactive_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && entry.get("agent_status").and_then(Value::as_str) != Some("blocked");

    Some(
        LocalAgent::new(pane_id, label)
            .with_target(target)
            .in_workspace(text(entry, "workspace_id"))
            .as_current(current)
            .when_ready(ready),
    )
}

/// A non-empty string field, or nothing.
fn text(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests;
