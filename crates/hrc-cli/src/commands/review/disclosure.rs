//! Disclosing a context package, behind its trusted preview.

use super::*;

/// Opens the context disclosure screen (PRD sections 20 and 22.4).
///
/// Both halves go through the daemon's trusted interface rather than the
/// local functions of the same name, because doing the work locally would
/// move the boundary into a process an agent can start.
///
/// Section 22.4's context authorization is different from the prompt gate's.
/// The approval one never leaves the daemon (decision DEC-040); this one is
/// returned by `PreviewContext` and must be presented to `SendContext`, and
/// it is bound to the package digest, the recipient, the channel and the send
/// action, with a five-minute server-controlled expiry. So it is held here,
/// in memory, between the human seeing the preview and answering the
/// confirmation — and it is dropped the moment the selection changes, because
/// an authorization for bytes nobody is looking at any more is one nobody
/// meant to spend.
pub fn context(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "send a context package")?;

    let database = Database::open(context.paths.database())?;
    let drafts: Vec<hrc_tui::ContextDraft> = database
        .context_drafts()?
        .into_iter()
        .map(|(package_id, digest)| hrc_tui::ContextDraft { package_id, digest })
        .collect();

    if drafts.is_empty() {
        return Ok(json!({
            "status": "ok",
            "drafts": 0,
            "sent": Value::Null,
        }));
    }

    let recipients: Vec<String> = addressable(context)?
        .into_iter()
        .filter(|recipient| recipient.active)
        .map(|recipient| recipient.principal_id)
        .collect();

    if recipients.is_empty() {
        return Ok(json!({
            "status": "ok",
            "recipients": 0,
            "sent": Value::Null,
        }));
    }

    let runtime = runtime()?;
    let endpoint = trusted_endpoint(context)?;
    let mut screen = ContextApp::new(drafts, recipients);
    let mut authorization: Option<String> = None;

    // The screen asks and answers more than once: a preview comes back into
    // it, and only then can a send be proposed. So the loop lives here rather
    // than in `hrc_tui::run`, which returns on the first outcome.
    loop {
        let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
            action: "run the context disclosure screen",
            source,
        })?;

        match outcome {
            ContextOutcome::Preview {
                package_id,
                recipient,
            } => {
                let previewed = runtime.block_on(trusted_call(
                    &endpoint,
                    TrustedRequest::PreviewContext {
                        recipient,
                        package_id: package_id.clone(),
                    },
                ))?;

                let parsed = preview_from(&previewed, &package_id)?;
                authorization = Some(parsed.authorization);
                screen.show_preview(parsed.preview);
            }

            ContextOutcome::Send {
                package_id,
                recipient,
            } => {
                // The authorization the preview issued for exactly this
                // package and recipient. Taken rather than cloned, so a
                // second send cannot reuse it from here — the daemon would
                // refuse it anyway, and the two agreeing is the point.
                let Some(granted) = authorization.take() else {
                    return Err(CliError::NoContextAuthorization);
                };

                let sent = runtime.block_on(trusted_call(
                    &endpoint,
                    TrustedRequest::SendContext {
                        recipient: recipient.clone(),
                        package_id: package_id.clone(),
                        authorization: granted,
                    },
                ))?;
                require_trusted_success(&sent, "send_context")?;

                return Ok(json!({
                    "status": "ok",
                    "contextId": package_id,
                    "recipient": recipient,
                    "daemon": sent,
                }));
            }

            ContextOutcome::Quit => {
                return Ok(json!({
                    "status": "ok",
                    "sent": Value::Null,
                }));
            }
        }
    }
}

