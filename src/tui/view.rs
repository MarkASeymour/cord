use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::contacts::ContactStatus;
use crate::runtime::events::DeliveryStatus;

use super::theme::{Theme, NAMES};
use super::{layout, App, ChatEntry, InputMode, Pane, SasPrompt, ThemePicker, TransportState};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let theme = app.theme;
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
    frame.render_widget(
        Paragraph::new(Line::from(status_spans))
            .style(Style::default().fg(theme.status_fg).bg(theme.status_bg)),
        status,
    );

    let active_label = app.active.and_then(|k| {
        app.contacts
            .iter()
            .find(|c| c.blob.noise_static_pub == k)
            .map(|c| c.short_label())
    });

    let chat_lines: Vec<Line> = app
        .active
        .and_then(|k| app.conversations.get(&k))
        .map(|convo| convo.entries.iter().map(|e| entry_line(e, theme)).collect())
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                "no conversation. /to <name> to pick a verified contact, then type.",
                Style::default().fg(theme.dim),
            ))]
        });

    if let Some(sidebar) = sidebar {
        let sidebar_block = Block::default()
            .borders(Borders::TOP | Borders::RIGHT)
            .border_style(Style::default().fg(theme.border))
            .title(Line::from(Span::styled(
                " contacts ",
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
            )));
        frame.render_widget(
            Paragraph::new(sidebar_lines(app, theme, sidebar.width))
                .block(sidebar_block)
                .wrap(Wrap { trim: false }),
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
    let chat_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border))
        .title(pane_title(
            &chat_title,
            app.focus == Pane::Conversation,
            app.chat_view.is_scrolled(),
            theme,
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
            .map(|text| Line::from(Span::styled(format!("· {text}"), Style::default().fg(theme.dim))))
            .collect();
        let log_inner_h = log.height.saturating_sub(1);
        let log_max = max_scroll(&log_lines, log.width, log_inner_h);
        app.log_view.max = log_max;
        app.log_view.page = log_inner_h.max(1);
        let log_offset = app.log_view.resolve(log_max);
        let log_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme.border))
            .title(pane_title(
                "system log",
                app.focus == Pane::SystemLog,
                app.log_view.is_scrolled(),
                theme,
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
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(stars, Style::default().fg(theme.text)),
            ];
            if let Some(err) = &p.error {
                spans.push(Span::styled(
                    format!("   {err}"),
                    Style::default().fg(theme.error),
                ));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), input);
        }
        InputMode::Confirm(c) => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    c.question(),
                    Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
                ))),
                input,
            );
        }
        InputMode::Sas(_) => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "pairing: compare the SAS in the box, then y to verify or n to reject",
                    Style::default().fg(theme.dim),
                ))),
                input,
            );
        }
        InputMode::ThemePicker(_) => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "theme picker: ↑/↓ to preview, Enter to apply, Esc to cancel",
                    Style::default().fg(theme.dim),
                ))),
                input,
            );
        }
        InputMode::Normal => {
            let input_line = if app.input.text.is_empty() {
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled("type /help for commands", Style::default().fg(theme.dim)),
                ])
            } else {
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(app.input.text.clone(), Style::default().fg(theme.text)),
                ])
            };
            frame.render_widget(Paragraph::new(input_line), input);
        }
    }

    let footer_line = Line::from(Span::styled(
        "Enter send  ·  /to switch  ·  Ctrl-L log  ·  PgUp/PgDn scroll  ·  /help  ·  Ctrl-C quit",
        Style::default().fg(theme.dim),
    ));
    frame.render_widget(Paragraph::new(footer_line), footer);

    if app.show_help {
        render_help_panel(frame, area, theme);
    }

    if let InputMode::Sas(p) = &app.mode {
        render_sas_modal(frame, area, p, theme);
    }

    if let InputMode::ThemePicker(p) = &app.mode {
        render_theme_picker(frame, area, p, theme);
    }

    let cursor_col = match &app.mode {
        InputMode::Passphrase(p) => {
            p.title().chars().count() as u16 + 2 + p.buffer.chars().count() as u16
        }
        InputMode::Confirm(c) => c.question().chars().count() as u16,
        InputMode::Sas(_) => 0,
        InputMode::ThemePicker(_) => 0,
        InputMode::Normal => 2 + app.input.cursor as u16,
    };
    let cursor_x = input.x + cursor_col;
    frame.set_cursor_position((cursor_x.min(input.x + input.width.saturating_sub(1)), input.y));
}

