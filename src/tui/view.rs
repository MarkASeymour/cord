use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::contacts::ContactStatus;
use crate::runtime::events::DeliveryStatus;

use super::{layout, App, ChatEntry, InputMode, Pane, SasPrompt, TransportState};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let layout::Regions {
        status,
        sidebar,
        chat,
        log,
        input,
        footer,
    } = layout::split(area, app.show_log);

    let instance = app
        .identity
        .config_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?");
    let mut status_spans = vec![
        Span::styled("cord", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" ({instance})"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(format!("  ·  {}", transport_label(&app.transport_state))),
        Span::raw(format!("  ·  peers: {}", app.peers.len())),
        Span::raw(format!("  ·  queue: {}", queue_label(app))),
    ];
    // when the log is hidden, nudge with the unread count instead of dumping the line
    if !app.show_log && app.log_unread > 0 {
        status_spans.push(Span::styled(
            format!("  ·  log {}", app.log_unread),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(status_spans)), status);

    let active_label = app.active.and_then(|k| {
        app.contacts
            .iter()
            .find(|c| c.blob.noise_static_pub == k)
            .map(|c| c.short_label())
    });

    let chat_lines: Vec<Line> = app
        .active
        .and_then(|k| app.conversations.get(&k))
        .map(|convo| convo.entries.iter().map(entry_line).collect())
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                "no conversation. /to <name> to pick a verified contact, then type.",
                Style::default().add_modifier(Modifier::DIM),
            ))]
        });

    if let Some(sidebar) = sidebar {
        let sidebar_block = Block::default()
            .borders(Borders::TOP | Borders::RIGHT)
            .title(Line::from(Span::styled(
                " contacts ",
                Style::default().add_modifier(Modifier::BOLD),
            )));
        frame.render_widget(
            Paragraph::new(sidebar_lines(app))
                .block(sidebar_block)
                .wrap(Wrap { trim: true }),
            sidebar,
        );
    }

    let chat_title = match &active_label {
        Some(name) => format!("conversation: {name}"),
        None => "conversation".to_string(),
    };
    // The top border takes one row, so the content height is one less.
    let chat_inner_h = chat.height.saturating_sub(1);
    let chat_max = max_scroll(&chat_lines, chat.width, chat_inner_h);
    app.chat_view.max = chat_max;
    app.chat_view.page = chat_inner_h.max(1);
    let chat_offset = app.chat_view.resolve(chat_max);
    let chat_block = Block::default().borders(Borders::TOP).title(pane_title(
        &chat_title,
        app.focus == Pane::Conversation,
        app.chat_view.is_scrolled(),
    ));
    frame.render_widget(
        Paragraph::new(chat_lines)
            .block(chat_block)
            .wrap(Wrap { trim: false })
            .scroll((chat_offset, 0)),
        chat,
    );

    if let Some(log) = log {
        app.log_unread = 0;
        let log_lines: Vec<Line> = app
            .system_log
            .iter()
            .map(|text| {
                Line::from(Span::styled(
                    format!("· {text}"),
                    Style::default().add_modifier(Modifier::DIM),
                ))
            })
            .collect();
        let log_inner_h = log.height.saturating_sub(1);
        let log_max = max_scroll(&log_lines, log.width, log_inner_h);
        app.log_view.max = log_max;
        app.log_view.page = log_inner_h.max(1);
        let log_offset = app.log_view.resolve(log_max);
        let log_block = Block::default().borders(Borders::TOP).title(pane_title(
            "system log",
            app.focus == Pane::SystemLog,
            app.log_view.is_scrolled(),
        ));
        frame.render_widget(
            Paragraph::new(log_lines)
                .block(log_block)
                .wrap(Wrap { trim: false })
                .scroll((log_offset, 0)),
            log,
        );
    }

    match &app.mode {
        InputMode::Passphrase(p) => {
            let stars = "*".repeat(p.buffer.chars().count());
            let mut spans = vec![
                Span::styled(
                    format!("{}: ", p.title()),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(stars),
            ];
            if let Some(err) = &p.error {
                spans.push(Span::styled(
                    format!("   {err}"),
                    Style::default().fg(Color::Red),
                ));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), input);
        }
        InputMode::Confirm(c) => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    c.question(),
                    Style::default().add_modifier(Modifier::BOLD),
                ))),
                input,
            );
        }
        InputMode::Sas(_) => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "pairing: compare the SAS in the box, then y to verify or n to reject",
                    Style::default().add_modifier(Modifier::DIM),
                ))),
                input,
            );
        }
        InputMode::Normal => {
            let input_line = if app.input_buffer.is_empty() {
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(
                        "type /help for commands",
                        Style::default().add_modifier(Modifier::DIM),
                    ),
                ])
            } else {
                Line::from(vec![Span::raw("> "), Span::raw(&app.input_buffer)])
            };
            frame.render_widget(Paragraph::new(input_line), input);
        }
    }

    let footer_line = Line::from(Span::styled(
        "Enter send  ·  /to switch  ·  Ctrl-L log  ·  PgUp/PgDn scroll  ·  /help  ·  Ctrl-C quit",
        Style::default().add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(footer_line), footer);

    if app.show_help {
        render_help_panel(frame, area);
    }

    if let InputMode::Sas(p) = &app.mode {
        render_sas_modal(frame, area, p);
    }

    let cursor_col = match &app.mode {
        InputMode::Passphrase(p) => {
            p.title().chars().count() as u16 + 2 + p.buffer.chars().count() as u16
        }
        InputMode::Confirm(c) => c.question().chars().count() as u16,
        InputMode::Sas(_) => 0,
        InputMode::Normal => 2 + app.input_buffer.chars().count() as u16,
    };
    let cursor_x = input.x + cursor_col;
    frame.set_cursor_position((cursor_x.min(input.x + input.width.saturating_sub(1)), input.y));
}

