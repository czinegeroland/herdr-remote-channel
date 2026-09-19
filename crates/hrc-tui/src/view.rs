//! Drawing the trusted inbox.
//!
//! Two rules from PRD section 27 shape this. Nothing relies on colour alone,
//! so every state that matters is spelled out in words as well — a terminal
//! without colour, or a reader who cannot distinguish it, must be able to see
//! which message is selected, whether a body is hidden, and what is about to
//! happen. And the body is drawn only when it has been revealed, because a
//! screen that painted it by default would disclose it to anyone who walked
//! past.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::app::{App, Focus};

/// Draws the whole screen.
pub fn render(frame: &mut Frame<'_>, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    render_list(frame, app, areas[0]);
    render_body(frame, app, areas[1]);
    render_status(frame, app, areas[2]);
}

/// The pending messages, with the selection marked in text.
fn render_list(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items: Vec<ListItem<'_>> = app
        .items()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            // The marker is what carries the selection for a reader who
            // cannot see the highlight; the highlight is an addition to it,
            // never a replacement for it.
            let marker = if index == app.selected_index() {
                "> "
            } else {
                "  "
            };

            let endpoint = if item.view.endpoint_label.is_empty() {
                String::new()
            } else {
                format!(" -> {}", item.view.endpoint_label)
            };

            let line = format!(
                "{marker}{} [{}]{endpoint}  {} bytes",
                item.view.sender_local_name, item.view.kind, item.view.plaintext_bytes
            );

            let style = if index == app.selected_index() {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            ListItem::new(Line::from(Span::styled(line, style)))
        })
        .collect();

    let title = format!("Pending approval ({})", app.items().len());
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

/// The body pane, which is empty until a human reveals it.
fn render_body(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let (title, text) = match (app.is_revealed(), app.selected()) {
        (true, Some(item)) => ("Message body (not yet approved)", item.body.clone()),
        (false, Some(_)) => (
            "Message body",
            "HIDDEN. This message has not been approved. Press Enter to read it.".to_owned(),
        ),
        _ => ("Message body", "No message selected.".to_owned()),
    };

    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    let lines = wrapped_lines(&text, inner.width);
    app.set_body_viewport(lines.len(), inner.height);

    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.body_scroll(), 0))
            .block(block),
        area,
    );
}

/// The status line, which always says what the next key does.
fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let prefix = match app.focus() {
        Focus::Confirm => "CONFIRM: ",
        Focus::Body => "READING: ",
        Focus::List => "",
    };

    frame.render_widget(
        Paragraph::new(format!("{prefix}{}", app.status()))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// Draws the membership approval screen.
///
/// The same two rules apply. Nothing relies on colour, so the selection is
/// carried by a text marker. And unlike the message screen there is nothing
/// to hide: the safety phrase must be on screen for the administrator to
/// read it aloud, so it is drawn in full for the selected request.
pub fn render_joins(frame: &mut Frame<'_>, app: &crate::join::JoinApp) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Length(8),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let items: Vec<ListItem<'_>> = app
        .requests()
        .iter()
        .enumerate()
        .map(|(index, request)| {
            let marker = if index == app.selected_index() {
                "> "
            } else {
                "  "
            };

            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::raw(request.principal_id.clone()),
                Span::raw(format!("  asked {}", request.created_at)),
            ]))
        })
        .collect();

    let title = format!(
        "Join requests for {} ({})",
        app.channel_local_name(),
        app.requests().len()
    );
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
        areas[0],
    );

    // The detail panel is where the verification actually happens, so the
    // phrase is given its own line and labelled with what to do with it.
    let detail = match app.selected() {
        Some(request) => vec![
            Line::from(format!("principal  {}", request.principal_id)),
            Line::from(format!("device     {}", request.device_id)),
            Line::from(""),
            Line::from("Read this phrase to the joiner and have them read it back:"),
            Line::from(Span::styled(
                request.safety_phrase.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ],
        None => vec![Line::from("No join requests are waiting.")],
    };

    frame.render_widget(
        Paragraph::new(detail)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Safety phrase"),
            )
            .wrap(Wrap { trim: false }),
        areas[1],
    );

    let status = match app.proposed() {
        Some(proposed) => proposed.prompt.clone(),
        None => app.status().to_owned(),
    };

    frame.render_widget(
        Paragraph::new(status)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        areas[2],
    );
}

