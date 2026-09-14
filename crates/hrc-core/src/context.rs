//! Context packages: bounded, previewable material a user chooses to send.
//!
//! PRD section 20. The inbound half of the safety story is the prompt gate;
//! this is the outbound half. The failure mode it exists to prevent is a
//! user, or an agent acting for them, attaching more than they realized —
//! an `.env` file, a scrollback buffer, a key.
//!
//! Three rules shape the design:
//!
//! 1. **Nothing is included implicitly.** A package contains exactly the
//!    items someone added. There is no "attach the current directory".
//! 2. **The preview is the payload.** [`ContextPackage::preview`] reports the
//!    exact bytes and item list that will leave the machine, so what a user
//!    approves is what is sent (HRC-CTX-002).
//! 3. **Detected secrets block the send.** They are never silently stripped:
//!    a user who believes they sent something they did not is worse off than
//!    one who is told to fix it (HRC-CTX-003, HRC-SEC-008).

use hrc_protocol::canonical;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// One piece of context (PRD section 20.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextItem {
    /// Free-form Markdown written by the sender.
    Note {
        /// The text.
        text: String,
    },
    /// A bounded excerpt of a file.
    Excerpt {
        /// Repository-relative path.
        path: String,
        /// First line included, one-based.
        #[serde(rename = "firstLine")]
        first_line: u32,
        /// Last line included, one-based and inclusive.
        #[serde(rename = "lastLine")]
        last_line: u32,
        /// Commit the excerpt was taken from, when known.
        #[serde(rename = "commitSha", skip_serializing_if = "Option::is_none")]
        commit_sha: Option<String>,
        /// The excerpt itself.
        text: String,
    },
    /// A Git patch that HRC produced itself.
    ///
    /// `range` records what was asked for, so the receiver sees where the
    /// diff came from and the sender cannot pass off arbitrary text as one.
    /// An item without it is refused: see [`AllowedCommand`] for the same
    /// reasoning applied to command output.
    Patch {
        /// The revision range the diff was taken over, for example
        /// `HEAD~3..HEAD`.
        range: String,
        /// The patch text.
        diff: String,
    },
    /// A commit or branch reference, carrying no content of its own.
    Reference {
        /// What is being referenced, for example a commit SHA.
        reference: String,
        /// Optional human description.
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Bounded output from a command.
    Output {
        /// The command that produced it, for the reader's context.
        command: String,
        /// The captured output.
        text: String,
    },
    /// A link. The receiver is never made to follow it.
    Link {
        /// The URL.
        url: String,
        /// Optional human description.
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

/// The commands HRC will run to produce an output item (PRD requirements
/// HRC-CTX-004 and HRC-CTX-007).
///
/// A closed set, and deliberately a short one. Every member is read-only,
/// reports on the repository rather than the machine, and emits no
/// environment variable, no shell history, and no terminal scrollback — the
/// four things section 12.5 requires excluded by default.
///
/// Git's own behavior does part of the work: `status` and `diff` do not
/// report ignored files, so an ignored path cannot reach a package through
/// them. It does not do all of it — an untracked `.env` appears in
/// `git status` — so each emitted path is still checked against
/// [`excluded_path_reason`].
///
/// Nothing here runs project code. `cargo test` would be a useful context
/// item and is deliberately absent: capturing it means executing whatever
/// the repository says to execute, which is a larger decision than adding an
/// attachment to a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedCommand {
    /// `git status --porcelain`: what is modified, staged, or untracked.
    GitStatus,
    /// `git log --oneline -n 20`: recent history, subjects only.
    GitLog,
    /// `git diff --stat`: which files changed and by how much, no content.
    GitDiffStat,
}

impl AllowedCommand {
    /// Every command HRC will run for an output item.
    pub const ALL: [AllowedCommand; 3] = [
        AllowedCommand::GitStatus,
        AllowedCommand::GitLog,
        AllowedCommand::GitDiffStat,
    ];

    /// The exact command string recorded in the item, and the only spelling
    /// accepted.
    ///
    /// One spelling per command on purpose. Accepting variants would mean
    /// parsing command lines, and a parser that decides `git  status` is the
    /// same as `git status` is one that can be argued into deciding
    /// something worse is too.
    pub const fn as_str(self) -> &'static str {
        match self {
            AllowedCommand::GitStatus => "git status --porcelain",
            AllowedCommand::GitLog => "git log --oneline -n 20",
            AllowedCommand::GitDiffStat => "git diff --stat",
        }
    }

    /// The argument vector to execute, without the `git` program name.
    pub fn argv(self) -> Vec<&'static str> {
        match self {
            AllowedCommand::GitStatus => vec!["status", "--porcelain"],
            AllowedCommand::GitLog => vec!["log", "--oneline", "-n", "20"],
            AllowedCommand::GitDiffStat => vec!["diff", "--stat"],
        }
    }

    /// Recognizes a command string, exactly.
    pub fn parse(command: &str) -> Option<Self> {
        AllowedCommand::ALL
            .into_iter()
            .find(|allowed| allowed.as_str() == command)
    }
}

