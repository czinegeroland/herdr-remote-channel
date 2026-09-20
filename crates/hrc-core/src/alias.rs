//! Locally assigned display names for verified principals.
//!
//! An alias is the text the approval screen asks a human to recognize before
//! a body is released, so what may become one is decided here rather than in
//! whichever interface happens to be collecting it. PRD section 16.4 makes
//! the same argument about the publication phrase: a check that lives in the
//! interface is a check the next interface can ship a weaker version of.

/// The longest locally assigned display name a surface will store.
///
/// Long enough for a full name, short enough that no alias can push a row
/// past the frame on its own. The renderer still clamps to the width it has;
/// this is the ceiling on what may be written in the first place, so a name
/// that cannot be drawn anywhere is refused at the point a human types it
/// rather than silently truncated every time it is read.
pub const MAX_ALIAS: usize = 32;

/// Why a proposed display name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasError {
    /// Nothing but whitespace.
    Empty,
    /// Longer than [`MAX_ALIAS`] characters.
    TooLong,
    /// Contains a control character.
    Control,
}

impl AliasError {
    /// Fixed local wording for the refusal.
    pub const fn as_str(self) -> &'static str {
        match self {
            AliasError::Empty => "a display name cannot be blank",
            AliasError::TooLong => "a display name is at most 32 characters",
            AliasError::Control => "a display name cannot contain control characters",
        }
    }
}

/// Checks a proposed display name and returns the form that gets stored.
///
/// Control characters are refused rather than stripped. This name is typed
/// by the local human, so a rejection is a typo they can see and fix, while
/// a silent repair would store something other than what they typed into the
/// field the gate asks them to recognize. Escape sequences in a string the
/// inbox draws every second are also the classic way to make a terminal draw
/// something other than what is in the buffer.
pub fn check_alias(proposed: &str) -> Result<String, AliasError> {
    let trimmed = proposed.trim();

    if trimmed.is_empty() {
        return Err(AliasError::Empty);
    }
    if trimmed.chars().count() > MAX_ALIAS {
        return Err(AliasError::TooLong);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(AliasError::Control);
    }

    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrounding_whitespace_is_not_part_of_a_name() {
        assert_eq!(check_alias("  Alice  ").unwrap(), "Alice");
    }

    #[test]
    fn a_blank_name_is_refused() {
        assert_eq!(check_alias("   "), Err(AliasError::Empty));
        assert_eq!(check_alias(""), Err(AliasError::Empty));
    }

    #[test]
    fn a_name_longer_than_the_ceiling_is_refused() {
        let name = "a".repeat(MAX_ALIAS);
        assert!(check_alias(&name).is_ok());
        assert_eq!(check_alias(&format!("{name}a")), Err(AliasError::TooLong));
    }

    #[test]
    fn control_characters_are_refused_rather_than_stripped() {
        // An escape sequence in a string the inbox redraws every second is
        // how a terminal is made to display something other than what is in
        // the buffer. Repairing it silently would store a name other than
        // the one the human typed into the field the gate asks them to
        // recognize.
        assert_eq!(check_alias("Al\u{1b}[31mice"), Err(AliasError::Control));
        assert_eq!(check_alias("Alice\u{7}"), Err(AliasError::Control));
        assert_eq!(check_alias("Al\nice"), Err(AliasError::Control));
    }

    #[test]
    fn the_ceiling_counts_characters_rather_than_bytes() {
        // Thirty-two multi-byte characters is a reasonable name and ninety-six
        // bytes. Counting bytes would refuse it.
        assert!(check_alias(&"é".repeat(MAX_ALIAS)).is_ok());
    }
}
