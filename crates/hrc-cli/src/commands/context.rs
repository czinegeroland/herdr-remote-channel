//! Context packages: drafting, previewing and sending (PRD section 20).

use super::*;

/// `hrc context draft`: keep an explicitly selected package locally.
pub fn context_draft(
    context: &Context,
    manifest_path: &str,
    repository: Option<&str>,
) -> Result<Value> {
    let manifest = std::fs::read_to_string(manifest_path).map_err(|source| CliError::Io {
        action: "read the context manifest",
        source,
    })?;
    let package: ContextPackage = canonical::from_json_str(&manifest)?;
    if package.version != hrc_protocol::PROTOCOL_VERSION {
        return Err(hrc_core::CoreError::MalformedMessage {
            reason: "context package has an unsupported version".into(),
        }
        .into());
    }
    let mut preview = package.preview()?;
    let repository_root = canonical_repository_root(repository)?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    if let Some(excluded) = preview.excluded.first() {
        return Err(hrc_core::CoreError::ContextContainsExcludedPath {
            path: excluded.path.clone(),
        }
        .into());
    }

    // The manifest's excerpt text is untrusted caller input. Replace it with
    // the exact selected source bytes before computing the stored digest.
    let package = materialize_excerpt_sources(package, repository_root.as_deref())?;
    let package = materialize_captures(package, repository_root.as_deref())?;
    let canonical_manifest = canonical::to_canonical_bytes(&package)?;
    let digest = package.digest()?;
    let mut preview = package.preview()?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    // Drafting has no side effect outside this installation, so findings are
    // retained for the required human preview instead of silently removing
    // content or making the author reconstruct what was rejected.
    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    database.save_context_draft(
        &package.id,
        repository_root.as_deref(),
        &digest,
        &canonical_manifest,
        &now,
    )?;

    Ok(context_preview_value(
        &package.id,
        &digest,
        &preview,
        "draft",
    ))
}

/// `hrc context preview`: report the selected bytes and blockers without
/// echoing package text or a suspected secret.
pub fn context_preview(context: &Context, package_id: &str) -> Result<Value> {
    let (package, digest, repository_root) = load_context_draft(context, package_id)?;
    let mut preview = package.preview()?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    verify_excerpt_sources(&package, repository_root.as_deref())?;
    Ok(context_preview_value(
        package_id, &digest, &preview, "preview",
    ))
}

/// `hrc context send`: attach the exact reviewed package to an encrypted note.
pub fn context_send(context: &Context, recipient: &str, package_id: &str) -> Result<Value> {
    let (package, digest, repository_root) = load_context_draft(context, package_id)?;
    let mut preview = package.preview()?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    verify_excerpt_sources(&package, repository_root.as_deref())?;
    if !preview.secrets.is_empty() {
        return Err(hrc_core::CoreError::ContextContainsSecrets {
            count: preview.secrets.len(),
        }
        .into());
    }
    if let Some(excluded) = preview.excluded.first() {
        return Err(hrc_core::CoreError::ContextContainsExcludedPath {
            path: excluded.path.clone(),
        }
        .into());
    }

    let mut sent = compose_body(
        context,
        hrc_protocol::MessageKind::Note,
        recipient,
        serde_json::json!({
            "context": package,
            "contextDigest": digest,
        }),
        None,
        None,
    )?;
    sent["contextId"] = Value::String(package_id.to_owned());
    sent["contextDigest"] = Value::String(digest);
    Ok(sent)
}

pub(super) fn load_context_draft(
    context: &Context,
    package_id: &str,
) -> Result<(ContextPackage, String, Option<String>)> {
    let database = Database::open(context.paths.database())?;
    let draft = database
        .context_draft(package_id)?
        .ok_or_else(|| CliError::NoSuchMessage {
            message_id: package_id.to_owned(),
        })?;
    let text = std::str::from_utf8(&draft.manifest).map_err(|_| {
        CliError::Core(hrc_core::CoreError::MalformedMessage {
            reason: "stored context manifest is not UTF-8".into(),
        })
    })?;
    let package = canonical::from_json_str::<ContextPackage>(text)?;
    package.verify_digest(&draft.digest)?;
    if package.id != draft.package_id {
        return Err(hrc_core::CoreError::MalformedMessage {
            reason: "stored context package ID does not match its record".into(),
        }
        .into());
    }
    Ok((package, draft.digest, draft.repository_root))
}