impl ContextItem {
    /// The wire name of this item kind.
    pub fn kind(&self) -> &'static str {
        match self {
            ContextItem::Note { .. } => "note",
            ContextItem::Excerpt { .. } => "excerpt",
            ContextItem::Patch { .. } => "patch",
            ContextItem::Reference { .. } => "reference",
            ContextItem::Output { .. } => "output",
            ContextItem::Link { .. } => "link",
        }
    }

    /// Every caller-controlled string serialized for this item.
    fn scannable_fields(&self) -> Vec<&str> {
        match self {
            ContextItem::Note { text } => vec![text],
            ContextItem::Excerpt {
                path,
                commit_sha,
                text,
                ..
            } => {
                let mut fields = vec![path.as_str(), text.as_str()];
                if let Some(commit_sha) = commit_sha {
                    fields.push(commit_sha);
                }
                fields
            }
            ContextItem::Patch { range, diff } => vec![range, diff],
            ContextItem::Reference {
                reference,
                description,
            } => {
                let mut fields = vec![reference.as_str()];
                if let Some(description) = description {
                    fields.push(description);
                }
                fields
            }
            ContextItem::Output { command, text } => vec![command, text],
            ContextItem::Link { url, description } => {
                let mut fields = vec![url.as_str()];
                if let Some(description) = description {
                    fields.push(description);
                }
                fields
            }
        }
    }

    /// The path this item came from, when it came from one.
    fn source_path(&self) -> Option<&str> {
        match self {
            ContextItem::Excerpt { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Material that is never an acceptable context source, independent of
    /// whether it happens to contain a recognizable secret.
    fn excluded_content_reason(&self) -> Option<&'static str> {
        match self {
            // A patch must name the range it was taken over. HRC produces
            // the diff from a verified worktree; text handed in with no
            // provenance is not a patch, it is a note the sender called one.
            ContextItem::Patch { range, .. } if range.trim().is_empty() => {
                Some("patch items must name the revision range HRC captured them over")
            }
            ContextItem::Patch { .. } => None,

            // An allowlist, not a deny-list. The previous check tried to
            // recognize environment dumps and scrollback by pattern, which
            // is a losing game: `sh -c 'cat ~/.bash_history'` is neither
            // `history` nor `env` and dumps a transcript anyway. Naming the
            // commands HRC will run inverts it — anything not on the list is
            // refused without having to be recognized as dangerous.
            ContextItem::Output { command, .. } => match AllowedCommand::parse(command) {
                Some(_) => None,
                None => Some(
                    "output items may carry only the output of a command HRC ran itself; \
                     see AllowedCommand for the list",
                ),
            },

            _ => None,
        }
    }
}

/// A finding from the secret scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// Index of the offending item within the package.
    pub item_index: usize,
    /// What kind of secret this looks like.
    pub rule: &'static str,
    /// Human explanation, with no secret material in it.
    pub detail: String,
}

/// A path that must not be read into a package (PRD section 20.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedPath {
    /// Index of the offending item.
    pub item_index: usize,
    /// The path that was refused.
    pub path: String,
    /// Why it was refused.
    pub reason: &'static str,
}

/// What a package will send, shown before it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPreview {
    /// One line per item: kind and byte count.
    pub items: Vec<(String, usize)>,
    /// Exact serialized size of the package, in bytes.
    pub total_bytes: usize,
    /// Secrets detected. A non-empty list blocks the send.
    pub secrets: Vec<SecretFinding>,
    /// Excluded paths found. A non-empty list blocks the send.
    pub excluded: Vec<ExcludedPath>,
}

impl ContextPreview {
    /// Whether this package may be sent as it stands.
    pub fn is_sendable(&self) -> bool {
        self.secrets.is_empty() && self.excluded.is_empty()
    }
}

