//! Built-in Git transport adapter for Herdr Remote Channel.
//!
//! One Git repository is one channel. The machine-managed `hrc` branch holds
//! an append-only history in which each commit is exactly one publication
//! (PRD sections 16 and 21.1.1).
//!
//! The mapping from the transport contract onto Git:
//!
//! | Transport concept | Git |
//! |---|---|
//! | Revision | Commit SHA |
//! | Publication | One commit |
//! | Genesis | The root commit, the only one with no parent |
//! | Compare-and-swap | Fast-forward push against the expected tip |
//! | Object name | Path within the tree |
//!
//! Everything runs through the system `git` executable
//! (PRD requirement HRC-TECH-010), against a bare repository this adapter
//! owns. No user checkout is touched, and no working tree exists to leave
//! dirty.
//!
//! Publication uses optimistic concurrency exactly as section 17.3 sets out:
//! build on the fetched tip, push fast-forward, and on rejection report a
//! conflict so the caller rebuilds. It never force-pushes — not as a policy
//! that could be relaxed, but because no code path passes `--force`.

pub mod error;
pub mod git;

use std::path::Path;

use hrc_transport::{
    AdapterCapabilities, FetchPage, GroupState, LINEAR_APPEND_ONLY, ObjectClass, ObjectRecord,
    Publication, PublicationClass, PublishObject, PublishRequest, Transport, TransportError,
    validate_publication,
};

pub use error::GitError;

use crate::git::GitRepository;

/// The machine-managed branch holding channel data.
pub const CHANNEL_BRANCH: &str = "hrc";

/// Largest object this adapter accepts, matching the PRD section 20.3 hard
/// maximum for a single attachment ciphertext.
const MAX_OBJECT_BYTES: u64 = 25 * 1024 * 1024;

/// The Git transport.
pub struct GitTransport {
    repository: GitRepository,
    branch: String,
    capabilities: AdapterCapabilities,
}

impl std::fmt::Debug for GitTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitTransport")
            .field("git_dir", &self.repository.git_dir())
            .field("branch", &self.branch)
            .finish_non_exhaustive()
    }
}

impl GitTransport {
    /// Opens the adapter against a locally managed repository.
    ///
    /// `git_dir` is created as a bare repository if it does not exist. The
    /// remote is only contacted by [`Self::sync_from_remote`] and by
    /// publication.
    pub fn open(git_dir: impl AsRef<Path>, remote_url: &str) -> Result<Self, GitError> {
        let repository = GitRepository::new(git_dir.as_ref());
        repository.init_bare()?;
        repository.set_remote(remote_url)?;

        Ok(Self {
            repository,
            branch: CHANNEL_BRANCH.to_owned(),
            capabilities: AdapterCapabilities {
                adapter: "hrc-transport-git".into(),
                protocol: hrc_protocol::TRANSPORT_PROTOCOL_ID.into(),
                max_object_bytes: MAX_OBJECT_BYTES,
                max_objects_per_publication: 1024,
                durable: true,
                history_model: LINEAR_APPEND_ONLY,
                // Git offers no blocking wait; the daemon polls `ls-remote`.
                supports_wait: false,
                min_poll_interval_seconds: 5,
                supports_lazy_objects: true,
                supports_group_creation: true,
            },
        })
    }

    /// The repository this adapter manages.
    pub fn repository(&self) -> &GitRepository {
        &self.repository
    }

    /// Fetches the channel branch from the remote.
    ///
    /// Kept separate from [`Transport::fetch`], which reads local history:
    /// the daemon decides when to spend a network round trip, usually after
    /// [`Self::remote_head`] shows the tip has moved (PRD section 17.1).
    pub fn sync_from_remote(&self) -> Result<(), GitError> {
        let refspec = format!("+refs/heads/{0}:refs/heads/{0}", self.branch);
        match self
            .repository
            .try_run(["fetch", "--quiet", "origin", refspec.as_str()])?
        {
            Ok(_) => Ok(()),
            // A remote with no channel branch yet is the pre-genesis state,
            // not a failure.
            Err(stderr) if stderr.contains("couldn't find remote ref") => Ok(()),
            Err(stderr) => Err(GitError::Command {
                status: None,
                stderr,
            }),
        }
    }

