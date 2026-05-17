use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::runtime::events::TransportCmd;

use super::App;

pub fn handle(app: &mut App, event: Event) -> Option<TransportCmd> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key),
        _ => None,
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Option<TransportCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return None;
    }
    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
            None
        }
        KeyCode::Enter => submit(app),
        KeyCode::Backspace => {
            app.input_buffer.pop();
            None
        }
        KeyCode::Char(c) => {
            app.input_buffer.push(c);
            None
        }
        _ => None,
    }
}

fn submit(app: &mut App) -> Option<TransportCmd> {
    let text = app.input_buffer.trim().to_string();
    app.input_buffer.clear();
    if text.is_empty() {
        return None;
    }
    if let Some(addr) = text.strip_prefix("/connect ") {
        let addr = addr.trim().to_string();
        if addr.is_empty() {
            app.push_system("usage: /connect <onion-address>");
            return None;
        }
        app.push_system(format!("dispatching /connect {addr}"));
        return Some(TransportCmd::ConnectOnion(addr));
    }
    app.push_system(format!("echo: {text}"));
    None
}
