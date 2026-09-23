//! Admitting a member: the join-request screen, with the safety phrase on
//! its confirmation (decision DEC-064).

use super::*;

/// Opens the membership approval screen (PRD requirement HRC-CH-007).
///
/// The requests come from `join_pending`, which only returns those whose
/// proof already validates against an invite this installation issued. An
/// unverifiable request is not shown, because there is nothing a human could
/// usefully decide about one.
pub fn review_joins(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "review join requests")?;
    let (_, listed) = crate::commands::pending_join_requests(context)?;
    let requests = pending_joins(listed);

    if requests.is_empty() {
        return Ok(json!({
            "status": "ok",
            "pending": 0,
            "decided": Value::Null,
        }));
    }

    let channel_local_name = channel_label(context)?;
    let mut screen = JoinApp::new(requests, channel_local_name);
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the membership approval screen",
        source,
    })?;

    let (request, admitted) = match outcome {
        JoinOutcome::Admit { request_id } => (
            TrustedRequest::ApproveJoin {
                request_id: request_id.clone(),
            },
            true,
        ),
        JoinOutcome::Refuse { request_id } => (
            TrustedRequest::RejectJoin {
                request_id: request_id.clone(),
            },
            false,
        ),
        // Leaving is not a decision, and recording one would put a choice in
        // the audit log that nobody made.
        JoinOutcome::Quit => {
            return Ok(json!({
                "status": "ok",
                "decided": Value::Null,
            }));
        }
    };

    let runtime = runtime()?;
    let endpoint = trusted_endpoint(context)?;
    let applied: Value = runtime.block_on(async {
        let mut client = connect(&endpoint).await?;
        let response: Value = client.call(&request).await.map_err(CliError::from)?;
        Ok::<Value, CliError>(response)
    })?;

    Ok(json!({
        "status": "ok",
        "decided": if admitted { "admit" } else { "refuse" },
        "daemon": applied,
    }))
}

/// The join requests as the review screen shows them.
pub(super) fn pending_joins(listed: Vec<crate::commands::JoinRequest>) -> Vec<PendingJoin> {
    listed
        .into_iter()
        .map(|request| PendingJoin {
            request_id: request.request_id,
            principal_id: request.principal_id,
            device_id: request.device_id,
            created_at: request.created_at,
            safety_phrase: request.safety_phrase,
        })
        .collect()
}