/// A centered popup for the pairing SAS comparison. Renders over the body so the
/// decision cannot be ignored by accident.
fn render_sas_modal(frame: &mut Frame, area: Rect, p: &SasPrompt) {
    let w = 62.min(area.width.saturating_sub(4));
    let h = 9.min(area.height.saturating_sub(2));
    let popup = centered_rect(area, w, h);
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from(Span::styled(
            format!("pair with {}", p.label),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("compare this code aloud over a channel you both trust:"),
        Line::from(Span::styled(
            p.sas.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "y = verify (codes match)    n = reject    Esc = decide later",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    let block = Block::default().borders(Borders::ALL).title(" pairing ");
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        popup,
    );
}

const HELP_COMMANDS: &[(&str, &str)] = &[
    ("/share [name]", "print your contact blob to the log"),
    ("/pair <blob>", "add a peer's blob as pending"),
    ("/verify <name|hex>", "verify after comparing the SAS"),
    ("/reject <name|hex>", "reject a contact"),
    ("/unpair <name|hex>", "remove a contact"),
    ("/to <name|hex>", "make this the active contact"),
    ("/msg <name> <text>", "one off send, keep active contact"),
    ("/connect <name|hex>", "dial a contact or .onion address"),
    ("/passphrase", "enable the offline queue"),
    ("/unlock", "unlock the offline queue"),
    ("/clearqueue", "discard all queued messages"),
    ("/help, /?", "show this panel"),
    ("/quit, /q", "exit"),
];

const HELP_KEYS: &[(&str, &str)] = &[
    ("Ctrl-C", "quit"),
    ("Esc", "clear the input, or close a panel"),
    ("Ctrl-L", "show or hide the system log"),
    ("Tab", "switch the pane the scroll keys drive"),
    ("Enter", "send a message or run a command"),
];

/// A centered reference panel listing the commands and keys, kept separate from
/// the system log so reference and diagnostics do not mix.
fn render_help_panel(frame: &mut Frame, area: Rect) {
    let w = 66.min(area.width.saturating_sub(2));
    let h = (HELP_COMMANDS.len() + HELP_KEYS.len() + 6) as u16;
    let h = h.min(area.height.saturating_sub(2));
    let popup = centered_rect(area, w, h);
    frame.render_widget(Clear, popup);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut lines = vec![Line::from(Span::styled("commands", bold))];
    for (cmd, desc) in HELP_COMMANDS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {cmd:<20}"), bold),
            Span::styled(*desc, dim),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("keys", bold)));
    for (k, desc) in HELP_KEYS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<20}"), bold),
            Span::styled(*desc, dim),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Esc to close", dim)));

    let block = Block::default().borders(Borders::ALL).title(" help ");
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        popup,
    );
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

