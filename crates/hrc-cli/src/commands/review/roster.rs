//! Removing a member, revoking a device, and naming a member.

use super::*;

/// The roster as the membership screen draws it.
///
/// `is_local` is what lets that screen refuse to remove this installation.
/// It compares roster *principals* with this installation's principal; it
/// used to compare them with the device's signing key, so it was never true
/// and the refusal never applied (docs/REFACTOR.md R4).
pub(super) fn member_rows(context: &Context) -> Result<Vec<hrc_tui::Member>> {
    let listing = crate::commands::roster_listing(context)?;
    let local_principal = crate::commands::local_identity(context)?.principal_id;

    // Read once, before the screen opens, so every row shows the name that
    // was on record when the person started looking at the roster.
    let aliases = {
        let database = Database::open(context.paths.database())?;
        database.principal_aliases(&listing.channel_id)?
    };

    Ok(listing
        .members
        .into_iter()
        .map(|member| hrc_tui::Member {
            is_local: member.principal_id == local_principal,
            display_name: aliases.get(&member.principal_id).cloned(),
            administrator: member.administrator,
            active: member.active,
            devices: member
                .devices
                .into_iter()
                .map(|device| hrc_tui::MemberDevice {
                    device_id: device.device_id,
                    active: device.active,
                })
                .collect(),
            principal_id: member.principal_id,
        })
        .collect())
}

/// Opens the membership screen (PRD requirement HRC-CH-008).
///
/// Removing a member and revoking a device are section 22.7 operations, and
/// both publish a control entry to an append-only history that advances the
/// roster epoch. Nothing here can take one back, which is why the screen says
/// so before it asks.
pub fn members(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "change who is in this channel")?;

    let roster = member_rows(context)?;

    if roster.is_empty() {
        return Ok(json!({
            "status": "ok",
            "members": 0,
            "changed": Value::Null,
        }));
    }

    let mut screen = MembersApp::new(roster);
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the membership screen",
        source,
    })?;

    let (request, changed) = match outcome {
        MemberOutcome::Remove { principal_id } => (
            TrustedRequest::RemoveMember {
                principal_id: principal_id.clone(),
            },
            json!({ "removed": principal_id }),
        ),
        MemberOutcome::Revoke { device_id } => (
            TrustedRequest::RevokeDevice {
                device_id: device_id.clone(),
            },
            json!({ "revoked": device_id }),
        ),
        MemberOutcome::Name {
            principal_id,
            display_name,
        } => (
            TrustedRequest::SetAlias {
                principal_id: principal_id.clone(),
                display_name: display_name.clone(),
            },
            // The principal is named, not the name: this answer is written
            // to a log and to an agent-readable surface, and the point of
            // the alias is that it stays on the screen a human is looking
            // at.
            json!({ "named": principal_id, "cleared": display_name.is_none() }),
        ),
        MemberOutcome::Quit => {
            return Ok(json!({
                "status": "ok",
                "changed": Value::Null,
            }));
        }
    };

    let runtime = runtime()?;
    let endpoint = trusted_endpoint(context)?;
    let applied = runtime.block_on(trusted_call(&endpoint, request))?;

    Ok(json!({
        "status": "ok",
        "changed": changed,
        "daemon": applied,
    }))
}
