//! The membership screen: removing a member, revoking a device.
//!
//! PRD requirement HRC-CH-008. Both are section 22.7 operations, and both are
//! irreversible in the way that matters: a control entry is published to an
//! append-only history and the roster epoch advances, so undoing one means
//! admitting the same person again rather than taking anything back.
//!
//! That is why this is a separate screen from the join one. Admitting someone
//! is a judgement about identity, checked against a safety phrase. Removing
//! someone is a judgement about trust, and the thing a human needs in front
//! of them is not a phrase but the consequences: which devices go with the
//! principal, and whether they are about to lock themselves out.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One device belonging to a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDevice {
    /// The device identifier.
    pub device_id: String,
    /// Whether the roster still lists it as active.
    pub active: bool,
}

/// One member of the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The verified principal.
    pub principal_id: String,
    /// Whether they administer the channel.
    pub administrator: bool,
    /// Whether the roster still lists them as active.
    pub active: bool,
    /// Their devices.
    pub devices: Vec<MemberDevice>,
    /// Whether this is the local principal.
    ///
    /// Supplied rather than guessed, and the reason the screen can refuse to
    /// let someone remove themselves.
    pub is_local: bool,
    /// What this installation currently calls them, if anything.
    ///
    /// `None` is not the same as an empty name: it means nobody has written
    /// one down, which is the state this screen exists to let a person fix
    /// while they are looking at the principal ID and the devices behind it.
    pub display_name: Option<String>,
}

/// What the human decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberOutcome {
    /// Remove a principal from the channel.
    Remove {
        /// Which principal.
        principal_id: String,
    },
    /// Revoke one device.
    Revoke {
        /// Which device.
        device_id: String,
    },
    /// Record, or forget, what this installation calls a member.
    ///
    /// Unlike the other two this publishes nothing and advances no epoch. It
    /// belongs on this screen anyway, because this is where a human is
    /// already looking at a principal and deciding what they think of it.
    Name {
        /// Which principal.
        principal_id: String,
        /// The name to store, or `None` to forget the one on record.
        display_name: Option<String>,
    },
    /// Leave without changing anything.
    Quit,
}

/// Which list has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberFocus {
    /// Choosing a member.
    Member,
    /// Choosing one of their devices.
    Device,
}

/// The membership screen.
#[derive(Debug, Clone)]
pub struct MembersApp {
    members: Vec<Member>,
    selected_member: usize,
    selected_device: usize,
    focus: MemberFocus,
    proposed: Option<(MemberOutcome, String)>,
    editing: Option<String>,
    status: String,
}

impl MembersApp {
    /// The key hints this screen returns to when nothing is in progress.
    const KEYS: &'static str =
        "Tab: devices   n: name   r: remove member   v: revoke device   q: leave";

    /// Builds the screen over the published roster.
    pub fn new(members: Vec<Member>) -> Self {
        Self {
            members,
            selected_member: 0,
            selected_device: 0,
            focus: MemberFocus::Member,
            proposed: None,
            editing: None,
            status: "Tab: devices   n: name   r: remove member   v: revoke device   q: leave"
                .into(),
        }
    }

    /// The members on screen.
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// The selected member, if there is one.
    pub fn selected_member(&self) -> Option<&Member> {
        self.members.get(self.selected_member)
    }

    /// The selected device, if there is one.
    pub fn selected_device(&self) -> Option<&MemberDevice> {
        self.selected_member()?.devices.get(self.selected_device)
    }

    /// The index of the selected member.
    pub fn selected_member_index(&self) -> usize {
        self.selected_member
    }

    /// The index of the selected device.
    pub fn selected_device_index(&self) -> usize {
        self.selected_device
    }

    /// Where the keyboard is.
    pub fn focus(&self) -> MemberFocus {
        self.focus
    }

    /// The change awaiting confirmation, if any.
    pub fn proposed(&self) -> Option<&str> {
        self.proposed.as_ref().map(|(_, prompt)| prompt.as_str())
    }

    /// The name being typed, if the screen is collecting one.
    ///
    /// `Some("")` is a real state and differs from `None`: the field is open
    /// and empty, which is how a person clears a name they no longer want.
    pub fn editing(&self) -> Option<&str> {
        self.editing.as_deref()
    }