/// Reads a successful trusted preview into the exact content the screen displays.
///
/// The daemon has already re-read source files and rejected secret or path
/// findings before it returns success. Missing fields and explicit error
/// responses therefore fail the command rather than becoming an empty,
/// apparently harmless preview.
pub(super) fn preview_from(
    response: &Value,
    expected_package_id: &str,
) -> Result<ParsedContextPreview> {
    require_trusted_success(response, "preview_context")?;

    let package_id = required_string(response, "contextId")?;
    if package_id != expected_package_id {
        return Err(invalid_trusted_response(format!(
            "trusted preview answered for context `{package_id}` instead of `{expected_package_id}`"
        )));
    }

    let digest = required_string(response, "digest")?.to_owned();
    let content = required_string(response, "content")?.to_owned();
    let authorization = required_string(response, "authorization")?.to_owned();
    let total_bytes = response["totalBytes"].as_u64().ok_or_else(|| {
        invalid_trusted_response("trusted preview omitted the canonical package size")
    })?;
    if total_bytes != content.len() as u64 {
        return Err(invalid_trusted_response(format!(
            "trusted preview declared {total_bytes} bytes but returned {}",
            content.len()
        )));
    }

    let package = hrc_protocol::canonical::from_json_str::<hrc_core::ContextPackage>(&content)
        .map_err(|error| {
            invalid_trusted_response(format!(
                "trusted preview returned invalid canonical content: {error}"
            ))
        })?;
    if package.id != package_id {
        return Err(invalid_trusted_response(
            "trusted preview content has a different package identifier",
        ));
    }
    let canonical_content =
        hrc_protocol::canonical::to_canonical_bytes(&package).map_err(|error| {
            invalid_trusted_response(format!(
                "trusted preview content could not be canonicalized: {error}"
            ))
        })?;
    if canonical_content.as_slice() != content.as_bytes() {
        return Err(invalid_trusted_response(
            "trusted preview content was not the canonical package representation",
        ));
    }
    package.verify_digest(&digest).map_err(|error| {
        invalid_trusted_response(format!(
            "trusted preview content does not match its digest: {error}"
        ))
    })?;

    let items = response["items"]
        .as_array()
        .ok_or_else(|| invalid_trusted_response("trusted preview omitted its item list"))?
        .iter()
        .map(parse_preview_item)
        .collect::<Result<Vec<_>>>()?;
    let verified = package.preview().map_err(|error| {
        invalid_trusted_response(format!(
            "trusted preview content could not be verified: {error}"
        ))
    })?;
    if !verified.is_sendable() {
        return Err(invalid_trusted_response(
            "trusted preview returned content that failed local disclosure checks",
        ));
    }
    let verified_items = verified
        .items
        .into_iter()
        .map(|(kind, bytes)| (kind, bytes as u64))
        .collect::<Vec<_>>();
    if items != verified_items || total_bytes != verified.total_bytes as u64 {
        return Err(invalid_trusted_response(
            "trusted preview summary did not match its canonical content",
        ));
    }

    Ok(ParsedContextPreview {
        authorization,
        preview: hrc_tui::ContextPreview {
            digest,
            content,
            items,
            total_bytes,
            secrets: Vec::new(),
            excluded: Vec::new(),
            // A successful daemon preview has already re-read the source,
            // checked exclusions and scanned secrets. Error responses never
            // reach the screen through this parser.
            sendable: true,
        },
    })
}

#[derive(Debug)]
pub(super) struct ParsedContextPreview {
    pub(super) authorization: String,
    pub(super) preview: hrc_tui::ContextPreview,
}

pub(super) fn parse_preview_item(value: &Value) -> Result<(String, u64)> {
    if let Some(pair) = value.as_array()
        && pair.len() == 2
        && let (Some(kind), Some(bytes)) = (pair[0].as_str(), pair[1].as_u64())
    {
        return Ok((kind.to_owned(), bytes));
    }

    if let (Some(kind), Some(bytes)) = (value["kind"].as_str(), value["bytes"].as_u64()) {
        return Ok((kind.to_owned(), bytes));
    }

    Err(invalid_trusted_response(
        "trusted preview returned a malformed item summary",
    ))
}
