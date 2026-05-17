use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::{layout, App, ChatEntry, TransportState};

const SHORT_ONION: usize = 16;

pub fn render(frame: &mut Frame, app: &App) {
    let (status, chat, input, footer) = layout::split(frame.area());

    let status_line = Line::from(vec![
        Span::styled("cord ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("status: "),
        Span::raw(transport_label(&app.transport_state)),
        Span::raw("  you: "),
        Span::raw(you_label(&app.transport_state, &app.identity.peer_id.short())),
        Span::raw(format!("  peers: {}", app.peers.len())),
    ]);
    frame.render_widget(Paragraph::new(status_line), status);

    let chat_lines: Vec<Line> = app
        .chat_log
        .iter()
        .map(|entry| match entry {
            ChatEntry::System(text) => Line::from(Span::styled(
                format!("· {text}"),
                Style::default().add_modifier(Modifier::DIM),
            )),
        })
        .collect();
    frame.render_widget(
        Paragraph::new(chat_lines).wrap(Wrap { trim: false }),
        chat,
    );

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

    let footer_line = Line::from(Span::styled(
        "Esc quit  ·  Enter send  ·  /help commands  ·  /quit exit",
        Style::default().add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(footer_line), footer);

    let cursor_x = input.x + 2 + app.input_buffer.chars().count() as u16;
    frame.set_cursor_position((cursor_x.min(input.x + input.width.saturating_sub(1)), input.y));
}

fn transport_label(state: &TransportState) -> String {
    match state {
        TransportState::Bootstrapping => "bootstrapping…".to_string(),
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