/// Draws the composition screen.
///
/// The focused half is named in its own border title rather than only
/// highlighted, for the same reason the selection marker exists: a reader
/// who cannot distinguish the highlight still has to know where their typing
/// is going.
pub fn render_compose(frame: &mut Frame<'_>, app: &crate::compose::ComposeApp) {
    use crate::compose::ComposeFocus;

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let items: Vec<ListItem<'_>> = app
        .recipients()
        .iter()
        .enumerate()
        .map(|(index, recipient)| {
            let marker = if index == app.selected_index() {
                "> "
            } else {
                "  "
            };

            // Inactive members are spelled out rather than greyed, so the
            // state survives a terminal without colour.
            let state = if recipient.active { "" } else { "  [inactive]" };

            // A reply target says what it answers, so the list distinguishes
            // "write to Bob" from "answer Bob's question" without relying on
            // the person remembering which entry is which.
            let answering = match &recipient.answering {
                Some(answering) => format!("  (reply to {answering})"),
                None => String::new(),
            };

            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::raw(recipient.principal_id.clone()),
                Span::raw(answering),
                Span::raw(state),
            ]))
        })
        .collect();

    let recipients_title = match app.focus() {
        ComposeFocus::Recipient => "To (choosing)",
        ComposeFocus::Body => "To",
    };

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(recipients_title),
        ),
        areas[0],
    );

    let body_title = match app.focus() {
        ComposeFocus::Body => format!("{} (writing)", app.kind().as_str()),
        ComposeFocus::Recipient => app.kind().as_str().to_owned(),
    };

    let body = if app.body().is_empty() {
        "Tab here and type.".to_owned()
    } else {
        app.body().to_owned()
    };

    frame.render_widget(
        Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title(body_title))
            .wrap(Wrap { trim: false }),
        areas[1],
    );

    let status = app.proposed().unwrap_or_else(|| app.status()).to_owned();
    frame.render_widget(
        Paragraph::new(status)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        areas[2],
    );
}