/// The maximum scroll offset for a wrapped paragraph: the row that puts the
/// last line at the bottom of the viewport. `Line::width` is exact for lines
/// that fit on one row (the common case here); a long line that word wraps is
/// estimated, which can leave the very last line a row low but never hides it.
fn max_scroll(lines: &[Line], width: u16, height: u16) -> u16 {
    if width == 0 || height == 0 {
        return 0;
    }
    let w = width as usize;
    let rows: usize = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(w))
        .sum();
    (rows as u16).saturating_sub(height)
}

/// Header line for a pane: bold when focused, dim otherwise, with a hint when
/// it is scrolled up off the newest line.
fn pane_title(name: &str, focused: bool, scrolled: bool) -> Line<'static> {
    let mut label = format!(" {name} ");
    if scrolled {
        label.push_str("[↑ scrolled, End to follow] ");
    }
    let style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    Line::from(Span::styled(label, style))
}

fn entry_line(entry: &ChatEntry) -> Line<'static> {
    match entry {
        ChatEntry::Incoming { from, text, .. } => Line::from(vec![
            Span::styled(
                format!("{from}: "),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(text.clone()),
        ]),
        ChatEntry::Outgoing { text, status, .. } => Line::from(vec![
            Span::styled("you: ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(text.clone()),
            Span::raw("  "),
            Span::styled(status.marker(), delivery_style(*status)),
        ]),
    }
}

/// One row per contact: a caret on the active one, the name, and either the
/// unread count or the pairing status glyph. Never shows connection state.
fn sidebar_lines(app: &App) -> Vec<Line<'static>> {
    if app.contacts.is_empty() {
        return vec![Line::from(Span::styled(
            "no contacts. /pair <blob>",
            Style::default().add_modifier(Modifier::DIM),
        ))];
    }
    app.contacts
        .iter()
        .map(|c| {
            let key = c.blob.noise_static_pub;
            let is_active = app.active == Some(key);
            let unread = app.conversations.get(&key).map(|cv| cv.unread).unwrap_or(0);
            let name_style = if is_active {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let right = if unread > 0 {
                Span::styled(
                    format!(" {unread}"),
                    Style::default().add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!(" {}", status_glyph(c.status)),
                    Style::default().add_modifier(Modifier::DIM),
                )
            };
            Line::from(vec![
                Span::styled(if is_active { "› " } else { "  " }, name_style),
                Span::styled(c.short_label(), name_style),
                right,
            ])
        })
        .collect()
}

fn status_glyph(status: ContactStatus) -> &'static str {
    match status {
        ContactStatus::Verified => "✓",
        ContactStatus::Pending => "?",
        ContactStatus::Rejected => "✗",
    }
}

fn delivery_style(status: DeliveryStatus) -> Style {
    match status {
        DeliveryStatus::Delivered => Style::default().fg(Color::Green),
        DeliveryStatus::Failed => Style::default().fg(Color::Red),
        _ => Style::default().add_modifier(Modifier::DIM),
    }
}

fn queue_label(app: &App) -> &'static str {
    if app.vault_ready {
        "on"
    } else if app.vault_locked {
        "locked"
    } else {
        "off"
    }
}

fn transport_label(state: &TransportState) -> String {
    match state {
        TransportState::Bootstrapping => "starting…".to_string(),
        TransportState::BootstrappingTor {
            percent, summary, ..
        } => format!("tor {percent}% ({summary})"),
        TransportState::Lan { .. } => "lan".to_string(),
        TransportState::Onion { lan: Some(_), .. } => "onion ready (+lan)".to_string(),
        TransportState::Onion { lan: None, .. } => "onion ready".to_string(),
        TransportState::Failed(msg) => format!("error: {msg}"),
    }
}
