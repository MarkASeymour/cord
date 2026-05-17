use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::runtime::events::TransportCmd;

use super::App;

pub fn handle(app: &mut App, event: Event) -> Option<TransportCmd> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => handle_key(app, key),
        _ => None,
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Option<TransportCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
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
        KeyCode::Char(c)
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
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
    if text == "/help" || text == "/?" {
        show_help(app);
        return None;
    }
    if let Some(addr) = text.strip_prefix("/connect ") {
        let addr = addr.trim().to_string();
        if addr.is_empty() {
            app.push_system("usage: /connect <address>");
            return None;
        }
        app.push_system(format!("dispatching /connect {addr}"));
        return Some(TransportCmd::ConnectOnion(addr));
    }
    if text == "/quit" || text == "/q" {
        app.should_quit = true;
        return None;
    }
    if text.starts_with('/') {
        app.push_system(format!("unknown command: {text}. type /help."));
        return None;
    }
    app.push_system(format!("(echo) {text}"));
    None
}

fn show_help(app: &mut App) {
    app.push_system("commands:");
    app.push_system("  /connect <address>   dial a peer over Tor (debug only)");
    app.push_system("  /help, /?            show this");
    app.push_system("  /quit, /q            exit");
    app.push_system("keys:");
    app.push_system("  Esc, Ctrl-C          exit");
    app.push_system("  Enter                submit");
    app.push_system("note: messaging is not implemented yet. typed text echoes back.");
}