/// Draws the channel setup screen.
///
/// The typed value comes from `displayed_input`, never from the buffer
/// directly, so a secret cannot be echoed by a change here.
pub fn render_setup(frame: &mut Frame<'_>, app: &crate::setup::SetupApp) {
    use crate::setup::SetupFocus;

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Length(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let items: Vec<ListItem<'_>> = app
        .steps()
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let marker = if index == app.selected_index() {
                "> "
            } else {
                "  "
            };

            ListItem::new(Line::from(vec![Span::raw(marker), Span::raw(step.title())]))
        })
        .collect();

    let menu_title = match app.focus() {
        SetupFocus::Menu => "Set up this channel (choosing)",
        SetupFocus::Input => "Set up this channel",
    };

    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(menu_title)),
        areas[0],
    );

    let detail = match app.focus() {
        SetupFocus::Input => vec![
            Line::from(app.prompt()),
            Line::from(""),
            Line::from(Span::styled(
                app.displayed_input(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ],
        SetupFocus::Menu => vec![Line::from("Choose a step and press Enter.")],
    };

    frame.render_widget(
        Paragraph::new(detail)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        areas[1],
    );

    let status = app.proposed().unwrap_or_else(|| app.status()).to_owned();
    frame.render_widget(
        Paragraph::new(status)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        areas[2],
    );
}

/// Draws the passphrase prompt.
///
/// The entry comes from `masked`, never from the buffer, so no change here
/// can paint a passphrase onto a screen other people can see.
pub fn render_passphrase(frame: &mut Frame<'_>, app: &crate::passphrase::PassphraseApp) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(0)])
        .split(frame.area());

    let lines = vec![
        Line::from(format!("Unlock this installation to {}.", app.reason())),
        Line::from(""),
        Line::from(Span::styled(
            app.masked(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Enter: unlock   Esc: cancel"),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Key store passphrase"),
            )
            .wrap(Wrap { trim: false }),
        areas[0],
    );
}

/// Draws the context disclosure screen.
///
/// Findings are spelled out rather than summarised. A count would tell a
/// person that something is wrong without telling them what, and the whole
/// purpose of the preview is that they can judge it.
pub fn render_context(frame: &mut Frame<'_>, app: &crate::context::ContextApp) {
    use crate::context::ContextFocus;

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let packages: Vec<ListItem<'_>> = app
        .drafts()
        .iter()
        .enumerate()
        .map(|(index, draft)| {
            let marker = if index == app.selected_draft_index() {
                "> "
            } else {
                "  "
            };

            ListItem::new(Line::from(format!(
                "{marker}{}  {}",
                draft.package_id,
                &draft.digest[..draft.digest.len().min(16)]
            )))
        })
        .collect();

    frame.render_widget(
        List::new(packages).block(Block::default().borders(Borders::ALL).title(
            match app.focus() {
                ContextFocus::Package => "Package (choosing)",
                _ => "Package",
            },
        )),
        areas[0],
    );

    let recipients: Vec<ListItem<'_>> = app
        .recipients()
        .iter()
        .enumerate()
        .map(|(index, recipient)| {
            let marker = if index == app.selected_recipient_index() {
                "> "
            } else {
                "  "
            };
            ListItem::new(Line::from(format!("{marker}{recipient}")))
        })
        .collect();

    frame.render_widget(
        List::new(recipients).block(Block::default().borders(Borders::ALL).title(
            match app.focus() {
                ContextFocus::Recipient => "To (choosing)",
                _ => "To",
            },
        )),
        areas[1],
    );

    let detail = match app.preview() {
        None => "Nothing has been disclosed yet. Press Enter to ask what this package contains."
            .to_owned(),
        Some(preview) => {
            let mut text = format!(
                "{} bytes in {} item(s)",
                preview.total_bytes,
                preview.items.len()
            );

            for (kind, bytes) in &preview.items {
                text.push_str(&format!("\n  {kind}  {bytes} bytes"));
            }

            if !preview.secrets.is_empty() {
                text.push_str("\n\nSecret-scan findings — this package cannot be sent:");
                for finding in &preview.secrets {
                    text.push_str(&format!("\n  {finding}"));
                }
            }

            if !preview.excluded.is_empty() {
                text.push_str("\n\nExcluded by the repository's own rules:");
                for path in &preview.excluded {
                    text.push_str(&format!("\n  {path}"));
                }
            }

            text.push_str("\n\nCanonical package content:\n");
            text.push_str(&preview.content);
            text
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title("What would be sent");
    let inner = block.inner(areas[2]);
    let lines = wrapped_lines(&detail, inner.width);
    app.set_preview_viewport(lines.len(), inner.height);

    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.preview_scroll(), 0))
            .block(block),
        areas[2],
    );

    let status = app.proposed().unwrap_or_else(|| app.status()).to_owned();
    frame.render_widget(
        Paragraph::new(status)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        areas[3],
    );
}

/// Wraps plain text into exactly the rows handed to a scrolling paragraph.
///
/// Doing the wrapping here gives the state machines a real rendered row
/// count, so resize clamping and PageUp/PageDown never depend on a guessed
/// terminal height or on byte counts that differ from display width.
fn wrapped_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut rendered = Vec::new();

    for source in text.split('\n') {
        if source.is_empty() {
            rendered.push(Line::from(String::new()));
            continue;
        }

        let mut row = String::new();
        let mut row_width = 0usize;

        for character in source.chars() {
            let character_width = Span::raw(character.to_string()).width();
            if !row.is_empty() && row_width.saturating_add(character_width) > width {
                rendered.push(Line::from(std::mem::take(&mut row)));
                row_width = 0;
            }

            row.push(character);
            row_width = row_width.saturating_add(character_width);

            if row_width >= width {
                rendered.push(Line::from(std::mem::take(&mut row)));
                row_width = 0;
            }
        }

        if !row.is_empty() {
            rendered.push(Line::from(row));
        }
    }

    if rendered.is_empty() {
        rendered.push(Line::from(String::new()));
    }

    rendered
}

/// Draws the membership screen.
///
/// Inactive members and revoked devices are spelled out rather than greyed,
/// for the same reason the selection is marked in text: a terminal without
/// colour, or a reader who cannot distinguish it, still has to be able to see
/// which of these is already gone.
pub fn render_members(frame: &mut Frame<'_>, app: &crate::members::MembersApp) {
    use crate::members::MemberFocus;

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Length(7),
            Constraint::Length(4),
        ])
        .split(frame.area());

    let members: Vec<ListItem<'_>> = app
        .members()
        .iter()
        .enumerate()
        .map(|(index, member)| {
            let marker = if index == app.selected_member_index() {
                "> "
            } else {
                "  "
            };

            let mut labels = Vec::new();
            if member.administrator {
                labels.push("admin");
            }
            if !member.active {
                labels.push("removed");
            }
            if member.is_local {
                labels.push("this installation");
            }

            let suffix = if labels.is_empty() {
                String::new()
            } else {
                format!("  [{}]", labels.join(", "))
            };

            ListItem::new(Line::from(format!(
                "{marker}{}{suffix}",
                member.principal_id
            )))
        })
        .collect();

    frame.render_widget(
        List::new(members).block(
            Block::default()
                .borders(Borders::ALL)
                .title(match app.focus() {
                    MemberFocus::Member => "Members (choosing)",
                    MemberFocus::Device => "Members",
                }),
        ),
        areas[0],
    );

    let devices: Vec<ListItem<'_>> = app
        .selected_member()
        .map(|member| {
            member
                .devices
                .iter()
                .enumerate()
                .map(|(index, device)| {
                    let marker = if app.focus() == MemberFocus::Device
                        && index == app.selected_device_index()
                    {
                        "> "
                    } else {
                        "  "
                    };

                    let state = if device.active { "" } else { "  [revoked]" };
                    ListItem::new(Line::from(format!("{marker}{}{state}", device.device_id)))
                })
                .collect()
        })
        .unwrap_or_default();

    frame.render_widget(
        List::new(devices).block(
            Block::default()
                .borders(Borders::ALL)
                .title(match app.focus() {
                    MemberFocus::Device => "Devices (choosing)",
                    MemberFocus::Member => "Devices",
                }),
        ),
        areas[1],
    );

    let status = app.proposed().unwrap_or_else(|| app.status()).to_owned();
    frame.render_widget(
        Paragraph::new(status)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        areas[2],
    );
}

