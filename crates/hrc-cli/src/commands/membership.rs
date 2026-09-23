//! The roster as read, and changes to it (PRD requirement HRC-CH-008).

use super::*;

/// Publishes one membership change as a control entry.
///
/// Reached only from the trusted interface: removing a member and revoking a
/// device are both on the section 22.7 list, so this is never called from an
/// agent-safe path.
pub(super) fn publish_membership_change(
    context: &Context,
    operation: ControlOperation,
) -> Result<String> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let channel = only_channel(&database)?;

    let mut transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, revision) = load_roster(&transport, &channel.channel_id)?;

    let action = operation.name();
    let signer = Signer {
        principal_id: principal.signing_key().verifying_key().to_base64url(),
        device_id: local_device_id(&roster, &device)?,
    };

    let name = publish_control(
        &mut transport,
        &roster,
        revision,
        &device.signing_key(),
        signer,
        operation,
        &now,
    )?;

    database.append_audit(
        Some(&channel.channel_id),
        None,
        action,
        None,
        Some(&name),
        &now,
    )?;

    Ok(name)
}

/// `hrc members`: who is in the channel, from the published control log.
/// One device in the roster.
pub struct RosterDevice {
    /// The device identifier.
    pub device_id: String,
    /// Whether it is still active.
    pub active: bool,
    /// The epoch it was added in.
    pub added_in_epoch: u64,
}

/// One member of the roster.
pub struct RosterMember {
    /// The member's principal.
    pub principal_id: String,
    /// Whether they administer the channel.
    pub administrator: bool,
    /// Whether they are still a member.
    pub active: bool,
    /// Their devices.
    pub devices: Vec<RosterDevice>,
}

/// The channel's current roster, fetched and verified.
pub struct RosterListing {
    /// The channel.
    pub channel_id: String,
    /// The roster epoch.
    pub epoch: u64,
    /// Every member, in roster order.
    pub members: Vec<RosterMember>,
}

/// Reads the roster, for callers inside this crate.
///
/// The screens used to call `members` and read its JSON back by key; a key
/// renamed in one place compiled and became an empty string in the other.
pub fn roster_listing(context: &Context) -> Result<RosterListing> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, _) = load_roster(&transport, &channel.channel_id)?;

    let members = roster
        .members()
        .map(|member| RosterMember {
            principal_id: member.principal_id.clone(),
            administrator: member.is_administrator,
            active: member.is_active,
            devices: roster
                .devices()
                .filter(|device| device.principal_id == member.principal_id)
                .map(|device| RosterDevice {
                    device_id: device.device_id.clone(),
                    active: device.status == hrc_core::DeviceStatus::Active,
                    added_in_epoch: device.added_in_epoch,
                })
                .collect(),
        })
        .collect();

    Ok(RosterListing {
        channel_id: channel.channel_id,
        epoch: roster.epoch(),
        members,
    })
}

/// `hrc members`. Its output is unchanged by R4; only what reads it is.
pub fn members(context: &Context) -> Result<Value> {
    let listing = roster_listing(context)?;

    let members: Vec<Value> = listing
        .members
        .iter()
        .map(|member| {
            json!({
                "principalId": member.principal_id,
                "administrator": member.administrator,
                "active": member.active,
                "devices": member
                    .devices
                    .iter()
                    .map(|device| json!({
                        "deviceId": device.device_id,
                        "active": device.active,
                        "addedInEpoch": device.added_in_epoch,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "channelId": listing.channel_id,
        "rosterEpoch": listing.epoch,
        "members": members,
    }))
}

/// `hrc device list`: this principal's devices.
pub fn device_list(context: &Context) -> Result<Value> {
    let store = context.key_store()?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;
    let principal_id = principal.signing_key().verifying_key().to_base64url();

    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, _) = load_roster(&transport, &channel.channel_id)?;

    let devices: Vec<Value> = roster
        .devices()
        .filter(|device| device.principal_id == principal_id)
        .map(|device| {
            json!({
                "deviceId": device.device_id,
                "active": device.status == hrc_core::DeviceStatus::Active,
                "addedInEpoch": device.added_in_epoch,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "principalId": principal_id,
        "devices": devices,
    }))
}
