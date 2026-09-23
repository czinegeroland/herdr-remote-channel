//! Channel invitation links (PRD sections 15 and 23.6).
//!
//! Herdr's `[[link_handlers]]` routes a modified click on a terminal URL to
//! a plugin action instead of the browser, handing the action the clicked
//! URL. This plugin declared none, which left enrolment the part of the
//! product a person had to be walked through: the invitee receives a
//! repository locator and an invite code out of band and retypes both into a
//! form.
//!
//! One of those two is safe to automate and the other is not.
//!
//! The **locator** is public. It is the URL of the repository backing the
//! channel, it is long, it is exactly the field a person mistypes, and
//! nothing about knowing it grants access — enrolment still requires an
//! invite the administrator issued and an approval the administrator gives
//! after comparing a safety phrase. So a link carries it.
//!
//! The **invite code** is a secret, and a URL is the worst place a secret
//! can be. It lands in terminal scrollback, in shell history if anyone pipes
//! it, in a clipboard manager, and in whatever the link was pasted into on
//! the way over. Worse, the gesture that redeems it would be a click, which
//! is not the moment to be deciding whether a link is genuine. The code is
//! never in a link; the join screen opens with that field empty and waiting.
//!
//! The marker matters too. A handler whose pattern matched bare repository
//! URLs would take over modified clicks on every GitHub link in every pane
//! — a plugin quietly stealing a gesture it does not own, which is the same
//! mistake as taking over the terminal window title. So a link is only ours
//! when it says so, with a fragment the invitation prints and nothing else
//! produces.

/// The fragment that marks a locator as an invitation to join.
///
/// A fragment rather than a query parameter: a fragment is not sent to a
/// server, so a link that ends up pasted into a browser fetches the ordinary
/// repository page and tells nobody that this URL is also a channel.
pub const JOIN_FRAGMENT: &str = "#hrc-join";

/// The regular expression Herdr matches clicked URLs against.
///
/// Anchored at both ends and requiring the fragment, so it claims modified
/// clicks on invitation links and on nothing else.
pub const JOIN_PATTERN: &str = r"^https?://\S+#hrc-join$";

/// The environment variable Herdr sets to the clicked URL.
pub const CLICKED_URL_ENV: &str = "HERDR_PLUGIN_CLICKED_URL";

/// The longest URL this build will read.
///
/// A clicked URL arrives from a pane, and a pane shows whatever a program
/// wrote to it, including something another person sent. It is not trusted
/// input: it is bounded, checked, and used only to prefill a field a human
/// then looks at.
const MAX_URL: usize = 512;

/// Builds the invitation link for one channel locator.
pub fn join_link(locator: &str) -> String {
    format!("{}{JOIN_FRAGMENT}", locator.trim_end_matches('/'))
}

/// The locator inside an invitation link, if it is one.
///
/// Returns `None` for anything that is not an `http` or `https` URL carrying
/// the join fragment. The check is deliberately strict and deliberately
/// small: what it produces is prefilled into a form, and the human who
/// clicked still has to type an invite code and still has to have their
/// request approved by an administrator comparing a safety phrase. It
/// authorizes nothing.
pub fn locator_from(clicked: &str) -> Option<String> {
    let clicked = clicked.trim();

    if clicked.len() > MAX_URL || clicked.chars().any(char::is_control) {
        return None;
    }

    let locator = clicked.strip_suffix(JOIN_FRAGMENT)?;

    if !(locator.starts_with("https://") || locator.starts_with("http://")) {
        return None;
    }

    // A URL is `scheme://host/path`; anything with no host is not one, and a
    // locator that is only a scheme would prefill a field with nonsense.
    let rest = locator
        .strip_prefix("https://")
        .or_else(|| locator.strip_prefix("http://"))?;
    if rest.is_empty() || rest.starts_with('/') {
        return None;
    }

    Some(locator.to_owned())
}

#[cfg(test)]
mod tests;