/// Draws the metadata-only inbox side view.
///
/// `now` is the current RFC 3339 UTC time, passed in rather than read here
/// so the rendered buffer is a pure function of its inputs and the age column
/// can be asserted in a test.
///
/// Every string that reaches the buffer is either a fixed local word, a
/// locally resolved name, a validated identifier, or a number. Nothing on
/// this screen is derived from a body, a subject, an attachment name, or any
/// other field a sender chose — the row type does not carry one to derive it
/// from.
pub fn render_inbox(frame: &mut Frame<'_>, app: &crate::inbox::InboxApp, now: &str) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let visible = app.visible();

    frame.render_widget(
        Paragraph::new(Line::from(format!(
            "{} pending - {} shown{}",
            app.pending(),
            visible.len(),
            if app.stale() { " - STALE" } else { "" },
        ))),
        areas[0],
    );

    // A split can be one column wide. Every row is built to the width the
    // pane actually has rather than to a fixed layout, so a narrow pane loses
    // the columns on the right instead of wrapping each message across three
    // lines and making the list unreadable.
    let width = areas[1].width.saturating_sub(2) as usize;

    let items: Vec<ListItem<'_>> = if visible.is_empty() {
        vec![ListItem::new(Line::from(match app.filter() {
            crate::inbox::InboxFilter::All => "Nothing has arrived yet.",
            _ => "No messages match this filter.",
        }))]
    } else {
        visible
            .iter()
            .map(|row| {
                let selected = app.selected_message_id() == Some(row.message_id.as_str());
                let marker = if selected { "> " } else { "  " };

                let line = format!(
                    "{marker}{} {} {} {}",
                    clamp(&row.sender_local_name, 10),
                    clamp(&row.kind, 8),
                    clamp(&crate::inbox::age(&row.arrival_at, now), 4),
                    row.disposition.as_str(),
                );

                let style = if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                ListItem::new(Line::from(Span::styled(clamp(&line, width), style)))
            })
            .collect()
    };

    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(format!(
            "{} ({})",
            app.channel_local_name(),
            app.filter().as_str()
        ))),
        areas[1],
    );

    frame.render_widget(
        Paragraph::new(app.status().to_owned()).wrap(Wrap { trim: false }),
        areas[2],
    );
}

/// Pads or truncates to an exact display width.
///
/// Truncation rather than wrapping, because a wrapped row in a narrow split
/// would push the rows below it out of alignment and make the list harder to
/// scan than a shortened name is to read. `chars` rather than bytes so a
/// multi-byte name is cut at a character boundary.
fn clamp(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let characters: Vec<char> = value.chars().collect();

    if characters.len() <= width {
        let mut padded: String = characters.into_iter().collect();
        padded.extend(std::iter::repeat_n(' ', width - padded.chars().count()));
        return padded;
    }

    if width == 1 {
        return "…".to_owned();
    }

    characters[..width - 1].iter().collect::<String>() + "…"
}
