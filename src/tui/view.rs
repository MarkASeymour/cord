use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::runtime::events::DeliveryStatus;

use super::{layout, App, ChatEntry, InputMode, Pane, TransportState};

const SHORT_ONION: usize = 16;

pub fn render(frame: &mut Frame, app: &mut App) {
    let (status, chat, log, input, footer) = layout::split(frame.area());

    let status_line = Line::from(vec![
        Span::styled("cord ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("status: "),
        Span::raw(transport_label(&app.transport_state)),
        Span::raw("  you: "),
        Span::raw(you_label(&app.transport_state, &app.identity.peer_id.short())),
        Span::raw(format!("  peers: {}", app.peers.len())),
        Span::raw(format!("  queue: {}", queue_label(app))),
    ]);
    frame.render_widget(Paragraph::new(status_line), status);

    let chat_lines: Vec<Line> = app
        .chat_log
        .iter()
        .map(|entry| match entry {
            ChatEntry::Incoming { from, text } => Line::from(vec![
                Span::styled(
                    format!("{from}: "),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(text.clone()),
            ]),
            ChatEntry::Outgoing {
                to, text, status, ..
            } => Line::from(vec![
                Span::styled(
                    format!("you → {to}: "),
                    Style::default().add_modifier(Modifier::DIM),
                ),
                Span::raw(text.clone()),
                Span::raw("  "),
                Span::styled(status.marker(), delivery_style(*status)),
            ]),
        })
        .collect();
    // The top border takes one row, so the content height is one less.
    let chat_inner_h = chat.height.saturating_sub(1);
    let chat_max = max_scroll(&chat_lines, chat.width, chat_inner_h);
    app.chat_view.max = chat_max;
    app.chat_view.page = chat_inner_h.max(1);
    let chat_offset = app.chat_view.resolve(chat_max);
    let chat_block = Block::default().borders(Borders::TOP).title(pane_title(
        "conversation",
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
        "Enter send  ·  Tab switch pane  ·  PgUp/PgDn scroll  ·  End follow  ·  /help  ·  Esc quit",
        Style::default().add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(footer_line), footer);

    let cursor_col = match &app.mode {
        InputMode::Passphrase(p) => {
            p.title().chars().count() as u16 + 2 + p.buffer.chars().count() as u16
        }
        InputMode::Confirm(c) => c.question().chars().count() as u16,
        InputMode::Normal => 2 + app.input_buffer.chars().count() as u16,
    };
    let cursor_x = input.x + cursor_col;
    frame.set_cursor_position((cursor_x.min(input.x + input.width.saturating_sub(1)), input.y));
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
        TransportState::Bootstrapping => "bootstrapping…".to_string(),
        TransportState::BootstrappingTor {
            percent,
            summary,
            lan: Some(lan),
        } => format!("tor: {percent}% ({summary})  lan ({lan})"),
        TransportState::BootstrappingTor {
            percent,
            summary,
            lan: None,
        } => format!("tor: {percent}% ({summary})"),
        TransportState::Lan { listening_on } => format!("lan ({listening_on})"),
        TransportState::Onion { lan: Some(lan), .. } => {
            format!("onion + lan ({lan})")
        }
        TransportState::Onion { lan: None, .. } => "onion".to_string(),
        TransportState::Failed(msg) => format!("error: {msg}"),
    }
}

fn you_label(state: &TransportState, peer_id_short: &str) -> String {
    match state {
        TransportState::Onion { onion_name, .. } => {
            if onion_name.len() > SHORT_ONION {
                format!("{}…", &onion_name[..SHORT_ONION])
            } else {
                onion_name.clone()
            }
        }
        _ => peer_id_short.to_string(),
    }
}