/// A centered popup for the pairing SAS comparison. Renders over the body so the
/// decision cannot be ignored by accident.
fn render_sas_modal(frame: &mut Frame, area: Rect, p: &SasPrompt, theme: Theme) {
    let w = 62.min(area.width.saturating_sub(4));
    let h = 9.min(area.height.saturating_sub(2));
    let popup = centered_rect(area, w, h);
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from(Span::styled(
            format!("pair with {}", p.label),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "compare this code aloud over a channel you both trust:",
            Style::default().fg(theme.text),
        )),
        Line::from(Span::styled(
            p.sas.clone(),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "y = verify (codes match)    n = reject    Esc = decide later",
            Style::default().fg(theme.dim),
        )),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::from(Span::styled(
            " pairing ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
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
    ("/theme [name]", "open the theme picker (or by name)"),
    ("/help, /?", "show this panel"),
    ("/quit, /q", "exit"),
];

const HELP_KEYS: &[(&str, &str)] = &[
    ("Ctrl-C", "quit"),
    ("Esc", "clear the input, or close a panel"),
    ("Ctrl-L", "show or hide the system log"),
    ("Tab", "switch the pane the scroll keys drive"),
    ("Enter", "send a message or run a command"),
    ("editing", "arrows, Ctrl-A/E/W/U, Up/Down history"),
];

/// A centered reference panel listing the commands and keys, kept separate from
/// the system log so reference and diagnostics do not mix.
fn render_help_panel(frame: &mut Frame, area: Rect, theme: Theme) {
    let w = 66.min(area.width.saturating_sub(2));
    let h = (HELP_COMMANDS.len() + HELP_KEYS.len() + 6) as u16;
    let h = h.min(area.height.saturating_sub(2));
    let popup = centered_rect(area, w, h);
    frame.render_widget(Clear, popup);

    let header = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let cmd = Style::default().fg(theme.text);
    let dim = Style::default().fg(theme.dim);
    let mut lines = vec![Line::from(Span::styled("commands", header))];
    for (c, desc) in HELP_COMMANDS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {c:<20}"), cmd),
            Span::styled(*desc, dim),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("keys", header)));
    for (k, desc) in HELP_KEYS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<20}"), cmd),
            Span::styled(*desc, dim),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Esc to close", dim)));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::from(Span::styled(
            " help ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        popup,
    );
}

/// The live theme picker: a small list with the selection highlighted. The whole
/// UI is already rendered in the previewed theme, so this box previews too.
fn render_theme_picker(frame: &mut Frame, area: Rect, picker: &ThemePicker, theme: Theme) {
    let w = 44.min(area.width.saturating_sub(2));
    let h = ((NAMES.len() + 4) as u16).min(area.height.saturating_sub(2));
    let popup = centered_rect(area, w, h);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    for (i, name) in NAMES.iter().enumerate() {
        let selected = i == picker.index;
        let style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let prefix = if selected { "› " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{prefix}{name}"), style)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑/↓ preview · Enter apply · Esc cancel",
        Style::default().fg(theme.dim),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::from(Span::styled(
            " theme ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
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

/// Header line for a pane: accent when focused, dim otherwise, with a hint when
/// it is scrolled up off the newest line.
fn pane_title(name: &str, focused: bool, scrolled: bool, theme: Theme) -> Line<'static> {
    let mut label = format!(" {name} ");
    if scrolled {
        label.push_str("[↑ scrolled, End to follow] ");
    }
    let style = if focused {
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim)
    };
    Line::from(Span::styled(label, style))
}

fn entry_line(entry: &ChatEntry, theme: Theme) -> Line<'static> {
    match entry {
        ChatEntry::Incoming { from, text, ts } => Line::from(vec![
            Span::styled(format!("{ts} "), Style::default().fg(theme.dim)),
            Span::styled(
                format!("{from}: "),
                Style::default().fg(theme.peer).add_modifier(Modifier::BOLD),
            ),
            Span::styled(text.clone(), Style::default().fg(theme.text)),
        ]),
        ChatEntry::Outgoing {
            text, status, ts, ..
        } => Line::from(vec![
            Span::styled(format!("{ts} "), Style::default().fg(theme.dim)),
            Span::styled(
                "you: ",
                Style::default().fg(theme.self_).add_modifier(Modifier::BOLD),
            ),
            Span::styled(text.clone(), Style::default().fg(theme.text)),
            Span::raw("  "),
            Span::styled(status.marker(), delivery_style(*status, theme)),
        ]),
    }
}

/// One row per contact: a caret on the active one, the name, and either the
/// unread count or the pairing status glyph, right aligned. Never shows
/// connection state.
fn sidebar_lines(app: &App, theme: Theme, width: u16) -> Vec<Line<'static>> {
    if app.contacts.is_empty() {
        return vec![Line::from(Span::styled(
            "no contacts. /pair <blob>",
            Style::default().fg(theme.dim),
        ))];
    }
    let inner = width.saturating_sub(2) as usize; // leave room before the right border
    app.contacts
        .iter()
        .map(|c| {
            let key = c.blob.noise_static_pub;
            let is_active = app.active == Some(key);
            let unread = app.conversations.get(&key).map(|cv| cv.unread).unwrap_or(0);

            let (marker, marker_style) = if unread > 0 {
                (
                    unread.to_string(),
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                )
            } else {
                (status_glyph(c.status).to_string(), glyph_style(c.status, theme))
            };

            let name_style = if is_active {
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            let caret = if is_active { "› " } else { "  " };

            let marker_len = marker.chars().count();
            let avail = inner.saturating_sub(2 + marker_len + 1);
            let mut name: String = c.short_label().chars().take(avail).collect();
            while name.chars().count() < avail {
                name.push(' ');
            }

            Line::from(vec![
                Span::styled(caret, name_style),
                Span::styled(name, name_style),
                Span::raw(" "),
                Span::styled(marker, marker_style),
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

fn glyph_style(status: ContactStatus, theme: Theme) -> Style {
    let color = match status {
        ContactStatus::Verified => theme.success,
        ContactStatus::Pending => theme.warn,
        ContactStatus::Rejected => theme.error,
    };
    Style::default().fg(color)
}

fn delivery_style(status: DeliveryStatus, theme: Theme) -> Style {
    match status {
        DeliveryStatus::Delivered => Style::default().fg(theme.success),
        DeliveryStatus::Failed => Style::default().fg(theme.error),
        _ => Style::default().fg(theme.dim),
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
