use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::runtime::events::{DeliveryStatus, Passphrase, TransportCmd};

use super::{App, InputMode, PassphrasePurpose, PassphraseStage};

pub fn handle(
    app: &mut App,
    event: Event,
    cmd_tx: &mpsc::Sender<TransportCmd>,
) -> Option<TransportCmd> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            if matches!(app.mode, InputMode::Passphrase(_)) {
                handle_passphrase_key(app, key)
            } else if matches!(app.mode, InputMode::Confirm(_)) {
                handle_confirm_key(app, key)
            } else if matches!(app.mode, InputMode::Sas(_)) {
                handle_sas_key(app, key)
            } else if matches!(app.mode, InputMode::ThemePicker(_)) {
                handle_theme_picker_key(app, key)
            } else {
                handle_key(app, key, cmd_tx)
            }
        }
        _ => None,
    }
}

fn handle_passphrase_key(app: &mut App, key: KeyEvent) -> Option<TransportCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        app.should_quit = true;
        return None;
    }
    match key.code {
        KeyCode::Esc => {
            app.mode = InputMode::Normal;
            app.push_system("passphrase entry cancelled.");
            None
        }
        KeyCode::Enter => submit_passphrase(app),
        KeyCode::Backspace => {
            if let InputMode::Passphrase(p) = &mut app.mode {
                p.buffer.pop();
            }
            None
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            if let InputMode::Passphrase(p) = &mut app.mode {
                p.buffer.push(c);
            }
            None
        }
        _ => None,
    }
}

fn handle_confirm_key(app: &mut App, key: KeyEvent) -> Option<TransportCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        app.should_quit = true;
        return None;
    }
    let (remote_static, id, text, label) = match &app.mode {
        InputMode::Confirm(c) => (c.remote_static, c.id, c.text.clone(), c.label.clone()),
        _ => return None,
    };
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            app.mode = InputMode::Normal;
            app.push_outgoing(remote_static, text.clone(), id, DeliveryStatus::Sending);
            Some(TransportCmd::SendMessage {
                remote_static,
                id,
                text,
            })
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.mode = InputMode::Normal;
            app.push_system(format!("discarded message to {label}"));
            None
        }
        _ => None,
    }
}

fn handle_theme_picker_key(app: &mut App, key: KeyEvent) -> Option<TransportCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        app.should_quit = true;
        return None;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.theme_picker_move(false),
        KeyCode::Down | KeyCode::Char('j') => app.theme_picker_move(true),
        KeyCode::Enter => app.theme_picker_apply(),
        KeyCode::Esc => app.theme_picker_cancel(),
        _ => {}
    }
    None
}

fn handle_sas_key(app: &mut App, key: KeyEvent) -> Option<TransportCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        app.should_quit = true;
        return None;
    }
    let remote_static = match &app.mode {
        InputMode::Sas(p) => p.remote_static,
        _ => return None,
    };
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            app.mode = InputMode::Normal;
            app.resolve_pairing(remote_static, true);
            None
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            app.mode = InputMode::Normal;
            app.resolve_pairing(remote_static, false);
            None
        }
        KeyCode::Esc => {
            app.mode = InputMode::Normal;
            app.push_system(
                "pairing deferred; the contact stays pending. reconnect to compare the SAS again, or /verify or /reject by name.",
            );
            None
        }
        _ => None,
    }
}

fn submit_passphrase(app: &mut App) -> Option<TransportCmd> {
    let InputMode::Passphrase(prompt) = &mut app.mode else {
        return None;
    };
    match prompt.purpose {
        PassphrasePurpose::Unlock => {
            if prompt.buffer.is_empty() {
                prompt.error = Some("passphrase is empty".into());
                return None;
            }
            let pass = std::mem::take(&mut prompt.buffer);
            prompt.stage = PassphraseStage::Waiting;
            prompt.error = None;
            Some(TransportCmd::UnlockVault(Passphrase(pass)))
        }
        PassphrasePurpose::Create => {
            // Take the stage out by value so reassigning it below is borrow free.
            let stage = std::mem::replace(&mut prompt.stage, PassphraseStage::Enter);
            match stage {
                PassphraseStage::Enter => {
                    if prompt.buffer.is_empty() {
                        prompt.error = Some("passphrase is empty".into());
                        return None;
                    }
                    let first = std::mem::take(&mut prompt.buffer);
                    prompt.stage = PassphraseStage::Confirm { first };
                    prompt.error = None;
                    None
                }
                PassphraseStage::Confirm { first } => {
                    if prompt.buffer == first {
                        let pass = std::mem::take(&mut prompt.buffer);
                        prompt.stage = PassphraseStage::Waiting;
                        prompt.error = None;
                        Some(TransportCmd::SetupVault(Passphrase(pass)))
                    } else {
                        prompt.buffer.clear();
                        prompt.error = Some("passphrases did not match, try again".into());
                        None
                    }
                }
                PassphraseStage::Waiting => {
                    prompt.stage = PassphraseStage::Waiting;
                    None
                }
            }
        }
    }
}