    /// Reads the remote branch tip without fetching objects.
    ///
    /// This is the cheap change check from PRD section 17.1: the daemon
    /// polls the tip and only fetches when it differs from its cursor.
    pub fn remote_head(&self) -> Result<Option<String>, GitError> {
        let reference = format!("refs/heads/{}", self.branch);
        let output = self
            .repository
            .run(["ls-remote", "origin", reference.as_str()])?;

        Ok(output
            .split_whitespace()
            .next()
            .filter(|sha| sha.len() == 40)
            .map(str::to_owned))
    }

    /// The local branch tip, if the branch exists.
    fn local_head(&self) -> Result<Option<String>, GitError> {
        let reference = format!("refs/heads/{}", self.branch);
        match self
            .repository
            .try_run(["rev-parse", "--verify", "--quiet", reference.as_str()])?
        {
            Ok(output) => {
                let sha = output.trim().to_owned();
                Ok(if sha.is_empty() { None } else { Some(sha) })
            }
            Err(_) => Ok(None),
        }
    }

    /// Commits `objects` on top of `parent` and moves the branch.
    fn commit(
        &self,
        parent: Option<&str>,
        objects: &[PublishObject],
        message: &str,
    ) -> Result<String, GitError> {
        // Start the index from the parent tree so the commit is purely
        // additive. Anything already present stays exactly as it was.
        match parent {
            Some(parent) => {
                self.repository.run(["read-tree", parent])?;
            }
            None => {
                self.repository.run(["read-tree", "--empty"])?;
            }
        }

        for object in objects {
            let blob = self
                .repository
                .run_with_input(["hash-object", "-w", "--stdin"], &object.bytes)?;
            let blob = blob.trim();

            self.repository.run([
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{}", object.name),
            ])?;
        }

        let tree = self.repository.run(["write-tree"])?;
        let tree = tree.trim().to_owned();

        let mut arguments = vec![
            "commit-tree".to_owned(),
            tree,
            "-m".to_owned(),
            message.to_owned(),
        ];
        if let Some(parent) = parent {
            arguments.push("-p".to_owned());
            arguments.push(parent.to_owned());
        }

        let commit = self.repository.run(arguments)?;
        let commit = commit.trim().to_owned();

        let reference = format!("refs/heads/{}", self.branch);
        match parent {
            // Compare-and-swap at the ref level too: passing the expected
            // old value makes the local update fail rather than clobber a
            // tip another process moved.
            Some(parent) => {
                self.repository
                    .run(["update-ref", &reference, &commit, parent])?;
            }
            None => {
                self.repository
                    .run(["update-ref", &reference, &commit, ""])?;
            }
        }

        Ok(commit)
    }

    /// Pushes the channel branch fast-forward.
    ///
    /// Returns `false` when the remote rejected the push, which means
    /// another peer published first.
    fn push(&self, commit: &str) -> Result<bool, GitError> {
        let refspec = format!("{commit}:refs/heads/{}", self.branch);
        // No `--force`, and no `+` in the refspec. A rejected push is a
        // signal to rebuild, never something to override.
        match self
            .repository
            .try_run(["push", "--quiet", "origin", refspec.as_str()])?
        {
            Ok(_) => Ok(true),
            Err(stderr) if is_non_fast_forward(&stderr) => Ok(false),
            Err(stderr) => Err(GitError::Command {
                status: None,
                stderr,
            }),
        }
    }

