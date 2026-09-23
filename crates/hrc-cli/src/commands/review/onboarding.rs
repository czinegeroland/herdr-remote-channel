//! Setting up: initializing, creating a channel, inviting and joining.

use super::*;

/// Opens the channel setup screen.
///
/// Which steps it offers depends on what this installation already has. A
/// step that can only fail is not offered: inviting needs a channel, and
/// creating one is pointless when a channel already exists.
pub fn setup(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let steps = available_steps(context)?;

    // Initialization is the one step that cannot ask for the passphrase
    // first, because it is the step that chooses it. Everything else needs
    // the key store open before it can do anything.
    let initializing = steps == [hrc_tui::SetupStep::Initialize];
    let owned;
    let context = if initializing {
        context
    } else {
        owned = unlocked(context, "set up this channel")?;
        &owned
    };

    // A person who arrived by clicking an invitation link has already said
    // what they came to do. Nothing from the URL is displayed or typed in:
    // a clicked URL is text somebody else sent, and this reads it only far
    // enough to decide which step to open on.
    let mut screen = match std::env::var(hrc_herdr::link::CLICKED_URL_ENV) {
        Ok(clicked) if hrc_herdr::link::locator_from(&clicked).is_some() => {
            SetupApp::new(steps).opening_on(hrc_tui::SetupStep::Join)
        }
        _ => SetupApp::new(steps),
    };
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the channel setup screen",
        source,
    })?;

    // Each arm calls the same function the CLI does, so there is one
    // implementation of what creating, inviting and joining mean.
    match outcome {
        SetupOutcome::Initialize { passphrase } => {
            let context = Context {
                paths: context.paths.clone(),
                passphrase: Some(SecretString::from(passphrase)),
            };
            crate::commands::init(&context)
        }
        SetupOutcome::CreateChannel { repo } => crate::commands::create(context, &repo, None),
        SetupOutcome::CreateInvite { github_user } => {
            crate::commands::invite_create(context, &github_user, DEFAULT_INVITE_LIFETIME)
        }
        SetupOutcome::Join { invite_code } => crate::commands::join(context, &invite_code),
        SetupOutcome::Quit => Ok(json!({
            "status": "ok",
            "done": Value::Null,
        })),
    }
}

/// How long an invite issued from the setup screen stays usable.
///
/// The CLI makes this an argument. The screen does not ask, because a
/// lifetime is a question most people cannot answer usefully at the moment
/// they are trying to invite someone, and a day is long enough to pass a
/// code along and short enough that a forgotten one lapses.
pub(super) const DEFAULT_INVITE_LIFETIME: &str = "24h";

/// The setup steps that make sense for this installation right now.
pub(super) fn available_steps(context: &Context) -> Result<Vec<hrc_tui::SetupStep>> {
    // Before there are keys there is nothing else to offer: every other step
    // signs something.
    //
    // The directory alone is not the test. `paths.ensure` creates it, so an
    // installation that has merely been looked at has one; what decides this
    // is whether anything is in it. Reading the store itself would need the
    // passphrase, which is the thing this step exists to choose.
    let initialized = std::fs::read_dir(context.paths.keys())
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);

    if !initialized {
        return Ok(vec![hrc_tui::SetupStep::Initialize]);
    }

    let database = Database::open(context.paths.database())?;
    let has_channel = !database.channels()?.is_empty();

    Ok(if has_channel {
        // Joining again would mean a second channel, which the rest of the
        // CLI does not support yet: `only_channel` refuses when there is
        // more than one.
        vec![hrc_tui::SetupStep::CreateInvite]
    } else {
        vec![hrc_tui::SetupStep::CreateChannel, hrc_tui::SetupStep::Join]
    })
}
