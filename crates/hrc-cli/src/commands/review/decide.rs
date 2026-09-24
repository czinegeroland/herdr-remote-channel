//! The parts of a review decision that happen after the screen closes.
//!
//! PRD section 11.6 gives the receiver five choices, two of which need more
//! than a key press: editing before delivery needs a text editor, and
//! declining may carry a reason. Both run here, on the ordinary terminal the
//! review screen has just handed back, and both can still be abandoned: an
//! edit that is empty or unchanged delivers nothing, and an empty reason is
//! no reason.

use std::io::{BufRead, Write};

use super::*;

/// Opens `original` in the person's editor and returns what they saved.
///
/// `None` when nothing should be delivered: the editor could not run or
/// failed, or the text came back empty or unchanged. An unchanged edit is
/// not delivered as an edit, because recording "edited" for content nobody
/// changed would make the audit say something that did not happen.
///
/// The draft is written to this installation's private runtime directory,
/// which only its owner can read, and removed as soon as the editor exits.
/// It holds the same plaintext the local inbox already stores.
pub(super) fn edit_in_editor(context: &Context, original: &str) -> Result<Option<String>> {
    let runtime = context.paths.runtime();
    hrc_ipc::endpoint::prepare_runtime_dir(&runtime).map_err(|source| CliError::Io {
        action: "prepare a private place for the edit",
        source: std::io::Error::other(source.to_string()),
    })?;

    let draft = runtime.join(format!("edit-{}.md", std::process::id()));
    write_private(&draft, original)?;

    let ran = run_editor(&draft);
    let edited = std::fs::read_to_string(&draft);
    let _ = std::fs::remove_file(&draft);

    if !ran {
        eprintln!("The editor did not finish cleanly, so nothing was delivered.");
        return Ok(None);
    }

    let edited = edited.map_err(|source| CliError::Io {
        action: "read back the edited message",
        source,
    })?;

    Ok(worth_delivering(original, &edited))
}

/// What an edit should deliver, if anything.
pub(super) fn worth_delivering(original: &str, edited: &str) -> Option<String> {
    let trimmed = edited.trim_end_matches(['\n', '\r']);
    if trimmed.trim().is_empty() || trimmed == original.trim_end_matches(['\n', '\r']) {
        return None;
    }

    Some(trimmed.to_owned())
}

/// Asks, on the terminal, for a yes to deliver the edit.
pub(super) fn confirm_edit(original: &str, edited: &str, destination: &str) -> bool {
    let before = original.lines().count();
    let after = edited.lines().count();
    ask(&format!(
        "Deliver your edit ({after} lines, from {before}) to {destination}? [y/N] "
    ))
    .is_some_and(|answer| matches!(answer.trim(), "y" | "Y"))
}

/// Asks, on the terminal, for an optional reason to send back.
///
/// The reason is the person's own words to the sender, sent as an ordinary
/// reply in the declined message's thread. Nothing is sent when they leave
/// it empty.
pub(super) fn decline_reason() -> Option<String> {
    ask("Reason to send back to the sender (Enter to send none): ")
        .map(|reason| reason.trim().to_owned())
        .filter(|reason| !reason.is_empty())
}

/// One line from the terminal, or `None` if there is none to read.
fn ask(prompt: &str) -> Option<String> {
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "{prompt}");
    let _ = stdout.flush();

    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// Runs `$VISUAL`, then `$EDITOR`, then the platform's plain editor, on
/// `path`, and says whether it exited successfully.
///
/// The variable may carry arguments, such as `code --wait`, so it is split on
/// whitespace. It is the person's own configuration on their own machine;
/// nothing a sender wrote reaches it.
fn run_editor(path: &std::path::Path) -> bool {
    let configured = std::env::var("VISUAL")
        .ok()
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|value| !value.trim().is_empty());
    let fallback = if cfg!(windows) { "notepad" } else { "vi" };
    let command = configured.unwrap_or_else(|| fallback.to_owned());

    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        return false;
    };

    std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

/// Writes `text` to a file only its owner can read.
fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|source| CliError::Io {
        action: "write the message to edit",
        source,
    })?;
    file.write_all(text.as_bytes())
        .map_err(|source| CliError::Io {
            action: "write the message to edit",
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_real_change_is_delivered_as_an_edit() {
        let original = "Please check the retry loop.\n";

        assert_eq!(worth_delivering(original, original), None, "unchanged");
        assert_eq!(
            worth_delivering(original, "Please check the retry loop."),
            None,
            "a trailing newline is not an edit"
        );
        assert_eq!(worth_delivering(original, "  \n\n"), None, "emptied");
        assert_eq!(
            worth_delivering(original, "Please check the retry loop only.\n"),
            Some("Please check the retry loop only.".to_owned())
        );
    }
}