/// A collection of context items with an integrity digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPackage {
    /// Protocol version.
    pub version: u32,
    /// Package identifier.
    pub id: String,
    /// The items, in the order the sender added them.
    pub items: Vec<ContextItem>,
}

impl ContextPackage {
    /// Builds a package.
    pub fn new(id: impl Into<String>, items: Vec<ContextItem>) -> Self {
        Self {
            version: hrc_protocol::PROTOCOL_VERSION,
            id: id.into(),
            items,
        }
    }

    /// SHA-256 over the canonical encoding of the package.
    ///
    /// A receiver verifies this before displaying or extracting anything
    /// (HRC-CTX-006), so altered content is detected rather than shown.
    pub fn digest(&self) -> Result<String> {
        Ok(canonical::canonical_sha256_hex(self)?)
    }

    /// Checks a package against the digest it was announced with.
    pub fn verify_digest(&self, expected: &str) -> Result<()> {
        if self.digest()? == expected {
            Ok(())
        } else {
            Err(CoreError::ContextDigestMismatch)
        }
    }

    /// Reports exactly what would be sent.
    ///
    /// This is the preview a user approves, so it counts the real serialized
    /// size rather than the sum of the item texts: the difference is the
    /// encoding overhead, and a user told the smaller number would be told
    /// something untrue.
    pub fn preview(&self) -> Result<ContextPreview> {
        let mut items = Vec::with_capacity(self.items.len());
        let mut secrets = scan_for_secrets(usize::MAX, &self.id);
        let mut excluded = Vec::new();

        for (index, item) in self.items.iter().enumerate() {
            let encoded = canonical::to_canonical_bytes(item)?;
            items.push((item.kind().to_owned(), encoded.len()));
            for field in item.scannable_fields() {
                secrets.extend(scan_for_secrets(index, field));
            }

            if let Some(path) = item.source_path()
                && let Some(reason) = excluded_path_reason(path)
            {
                excluded.push(ExcludedPath {
                    item_index: index,
                    path: path.to_owned(),
                    reason,
                });
            }
            if let Some(reason) = item.excluded_content_reason() {
                excluded.push(ExcludedPath {
                    item_index: index,
                    path: item.kind().to_owned(),
                    reason,
                });
            }
        }

        Ok(ContextPreview {
            items,
            total_bytes: canonical::to_canonical_bytes(self)?.len(),
            secrets,
            excluded,
        })
    }

    /// Returns the package only if it is safe to send.
    ///
    /// Blocking rather than stripping is deliberate: a package silently
    /// reduced to something else is a package the sender did not approve.
    pub fn ready_to_send(&self) -> Result<ContextPreview> {
        let preview = self.preview()?;

        if !preview.secrets.is_empty() {
            return Err(CoreError::ContextContainsSecrets {
                count: preview.secrets.len(),
            });
        }

        if !preview.excluded.is_empty() {
            return Err(CoreError::ContextContainsExcludedPath {
                path: preview.excluded[0].path.clone(),
            });
        }

        Ok(preview)
    }

    /// Decodes a context package announced inside a message body.
    ///
    /// The digest is checked before the caller stores or displays any part
    /// of the package. The returned package remains quarantined; verifying
    /// integrity does not make remote content trusted.
    pub fn from_message_body(body: &serde_json::Value) -> Result<Option<(Self, String)>> {
        let Some(value) = body.get("context") else {
            return Ok(None);
        };
        let expected = body
            .get("contextDigest")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| CoreError::MalformedMessage {
                reason: "context package has no digest".into(),
            })?;
        let encoded = canonical::to_canonical_json(value)?;
        let package = canonical::from_json_str::<ContextPackage>(&encoded)?;

        if package.version != hrc_protocol::PROTOCOL_VERSION {
            return Err(CoreError::MalformedMessage {
                reason: "context package has an unsupported version".into(),
            });
        }
        package.verify_digest(expected)?;

        Ok(Some((package, expected.to_owned())))
    }

    /// Source paths that require a repository-aware exclusion check.
    pub fn source_paths(&self) -> impl Iterator<Item = (usize, &str)> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.source_path().map(|path| (index, path)))
    }
}