    /// Rewinds the local branch after a rejected push.
    fn reset_local(&self, to: Option<&str>, expected: &str) -> Result<(), GitError> {
        let reference = format!("refs/heads/{}", self.branch);
        let result = match to {
            Some(sha) => self
                .repository
                .try_run(["update-ref", &reference, sha, expected])?,
            None => self
                .repository
                .try_run(["update-ref", "-d", &reference, expected])?,
        };

        match result {
            Ok(_) => Ok(()),
            Err(_) if self.local_head()?.as_deref() != Some(expected) => Ok(()),
            Err(stderr) => Err(GitError::Command {
                status: None,
                stderr,
            }),
        }
    }

    /// Lists commits on the branch, oldest first.
    fn commit_list(&self) -> Result<Vec<String>, GitError> {
        let Some(_) = self.local_head()? else {
            return Ok(Vec::new());
        };

        let output =
            self.repository
                .run(["rev-list", "--first-parent", "--reverse", &self.branch])?;

        Ok(output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Reads one commit as a publication.
    fn publication_for(&self, commit: &str) -> Result<Publication, TransportError> {
        let parents = self
            .repository
            .run(["rev-list", "--parents", "-n", "1", commit])
            .map_err(to_transport_error)?;

        let mut fields = parents.split_whitespace();
        let _self = fields.next();
        let parent_list: Vec<&str> = fields.collect();

        if parent_list.len() > 1 {
            // A merge commit has no single predecessor, so a client could
            // not say which roster state governs the objects it introduces
            // (PRD section 17.5.1).
            return Err(TransportError::InvalidPublication {
                reason: format!("commit {commit} is a merge commit"),
            });
        }
        let parent = parent_list.first().map(|value| (*value).to_owned());

        let changes = match parent.as_deref() {
            Some(parent) => self
                .repository
                .run([
                    "diff-tree",
                    "-r",
                    "--no-commit-id",
                    "--name-status",
                    parent,
                    commit,
                ])
                .map_err(to_transport_error)?,
            None => self
                .repository
                .run(["ls-tree", "-r", "--name-only", commit])
                .map_err(to_transport_error)?
                .lines()
                .map(|path| format!("A\t{path}"))
                .collect::<Vec<_>>()
                .join("\n"),
        };

        let mut objects = Vec::new();
        for line in changes.lines().filter(|line| !line.trim().is_empty()) {
            let (status, path) = line.split_once('\t').ok_or_else(|| {
                TransportError::Provider(format!("unparsable change line: {line}"))
            })?;

            if status != "A" {
                // History is append-only. A modification or deletion means
                // an object the client may already have validated has
                // changed underneath it (PRD section 16.3).
                return Err(TransportError::InvalidPublication {
                    reason: format!(
                        "commit {commit} {} {path} instead of adding it",
                        match status {
                            "M" => "modifies",
                            "D" => "deletes",
                            other => other,
                        }
                    ),
                });
            }

            objects.push(self.object_record(commit, path)?);
        }

        let class = classify_publication(&objects, parent.is_none())?;

        let transport_time = self
            .repository
            .run(["show", "-s", "--format=%cI", commit])
            .map_err(to_transport_error)?
            .trim()
            .to_owned();

        Ok(Publication {
            revision: commit.to_owned(),
            parent_revision: parent,
            class,
            transport_time,
            objects,
        })
    }

    /// Describes one object in a commit's tree.
    fn object_record(&self, commit: &str, path: &str) -> Result<ObjectRecord, TransportError> {
        let bytes = self
            .repository
            .run_bytes(["cat-file", "blob", &format!("{commit}:{path}")])
            .map_err(to_transport_error)?;

        Ok(ObjectRecord {
            name: path.to_owned(),
            class: classify_object(path)?,
            size: bytes.len() as u64,
            sha256: hrc_protocol::canonical::sha256_hex(&bytes),
        })
    }

    /// Rejects objects that break a declared limit or a structural rule.
    fn check_objects(&self, objects: &[PublishObject]) -> Result<(), TransportError> {
        if objects.len() > self.capabilities.max_objects_per_publication {
            return Err(TransportError::InvalidPublication {
                reason: format!(
                    "{} objects exceeds the {} object limit",
                    objects.len(),
                    self.capabilities.max_objects_per_publication
                ),
            });
        }

        let head = self.local_head().map_err(to_transport_error)?;

        for object in objects {
            let size = object.bytes.len() as u64;
            if size > self.capabilities.max_object_bytes {
                return Err(TransportError::ObjectTooLarge {
                    name: object.name.clone(),
                    size,
                    limit: self.capabilities.max_object_bytes,
                });
            }

            if object.name.starts_with('/') || object.name.contains("..") {
                return Err(TransportError::InvalidPublication {
                    reason: format!("object name {} escapes the channel layout", object.name),
                });
            }

            // Objects are immutable. Reusing a path with different bytes is
            // the substitution case from PRD section 17.4.
            if let Some(head) = head.as_deref()
                && let Ok(existing) = self.repository.run_bytes([
                    "cat-file",
                    "blob",
                    &format!("{head}:{}", object.name),
                ])
                && existing != object.bytes
            {
                return Err(TransportError::InvalidPublication {
                    reason: format!(
                        "object {} already exists with different content",
                        object.name
                    ),
                });
            }
        }

        Ok(())
    }
}

impl Transport for GitTransport {
    fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    fn create_group(&mut self, objects: Vec<PublishObject>) -> hrc_transport::Result<Publication> {
        self.sync_from_remote().map_err(to_transport_error)?;

        if self.local_head().map_err(to_transport_error)?.is_some() {
            return Err(TransportError::GroupExists);
        }

        let request = PublishRequest {
            expected_revision: None,
            class: PublicationClass::Genesis,
            objects,
        };
        validate_publication(&request)?;
        self.check_objects(&request.objects)?;

        let commit = self
            .commit(None, &request.objects, "hrc: genesis")
            .map_err(to_transport_error)?;

        if !self.push(&commit).map_err(to_transport_error)? {
            // Someone else created the channel between the fetch and the
            // push. Drop the local branch so the next attempt starts from
            // their history rather than ours.
            self.reset_local(None, &commit)
                .map_err(to_transport_error)?;
            return Err(TransportError::GroupExists);
        }

        self.publication_for(&commit)
    }

    fn open_group(&self) -> hrc_transport::Result<GroupState> {
        let head = self.local_head().map_err(to_transport_error)?;
        if head.is_none() {
            return Err(TransportError::NoSuchGroup);
        }

        Ok(GroupState {
            group_id: self.branch.clone(),
            revision: head,
            history_model: LINEAR_APPEND_ONLY,
        })
    }

    fn publish(&mut self, request: PublishRequest) -> hrc_transport::Result<Publication> {
        let head = self.local_head().map_err(to_transport_error)?;
        if head.is_none() {
            return Err(TransportError::NoSuchGroup);
        }

        if request.class == PublicationClass::Genesis {
            return Err(TransportError::InvalidPublication {
                reason: "genesis can only be published by creating the channel".into(),
            });
        }

        if request.expected_revision != head {
            return Err(TransportError::Conflict { current: head });
        }

        validate_publication(&request)?;
        self.check_objects(&request.objects)?;

        let parent = head.expect("head was checked above");
        let commit = self
            .commit(
                Some(&parent),
                &request.objects,
                &format!("hrc: {} publication", request.class.as_str()),
            )
            .map_err(to_transport_error)?;

        match self.push(&commit) {
            Ok(true) => {}
            Ok(false) => {
                // Another peer pushed first. Rewind to the tip we built on
                // and report a conflict; the caller re-fetches and rebuilds.
                self.reset_local(Some(&parent), &commit)
                    .map_err(to_transport_error)?;
                self.sync_from_remote().map_err(to_transport_error)?;

                return Err(TransportError::Conflict {
                    current: self.local_head().map_err(to_transport_error)?,
                });
            }
            Err(error) => {
                // A failed push never proves that the local commit reached
                // the remote. Drop it so later lost-ack detection only sees
                // history confirmed by a subsequent fetch.
                self.reset_local(Some(&parent), &commit)
                    .map_err(to_transport_error)?;
                return Err(to_transport_error(error));
            }
        }

        self.publication_for(&commit)
    }

    fn fetch(&self, after: Option<&str>, limit: usize) -> hrc_transport::Result<FetchPage> {
        let commits = self.commit_list().map_err(to_transport_error)?;
        if commits.is_empty() {
            return Err(TransportError::NoSuchGroup);
        }

        let start = match after {
            None => 0,
            Some(revision) => {
                let position = commits.iter().position(|c| c == revision).ok_or_else(|| {
                    // A cursor the branch no longer contains means the
                    // history was rewritten under this client.
                    TransportError::InvalidPublication {
                        reason: format!("cursor {revision} is not part of this history"),
                    }
                })?;
                position + 1
            }
        };

        let end = commits.len().min(start + limit.max(1));
        let mut publications = Vec::with_capacity(end - start);
        for commit in &commits[start..end] {
            publications.push(self.publication_for(commit)?);
        }

        let cursor = publications
            .last()
            .map(|p| p.revision.clone())
            .or_else(|| after.map(str::to_owned));

        Ok(FetchPage {
            more: end < commits.len(),
            publications,
            cursor,
            anomalies: Vec::new(),
        })
    }

    fn get_object(&self, name: &str, expected_sha256: &str) -> hrc_transport::Result<Vec<u8>> {
        let head = self
            .local_head()
            .map_err(to_transport_error)?
            .ok_or(TransportError::NoSuchGroup)?;

        let bytes = self
            .repository
            .run_bytes(["cat-file", "blob", &format!("{head}:{name}")])
            .map_err(|_| TransportError::NoSuchObject {
                name: name.to_owned(),
            })?;

        if hrc_protocol::canonical::sha256_hex(&bytes) != expected_sha256 {
            return Err(TransportError::ObjectHashMismatch {
                name: name.to_owned(),
            });
        }

        Ok(bytes)
    }

    fn health(&self) -> hrc_transport::Result<()> {
        self.repository
            .run(["rev-parse", "--git-dir"])
            .map(|_| ())
            .map_err(to_transport_error)
    }
}

/// Classifies an object from its path in the channel layout (PRD 16.2).
fn classify_object(path: &str) -> Result<ObjectClass, TransportError> {
    if path == "protocol.json" {
        return Ok(ObjectClass::Protocol);
    }

    let class = match path.split('/').next().unwrap_or_default() {
        "control" => ObjectClass::Control,
        "joins" => ObjectClass::Join,
        "messages" => ObjectClass::Message,
        "blobs" => ObjectClass::Blob,
        "snapshots" => ObjectClass::Snapshot,
        _ => {
            return Err(TransportError::InvalidPublication {
                reason: format!("object path {path} is outside the channel layout"),
            });
        }
    };

    Ok(class)
}

/// Determines a publication's class from the objects it introduces.
fn classify_publication(
    objects: &[ObjectRecord],
    is_root: bool,
) -> Result<PublicationClass, TransportError> {
    let has_control = objects.iter().any(|object| object.class.is_control());
    let has_data = objects.iter().any(|object| !object.class.is_control());

    if has_control && has_data {
        return Err(TransportError::InvalidPublication {
            reason: "commit mixes control and data objects".into(),
        });
    }

    if is_root {
        return Ok(PublicationClass::Genesis);
    }

    Ok(if has_control {
        PublicationClass::Control
    } else {
        PublicationClass::Data
    })
}

/// Whether a push failure means the remote branch moved.
fn is_non_fast_forward(stderr: &str) -> bool {
    stderr.contains("non-fast-forward")
        || stderr.contains("fetch first")
        || stderr.contains("Updates were rejected")
        || stderr.contains("cannot lock ref")
        || stderr.contains("failed to push some refs")
}

/// Converts a Git failure into a transport error.
fn to_transport_error(error: GitError) -> TransportError {
    TransportError::Provider(error.to_string())
}