fn handle_key(
    app: &mut App,
    key: KeyEvent,
    cmd_tx: &mpsc::Sender<TransportCmd>,
) -> Option<TransportCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        app.should_quit = true;
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('l') | KeyCode::Char('L'))
    {
        app.toggle_log();
        return None;
    }
    // readline style line editing, so Home/End/PgUp/PgDn stay on pane scroll
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') => {
                app.input.home();
                return None;
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                app.input.end();
                return None;
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                app.input.delete_word();
                return None;
            }
            KeyCode::Char('u') | KeyCode::Char('U') => {
                app.input.kill_to_start();
                return None;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => {
            // cancel: close the help panel if open, else clear the input; never quit
            if app.show_help {
                app.show_help = false;
            } else {
                app.input.clear();
            }
            None
        }
        KeyCode::Enter => submit(app, cmd_tx),
        KeyCode::Backspace => {
            app.input.backspace();
            None
        }
        KeyCode::Char(c)
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
            app.input.insert(c);
            None
        }
        KeyCode::Left => {
            app.input.left();
            None
        }
        KeyCode::Right => {
            app.input.right();
            None
        }
        KeyCode::Up => {
            app.history_prev();
            None
        }
        KeyCode::Down => {
            app.history_next();
            None
        }
        KeyCode::Tab => {
            app.toggle_focus();
            None
        }
        KeyCode::PageUp => {
            app.focused_view().page_up();
            None
        }
        KeyCode::PageDown => {
            app.focused_view().page_down();
            None
        }
        KeyCode::Home => {
            app.focused_view().to_top();
            None
        }
        KeyCode::End => {
            app.focused_view().to_bottom();
            None
        }
        _ => None,
    }
}

fn submit(app: &mut App, cmd_tx: &mpsc::Sender<TransportCmd>) -> Option<TransportCmd> {
    let text = app.input.text.trim().to_string();
    app.input.clear();
    if text.is_empty() {
        return None;
    }
    app.history_push(&text);
    if text == "/help" || text == "/?" {
        app.show_help = true;
        return None;
    }
    app.show_help = false; // any other action dismisses the commands panel
    if let Some(rest) = text.strip_prefix("/msg ") {
        let trimmed = rest.trim();
        match trimmed.split_once(char::is_whitespace) {
            Some((name, body)) => {
                let body = body.trim();
                if body.is_empty() {
                    app.push_system("usage: /msg <name> <text>");
                } else {
                    app.send_to_contact(name.trim(), body, cmd_tx);
                }
            }
            None => app.push_system("usage: /msg <name> <text>"),
        }
        return None;
    }
    if let Some(rest) = text.strip_prefix("/pair ") {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            app.push_system("usage: /pair <blob>");
            return None;
        }
        app.pair_with(trimmed);
        return None;
    }
    if let Some(rest) = text.strip_prefix("/to ") {
        let q = rest.trim();
        if q.is_empty() {
            app.push_system("usage: /to <name-or-hex>");
            return None;
        }
        app.switch_to(q);
        return None;
    }
    if text == "/theme" {
        app.open_theme_picker();
        return None;
    }
    if let Some(rest) = text.strip_prefix("/theme ") {
        let name = rest.trim();
        if name.is_empty() {
            app.push_system("usage: /theme <name>");
            return None;
        }
        app.set_theme(name);
        return None;
    }
    if let Some(rest) = text.strip_prefix("/verify ") {
        let q = rest.trim();
        if q.is_empty() {
            app.push_system("usage: /verify <name-or-hex>");
            return None;
        }
        app.verify_contact(q);
        return None;
    }
    if let Some(rest) = text.strip_prefix("/reject ") {
        let q = rest.trim();
        if q.is_empty() {
            app.push_system("usage: /reject <name-or-hex>");
            return None;
        }
        app.reject_contact(q);
        return None;
    }
    if let Some(rest) = text.strip_prefix("/unpair ") {
        let q = rest.trim();
        if q.is_empty() {
            app.push_system("usage: /unpair <name-or-hex>");
            return None;
        }
        app.unpair_contact(q);
        return None;
    }
    if text == "/share" {
        app.show_log = true; // the blob must be readable to copy
        app.share_blob(None);
        return None;
    }
    if let Some(rest) = text.strip_prefix("/share ") {
        app.show_log = true; // the blob must be readable to copy
        let name = rest.trim();
        if name.is_empty() {
            app.share_blob(None);
        } else {
            app.share_blob(Some(name.to_string()));
        }
        return None;
    }
    if let Some(arg) = text.strip_prefix("/connect ") {
        let arg = arg.trim().to_string();
        if arg.is_empty() {
            app.push_system("usage: /connect <name-or-hex> (or a full .onion address)");
            return None;
        }
        let address = if arg.ends_with(".onion") {
            arg
        } else {
            match app.resolve_contact_onion(&arg) {
                Ok(a) => a,
                Err(msg) => {
                    app.push_system(msg);
                    return None;
                }
            }
        };
        app.push_system(format!("dispatching /connect {address}"));
        return Some(TransportCmd::ConnectOnion(address));
    }
    if text == "/passphrase" {
        app.begin_passphrase_create();
        return None;
    }
    if text == "/unlock" {
        app.begin_passphrase_unlock();
        return None;
    }
    if text == "/clearqueue" {
        return Some(TransportCmd::ClearQueue);
    }
    if text == "/quit" || text == "/q" {
        app.should_quit = true;
        return None;
    }
    if text.starts_with('/') {
        app.push_system(format!("unknown command: {text}. type /help."));
        return None;
    }
    app.send_active(&text, cmd_tx);
    None
}