pub(super) fn context_preview_value(
    package_id: &str,
    digest: &str,
    preview: &hrc_core::ContextPreview,
    state: &str,
) -> Value {
    json!({
        "status": "ok",
        "state": state,
        "contextId": package_id,
        "digest": digest,
        "totalBytes": preview.total_bytes,
        "items": preview.items.iter().map(|(kind, bytes)| json!({
            "kind": kind,
            "bytes": bytes,
        })).collect::<Vec<_>>(),
        "sendable": preview.is_sendable(),
        "secretFindings": preview.secrets.iter().map(|finding| json!({
            "item": finding.item_index,
            "rule": finding.rule,
            "detail": finding.detail,
        })).collect::<Vec<_>>(),
        "excludedPaths": preview.excluded.iter().map(|excluded| json!({
            "item": excluded.item_index,
            "path": excluded.path,
            "reason": excluded.reason,
        })).collect::<Vec<_>>(),
    })
}

/// Adds exclusions that only the source repository can decide.
pub(super) fn append_repository_exclusions(
    package: &ContextPackage,
    repository: Option<&str>,
    preview: &mut hrc_core::ContextPreview,
) -> Result<()> {
    let source_paths = package.source_paths().collect::<Vec<_>>();
    if source_paths.is_empty() {
        return Ok(());
    }

    let repository = repository.ok_or_else(|| CliError::ContextRepositoryRequired {
        package_id: package.id.clone(),
    })?;

    for (item_index, path) in source_paths {
        if !is_repository_relative_path(path) {
            preview.excluded.push(ExcludedPath {
                item_index,
                path: path.to_owned(),
                reason: "excerpt paths must be repository-relative",
            });
            continue;
        }
        let normalized_path = path.replace('\\', "/");

        let status = ProcessCommand::new("git")
            .args([
                "-C",
                repository,
                "check-ignore",
                "--quiet",
                "--no-index",
                "--",
            ])
            .arg(&normalized_path)
            .status()
            .map_err(|source| CliError::Io {
                action: "check whether a context path is Git-ignored",
                source,
            })?;
        if status.success() {
            preview.excluded.push(ExcludedPath {
                item_index,
                path: path.to_owned(),
                reason: "path is ignored by its Git repository",
            });
        } else if status.code() != Some(1) {
            return Err(CliError::Io {
                action: "check whether a context path is Git-ignored",
                source: std::io::Error::other("Git could not evaluate the source repository"),
            });
        }
    }

    Ok(())
}

/// Resolves a supplied repository to its canonical absolute Git worktree
/// root. A relative display label is not a durable source identity.
pub(super) fn canonical_repository_root(repository: Option<&str>) -> Result<Option<String>> {
    let Some(repository) = repository else {
        return Ok(None);
    };
    let output = ProcessCommand::new("git")
        .args(["-C", repository, "rev-parse", "--show-toplevel"])
        .output()
        .map_err(|source| CliError::Io {
            action: "resolve the context repository root",
            source,
        })?;
    if !output.status.success() {
        return Err(CliError::InvalidContextSource {
            reason: "repository is not a Git worktree",
        });
    }
    let root = String::from_utf8(output.stdout).map_err(|_| CliError::InvalidContextSource {
        reason: "Git returned a non-UTF-8 repository path",
    })?;
    let root = std::fs::canonicalize(root.trim()).map_err(|source| CliError::Io {
        action: "canonicalize the context repository root",
        source,
    })?;
    Ok(Some(root.display().to_string()))
}

/// Replaces every caller-supplied excerpt with bytes from its declared,
/// canonical repository source before the package digest is calculated.
pub(super) fn materialize_excerpt_sources(
    mut package: ContextPackage,
    repository_root: Option<&str>,
) -> Result<ContextPackage> {
    if package.source_paths().next().is_some() && repository_root.is_none() {
        return Err(CliError::ContextRepositoryRequired {
            package_id: package.id.clone(),
        });
    }
    for item in &mut package.items {
        if let hrc_core::ContextItem::Excerpt {
            path,
            first_line,
            last_line,
            commit_sha,
            text,
        } = item
        {
            *text = read_excerpt(
                repository_root.expect("checked above"),
                path,
                *first_line,
                *last_line,
                commit_sha.as_deref(),
            )?;
        }
    }
    Ok(package)
}