    /// The line telling the human what to do next.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Handles one key press.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<MemberOutcome> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(MemberOutcome::Quit);
        }

        if self.proposed.is_some() {
            return self.confirm(key);
        }

        if self.editing.is_some() {
            return self.type_name(key);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(MemberOutcome::Quit),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    MemberFocus::Member => MemberFocus::Device,
                    MemberFocus::Device => MemberFocus::Member,
                };
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                None
            }
            KeyCode::Char('n') => {
                self.begin_naming();
                None
            }
            KeyCode::Char('r') => {
                self.propose_removal();
                None
            }
            KeyCode::Char('v') => {
                self.propose_revocation();
                None
            }
            _ => None,
        }
    }

    /// Opens the name field on the selected member.
    ///
    /// Pre-filled with the name on record so that correcting a typo does not
    /// mean retyping the whole thing, and so that the field shows what is
    /// being replaced rather than implying there was nothing there.
    fn begin_naming(&mut self) {
        let Some(member) = self.selected_member() else {
            return;
        };

        self.editing = Some(member.display_name.clone().unwrap_or_default());
        self.status = "Type a name   Enter: save   empty: clear   Esc: cancel".into();
    }

    /// Collects one key of a name.
    ///
    /// Only ordinary characters are taken. A control character is ignored
    /// here rather than stored and refused later, so the field never shows a
    /// person something the daemon is going to reject.
    fn type_name(&mut self, key: KeyEvent) -> Option<MemberOutcome> {
        let typed = self.editing.as_mut()?;

        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.status = Self::KEYS.into();
                None
            }
            KeyCode::Backspace => {
                typed.pop();
                None
            }
            KeyCode::Char(character) if !character.is_control() => {
                if typed.chars().count() < hrc_core::alias::MAX_ALIAS {
                    typed.push(character);
                }
                None
            }
            KeyCode::Enter => {
                let typed = self.editing.take()?;
                let trimmed = typed.trim();
                let principal_id = self.selected_member()?.principal_id.clone();

                self.status = Self::KEYS.into();

                Some(MemberOutcome::Name {
                    principal_id,
                    display_name: if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_owned())
                    },
                })
            }
            _ => None,
        }
    }

    /// Answers the confirmation. Only `y` proceeds.
    fn confirm(&mut self, key: KeyEvent) -> Option<MemberOutcome> {
        let (outcome, _) = self.proposed.take()?;

        if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            return Some(outcome);
        }

        self.status = "Cancelled. Nothing was changed.".into();
        None
    }

    /// Moves whichever list has the keyboard.
    fn move_selection(&mut self, delta: isize) {
        let length = match self.focus {
            MemberFocus::Member => self.members.len(),
            MemberFocus::Device => self.selected_member().map_or(0, |m| m.devices.len()),
        };

        if length == 0 {
            return;
        }

        let index = match self.focus {
            MemberFocus::Member => &mut self.selected_member,
            MemberFocus::Device => &mut self.selected_device,
        };

        *index = match delta {
            d if d < 0 => index.saturating_sub(1),
            _ => (*index + 1).min(length - 1),
        };

        if self.focus == MemberFocus::Member {
            // A device index from the previous member means nothing here.
            self.selected_device = 0;
        }

        self.proposed = None;
    }

    /// Proposes removing the selected member.
    fn propose_removal(&mut self) {
        let Some(member) = self.selected_member() else {
            return;
        };

        // Removing yourself publishes a control entry revoking the authority
        // that signed it. The channel would carry on without an administrator
        // who can act, and nothing in this screen could undo it.
        if member.is_local {
            self.status =
                "That is this installation. Removing yourself would leave the channel with no way \
                 back in."
                    .into();
            return;
        }

        if !member.active {
            self.status = "That member has already been removed.".into();
            return;
        }

        // The device count is named because removing a principal takes every
        // device with it, and a person looking at one device may not have
        // that in mind.
        let prompt = format!(
            "Remove {} and all {} of their device(s) from this channel? \
             The roster epoch advances and this cannot be undone. [y/N]",
            member.principal_id,
            member.devices.len()
        );

        self.proposed = Some((
            MemberOutcome::Remove {
                principal_id: member.principal_id.clone(),
            },
            prompt,
        ));
    }

    /// Proposes revoking the selected device.
    fn propose_revocation(&mut self) {
        let Some(member) = self.selected_member() else {
            return;
        };
        let Some(device) = self.selected_device() else {
            self.status = "That member has no device selected.".into();
            return;
        };

        if !device.active {
            self.status = "That device has already been revoked.".into();
            return;
        }

        // Revoking the last active device of a member leaves a principal
        // nobody can reach: still in the roster, addressable, and unable to
        // read anything. Saying so is not the same as refusing it — there are
        // good reasons to do it — but it should not be a surprise.
        let remaining = member
            .devices
            .iter()
            .filter(|candidate| candidate.active && candidate.device_id != device.device_id)
            .count();

        let consequence = if remaining == 0 {
            " This is their last active device: they will remain a member with no way to read \
             anything."
        } else {
            ""
        };

        let prompt = format!(
            "Revoke device {} of {}?{consequence} [y/N]",
            device.device_id, member.principal_id
        );

        self.proposed = Some((
            MemberOutcome::Revoke {
                device_id: device.device_id.clone(),
            },
            prompt,
        ));
    }
}

impl crate::run::Screen for MembersApp {
    type Outcome = MemberOutcome;

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::view::render_members(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<MemberOutcome> {
        MembersApp::on_key(self, key)
    }
}

#[cfg(test)]
mod tests;
