//! Making a channel's repository public (PRD requirement HRC-CH-004,
//! section 16.4).
//!
//! Private is the default and public is close to irreversible: once
//! ciphertext, commit timing, and pusher identities have been readable by
//! anyone, making the repository private again does not unpublish what was
//! already copied. So this is not a yes/no prompt.
//!
//! Two things must happen before it can proceed, and they are separate on
//! purpose:
//!
//! 1. The seven consequences of section 16.4 are shown. All of them, in
//!    full — [`PublicDisclosure::ALL`] is the closed list and a caller
//!    cannot show a subset.
//! 2. The human types a phrase naming the specific channel. Not `y`, not
//!    `yes`, not Enter. The phrase contains the channel's own local name, so
//!    the muscle memory built by confirming one channel does not carry to
//!    another.
//!
//! This mirrors the reasoning behind decision DEC-046 for the prompt gate:
//! a decision that a habitual keypress can make is not a decision.

use serde::{Deserialize, Serialize};

/// One consequence of publishing a channel's repository (PRD section 16.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicDisclosure {
    /// The ciphertext stays readable by anyone, permanently.
    PermanentCiphertext,
    /// When commits happen, and how often.
    CommitTiming,
    /// Which GitHub accounts pushed.
    PusherIdentities,
    /// Padded sizes still bound the size of what was said.
    ApproximateObjectSizes,
    /// How many members and devices a channel has.
    MembershipCounts,
    /// When membership changed.
    RosterChangeTiming,
    /// Encryption sound today may not be sound for the life of the data.
    FutureCryptographicRisk,
}

impl PublicDisclosure {
    /// Every consequence section 16.4 requires disclosing.
    ///
    /// A closed list rather than free text, so a caller renders all seven or
    /// does not compile. Disclosure that a screen can quietly shorten is the
    /// failure this requirement exists to prevent.
    pub const ALL: [PublicDisclosure; 7] = [
        PublicDisclosure::PermanentCiphertext,
        PublicDisclosure::CommitTiming,
        PublicDisclosure::PusherIdentities,
        PublicDisclosure::ApproximateObjectSizes,
        PublicDisclosure::MembershipCounts,
        PublicDisclosure::RosterChangeTiming,
        PublicDisclosure::FutureCryptographicRisk,
    ];

    /// What the human is told.
    ///
    /// **The exact wording is provisional.** Open question OQ-008 asks what
    /// public-repository warning and confirmation text is required, and it is
    /// unanswered. The mechanism below — all seven shown, an exact typed
    /// phrase naming the channel — is what this requirement is really about
    /// and does not depend on the prose. A product owner should replace these
    /// strings before a public channel is offered to anyone; the tests assert
    /// the structure, not the sentences, so rewording them breaks nothing.
    pub const fn text(self) -> &'static str {
        match self {
            PublicDisclosure::PermanentCiphertext => {
                "Every encrypted message and attachment in this channel becomes readable \
                 by anyone, forever. Making the repository private again does not unpublish \
                 what has already been copied."
            }
            PublicDisclosure::CommitTiming => {
                "When each message was published, and how often this channel is active, \
                 becomes public even though the contents stay encrypted."
            }
            PublicDisclosure::PusherIdentities => {
                "The GitHub accounts that pushed to this channel become public, linking \
                 those identities to this conversation."
            }
            PublicDisclosure::ApproximateObjectSizes => {
                "Message sizes are padded into buckets, not hidden. The bucket a message \
                 falls into is public and bounds how long it was."
            }
            PublicDisclosure::MembershipCounts => {
                "How many people and devices are in this channel becomes public, even \
                 though their identities stay encrypted."
            }
            PublicDisclosure::RosterChangeTiming => {
                "When someone joined, left, or had a device revoked becomes public."
            }
            PublicDisclosure::FutureCryptographicRisk => {
                "Anyone may copy the ciphertext now and keep it. Encryption considered \
                 sound today may not be for the lifetime of what this channel holds."
            }
        }
    }
}

/// The phrase a human must type, and whether they typed it.
///
/// Built from the channel's locally chosen name, so confirming one channel
/// teaches no keystrokes that would confirm another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    phrase: String,
}

impl Confirmation {
    /// The challenge for one channel.
    pub fn for_channel(channel_local_name: &str) -> Self {
        Self {
            phrase: format!("make {channel_local_name} public"),
        }
    }

    /// The exact phrase to display and require.
    pub fn phrase(&self) -> &str {
        &self.phrase
    }

    /// Whether what the human typed is the phrase.
    ///
    /// Surrounding whitespace is forgiven because a terminal adds it and a
    /// person cannot see it. Nothing else is: not case, not a prefix, not a
    /// yes, not an empty line. Anything accepted here that is easier to type
    /// than the phrase is the thing that will eventually be typed by
    /// accident.
    pub fn is_satisfied_by(&self, typed: &str) -> bool {
        typed.trim() == self.phrase
    }
}

/// A request to publish a repository that has cleared both gates.
///
/// Only constructible through [`Self::new`], which requires the whole
/// disclosure to have been shown and the phrase to have been typed. A
/// caller cannot assemble one field by field and skip a check, which is why
/// the fields are private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedPublication {
    channel_local_name: String,
    confirmed_by: String,
}

/// Why a publication was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusedPublication {
    /// The caller did not show every consequence in section 16.4.
    DisclosureIncomplete {
        /// What was not shown.
        missing: Vec<PublicDisclosure>,
    },
    /// What the human typed was not the phrase.
    PhraseNotTyped {
        /// The phrase that was required.
        expected: String,
    },
}

impl ConfirmedPublication {
    /// Builds a confirmation, or explains why there is not one.
    ///
    /// `shown` is what the interface actually displayed. Passing it rather
    /// than assuming it means a screen that rendered five of the seven
    /// consequences cannot proceed, which is the failure mode a disclosure
    /// requirement is written against.
    pub fn new(
        channel_local_name: &str,
        shown: &[PublicDisclosure],
        typed: &str,
        confirmed_by: &str,
    ) -> Result<Self, RefusedPublication> {
        let missing: Vec<PublicDisclosure> = PublicDisclosure::ALL
            .into_iter()
            .filter(|disclosure| !shown.contains(disclosure))
            .collect();

        if !missing.is_empty() {
            return Err(RefusedPublication::DisclosureIncomplete { missing });
        }

        let confirmation = Confirmation::for_channel(channel_local_name);
        if !confirmation.is_satisfied_by(typed) {
            return Err(RefusedPublication::PhraseNotTyped {
                expected: confirmation.phrase().to_owned(),
            });
        }

        Ok(Self {
            channel_local_name: channel_local_name.to_owned(),
            confirmed_by: confirmed_by.to_owned(),
        })
    }

    /// The channel being published.
    pub fn channel_local_name(&self) -> &str {
        &self.channel_local_name
    }

    /// Who confirmed it, for the audit record.
    pub fn confirmed_by(&self) -> &str {
        &self.confirmed_by
    }

    /// The audit detail recorded alongside the decision.
    ///
    /// Names the phrase that was required rather than what was typed: the
    /// record needs to show which channel was confirmed, and echoing input
    /// into an audit log is a habit worth not having.
    pub fn audit_detail(&self) -> String {
        format!(
            "repository made public for channel {} after the full section 16.4 disclosure \
             and typed confirmation by {}",
            self.channel_local_name, self.confirmed_by
        )
    }
}

#[cfg(test)]
mod tests;