/// The largest capture HRC will carry into a context package.
///
/// A bound rather than a limit chosen for elegance: a repository with a
/// thousand modified files produces a `git status` nobody will read and a
/// message that costs everyone bandwidth. Truncation is reported in the
/// text, so a reader never mistakes a cut-off capture for a complete one.
pub(super) const MAX_CAPTURE_BYTES: usize = 64 * 1024;

/// Replaces requested patch and output items with what HRC captured itself
/// (PRD requirements HRC-CTX-004 and HRC-CTX-007).
///
/// This is the whole point of those two rows. A caller names *what* to
/// capture — a revision range, or one command from
/// [`hrc_core::context::AllowedCommand`] — and HRC produces the bytes. Text
/// the caller supplied is discarded rather than trusted, exactly as
/// [`materialize_excerpt_sources`] does for excerpts, so provenance is
/// derived and not asserted.
pub(super) fn materialize_captures(
    mut package: ContextPackage,
    repository_root: Option<&str>,
) -> Result<ContextPackage> {
    let needs_capture = package.items.iter().any(|item| {
        matches!(
            item,
            hrc_core::ContextItem::Patch { .. } | hrc_core::ContextItem::Output { .. }
        )
    });

    if !needs_capture {
        return Ok(package);
    }

    let Some(root) = repository_root else {
        return Err(CliError::ContextRepositoryRequired {
            package_id: package.id.clone(),
        });
    };

    for item in &mut package.items {
        match item {
            hrc_core::ContextItem::Patch { range, diff } => {
                // A range is caller input that reaches a command line, so it
                // is checked against a conservative shape rather than passed
                // through. `--output=/etc/passwd` is a revision range as far
                // as a naive check is concerned.
                if !is_plain_revision_range(range) {
                    // The rejected range is deliberately not echoed: this
                    // field is `&'static str` so caller input cannot reach a
                    // diagnostic, and a range is caller input.
                    return Err(CliError::InvalidContextSource {
                        reason: "a patch range must be a plain revision range such as \
                                 HEAD~3..HEAD",
                    });
                }

                *diff = capture_git(root, &["diff", "--no-color", range])?;
            }
            hrc_core::ContextItem::Output { command, text } => {
                let allowed = hrc_core::context::AllowedCommand::parse(command).ok_or(
                    CliError::InvalidContextSource {
                        reason: "output items may carry only the output of a command HRC runs \
                                 itself; see AllowedCommand for the list",
                    },
                )?;

                *text = capture_git(root, &allowed.argv())?;
            }
            _ => {}
        }
    }

    Ok(package)
}

/// Whether a string is a revision range and nothing else.
///
/// Deliberately narrow. Git accepts a great deal of syntax, and the ones
/// that matter here are the ones that stop being a range: anything starting
/// with `-` is an option, and whitespace makes it more than one argument.
pub(super) fn is_plain_revision_range(range: &str) -> bool {
    let range = range.trim();

    !range.is_empty()
        && !range.starts_with('-')
        && range.len() <= 200
        && range.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | '-' | '/' | '~' | '^' | '@')
        })
}

/// Runs one read-only Git command in the repository and returns its output.
pub(super) fn capture_git(root: &str, arguments: &[&str]) -> Result<String> {
    let mut command = ProcessCommand::new("git");
    command.args(["-C", root]).args(arguments);

    let output = command.output().map_err(|source| CliError::Io {
        action: "capture context from the repository",
        source,
    })?;

    if !output.status.success() {
        // Git's stderr can quote refs and paths from the repository, so it
        // is not repeated here for the same reason the range is not.
        return Err(CliError::InvalidContextSource {
            reason: "the repository could not produce that capture",
        });
    }

    let captured = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(truncate_capture(captured))
}

