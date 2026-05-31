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
            app.push_outgoing(label, text.clone(), id, DeliveryStatus::Sending);
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
    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
            None
        }
        KeyCode::Enter => submit(app, cmd_tx),
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

fn submit(app: &mut App, cmd_tx: &mpsc::Sender<TransportCmd>) -> Option<TransportCmd> {
    let text = app.input_buffer.trim().to_string();
    app.input_buffer.clear();
    if text.is_empty() {
        return None;
    }
    if text == "/help" || text == "/?" {
        show_help(app);
        return None;
    }
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
    if text == "/contacts" {
        app.list_contacts();
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
        app.share_blob(None);
        return None;
    }
    if let Some(rest) = text.strip_prefix("/share ") {
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
    app.push_system("  /share [name]        print your own contact blob, optionally with a display name");
    app.push_system("  /pair <blob>         add a peer's contact blob (status: pending)");
    app.push_system("  /contacts            list paired contacts");
    app.push_system("  /verify <name-or-hex>  upgrade a pending contact to verified (after comparing the SAS aloud)");
    app.push_system("  /reject <name-or-hex>  mark a contact as rejected");
    app.push_system("  /unpair <name-or-hex>  remove a contact entirely (use to start over)");
    app.push_system("  /msg <name> <text>   send a text message to a verified contact");
    app.push_system("  /connect <name-or-hex>  dial a verified contact (or a raw .onion address)");
    app.push_system("  /passphrase          set a passphrase to enable the encrypted offline queue");
    app.push_system("  /unlock              unlock the offline queue for this session");
    app.push_system("  /help, /?            show this");
    app.push_system("  /quit, /q            exit");
    app.push_system("keys:");
    app.push_system("  Esc, Ctrl-C          exit");
    app.push_system("  Enter                submit");
    app.push_system("note: messaging requires both sides verified. if a contact is offline, cord asks whether to queue the message (encrypted on disk; needs a passphrase via /passphrase); answer no to discard it.");
}