/// Why a path may not be read into a package, if it may not.
///
/// PRD section 20.2 lists what is excluded by default. Git-ignored status is
/// not decidable here — it needs the repository — so the caller checks that
/// separately and this covers the rules that are purely about the path.
pub fn excluded_path_reason(path: &str) -> Option<&'static str> {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);

    if name == ".env" || name.starts_with(".env.") || lower.contains("/.env/") {
        return Some("environment files are excluded by default");
    }

    if name == ".git" || lower.starts_with(".git/") || lower.contains("/.git/") {
        return Some("git internals are excluded by default");
    }

    if name == ".ssh" || lower.starts_with(".ssh/") || lower.contains("/.ssh/") {
        return Some("ssh material is excluded by default");
    }

    let key_suffixes = [".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"];
    if key_suffixes.iter().any(|suffix| name.ends_with(suffix)) {
        return Some("key material is excluded by default");
    }

    let credential_names = [
        "id_rsa",
        "id_ed25519",
        "id_ecdsa",
        "credentials",
        ".netrc",
        ".npmrc",
        ".pypirc",
        "secrets.yaml",
        "secrets.yml",
        "secrets.json",
    ];
    if credential_names.contains(&name) {
        return Some("credential files are excluded by default");
    }

    None
}

/// Scans text for material that looks like a secret.
///
/// The rules are conservative and prefix-anchored rather than entropy-based.
/// A scanner that guesses from entropy alone flags base64 payloads and
/// hashes constantly, and a blocking check that cries wolf gets disabled —
/// at which point it protects nothing. See PRD open question OQ-006 on
/// whether a fuller scanner should be adopted later.
///
/// Findings never quote the matched value. An error message that echoes the
/// secret puts it in logs and terminal scrollback, which is where it was not
/// supposed to go.
pub fn scan_for_secrets(item_index: usize, text: &str) -> Vec<SecretFinding> {
    /// A rule: a name, and the markers that trigger it.
    const RULES: &[(&str, &[&str])] = &[
        (
            "private_key",
            &[
                "-----BEGIN RSA PRIVATE KEY-----",
                "-----BEGIN OPENSSH PRIVATE KEY-----",
                "-----BEGIN EC PRIVATE KEY-----",
                "-----BEGIN DSA PRIVATE KEY-----",
                "-----BEGIN PGP PRIVATE KEY BLOCK-----",
                "-----BEGIN PRIVATE KEY-----",
            ],
        ),
        ("age_identity", &["AGE-SECRET-KEY-1"]),
        ("aws_access_key", &["AKIA", "ASIA"]),
        (
            "github_token",
            &["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"],
        ),
        (
            "slack_token",
            &["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"],
        ),
        ("google_api_key", &["AIza"]),
        ("stripe_key", &["sk_live_", "rk_live_"]),
        ("openai_key", &["sk-proj-", "sk-ant-"]),
        ("json_web_token", &["eyJhbGciOi"]),
    ];

    let mut findings = Vec::new();

    for (rule, markers) in RULES {
        if markers.iter().any(|marker| text.contains(marker)) {
            findings.push(SecretFinding {
                item_index,
                rule,
                // Deliberately no excerpt of the match.
                detail: format!("content matches the {rule} pattern"),
            });
        }
    }

    findings.extend(scan_assignments(item_index, text));
    findings
}

/// Flags assignments that name a secret and carry a substantial value.
///
/// `password=` with a long value is worth blocking; `password=` with an
/// empty value or a placeholder is noise, and noise is what gets a blocking
/// check turned off.
fn scan_assignments(item_index: usize, text: &str) -> Vec<SecretFinding> {
    const NAMES: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
        "private_key",
        "client_secret",
    ];
    const PLACEHOLDERS: &[&str] = &[
        "changeme",
        "example",
        "placeholder",
        "redacted",
        "xxxxx",
        "your_",
        "<your",
        "todo",
        "none",
        "null",
    ];
    const MINIMUM_VALUE_LENGTH: usize = 8;

    let mut findings = Vec::new();
    let lower = text.to_ascii_lowercase();

    for line in lower.lines() {
        let Some((name, value)) = line.split_once(['=', ':']) else {
            continue;
        };

        let name = name.trim().trim_matches(['"', '\'', '-', ' ']);
        if !NAMES.iter().any(|candidate| name.ends_with(candidate)) {
            continue;
        }

        let value = value.trim().trim_matches(['"', '\'', ',', ';', ' ']);
        if value.len() < MINIMUM_VALUE_LENGTH {
            continue;
        }
        if PLACEHOLDERS
            .iter()
            .any(|placeholder| value.contains(placeholder))
        {
            continue;
        }

        findings.push(SecretFinding {
            item_index,
            rule: "credential_assignment",
            detail: format!("a `{name}` assignment carries a non-placeholder value"),
        });
        break;
    }

    findings
}

#[cfg(test)]
mod tests;