/// Bounds a capture, saying so in the text when it was cut.
pub(super) fn truncate_capture(captured: String) -> String {
    if captured.len() <= MAX_CAPTURE_BYTES {
        return captured;
    }

    // Cut on a character boundary, then on a line, so the result is neither
    // invalid UTF-8 nor a half-written path.
    let mut cut = MAX_CAPTURE_BYTES;
    while cut > 0 && !captured.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &captured[..cut];
    let head = head.rfind('\n').map_or(head, |line| &head[..line]);

    format!("{head}\n[truncated by HRC at {MAX_CAPTURE_BYTES} bytes]\n")
}

/// Confirms that the checked source still equals the snapshot a human will
/// review and authorize.
pub(super) fn verify_excerpt_sources(
    package: &ContextPackage,
    repository_root: Option<&str>,
) -> Result<()> {
    for item in &package.items {
        if let hrc_core::ContextItem::Excerpt {
            path,
            first_line,
            last_line,
            commit_sha,
            text,
        } = item
        {
            let actual = read_excerpt(
                repository_root.ok_or_else(|| CliError::ContextRepositoryRequired {
                    package_id: package.id.clone(),
                })?,
                path,
                *first_line,
                *last_line,
                commit_sha.as_deref(),
            )?;
            if actual != *text {
                return Err(CliError::InvalidContextSource {
                    reason: "excerpt source changed since the package was drafted",
                });
            }
        }
    }
    Ok(())
}

/// Reads one bounded source selection without accepting manifest text.
pub(super) fn read_excerpt(
    repository_root: &str,
    path: &str,
    first_line: u32,
    last_line: u32,
    commit_sha: Option<&str>,
) -> Result<String> {
    if !is_repository_relative_path(path) {
        return Err(CliError::InvalidContextSource {
            reason: "excerpt paths must be repository-relative",
        });
    }
    if first_line == 0 || last_line < first_line {
        return Err(CliError::InvalidContextSource {
            reason: "excerpt line range is invalid",
        });
    }

    let source = if let Some(commit_sha) = commit_sha {
        if !matches!(commit_sha.len(), 40 | 64)
            || !commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt commit must be a full SHA-1 or SHA-256 object ID",
            });
        }
        let commit_expression = format!("{commit_sha}^{{commit}}");
        let verified = ProcessCommand::new("git")
            .args(["-C", repository_root, "rev-parse", "--verify"])
            .arg(&commit_expression)
            .output()
            .map_err(|source| CliError::Io {
                action: "verify the committed context excerpt",
                source,
            })?;
        if !verified.status.success() {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt commit does not resolve to a commit",
            });
        }
        let output = ProcessCommand::new("git")
            .args(["-C", repository_root, "show", "--no-textconv"])
            .arg(format!("{commit_sha}:{}", path.replace('\\', "/")))
            .output()
            .map_err(|source| CliError::Io {
                action: "read the committed context excerpt",
                source,
            })?;
        if !output.status.success() {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt commit or path is unavailable",
            });
        }
        String::from_utf8(output.stdout).map_err(|_| CliError::InvalidContextSource {
            reason: "excerpt source is not UTF-8 text",
        })?
    } else {
        let root = std::fs::canonicalize(repository_root).map_err(|source| CliError::Io {
            action: "canonicalize the context repository root",
            source,
        })?;
        let source_path =
            std::fs::canonicalize(root.join(path.replace('\\', "/"))).map_err(|source| {
                CliError::Io {
                    action: "read the context excerpt",
                    source,
                }
            })?;
        if !source_path.starts_with(&root) {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt path escapes its repository",
            });
        }
        std::fs::read_to_string(&source_path).map_err(|source| CliError::Io {
            action: "read the context excerpt",
            source,
        })?
    };

    let selected = source
        .lines()
        .skip((first_line - 1) as usize)
        .take((last_line - first_line + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n");
    if selected.lines().count() != (last_line - first_line + 1) as usize {
        return Err(CliError::InvalidContextSource {
            reason: "excerpt line range is outside the source file",
        });
    }
    Ok(selected)
}

pub(super) fn is_repository_relative_path(path: &str) -> bool {
    // Context manifests move between Windows and Unix. Treat both separators
    // as separators before asking the host's path parser, or a Windows
    // traversal could become an ordinary filename on Unix.
    let normalized = path.replace('\\', "/");
    let path = Path::new(&normalized);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}
