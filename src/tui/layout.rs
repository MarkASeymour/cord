use ratatui::layout::{Constraint, Layout, Rect};

/// The minimum terminal width that still shows the contacts sidebar. Below it the
/// sidebar is hidden and the conversation header names the active contact.
const MIN_WIDTH_FOR_SIDEBAR: u16 = 60;
const SIDEBAR_WIDTH: u16 = 24;
const LOG_STRIP_HEIGHT: u16 = 8;

pub struct Regions {
    pub status: Rect,
    pub sidebar: Option<Rect>,
    pub chat: Rect,
    pub log: Option<Rect>,
    pub input: Rect,
    pub footer: Rect,
}

/// Status bar, a body, an input line, and a footer. The body is the contacts
/// sidebar on the left (hidden on a narrow terminal) and, on the right, the
/// conversation. When `show_log` is set the system log takes a strip at the
/// bottom of the conversation; the conversation shrinks so its newest lines stay
/// visible above it.
pub fn split(area: Rect, show_log: bool) -> Regions {
    let rows = Layout::vertical([
        Constraint::Length(1), // status
        Constraint::Min(1),    // body
        Constraint::Length(1), // input
        Constraint::Length(1), // footer
    ])
    .split(area);
    let (status, body, input, footer) = (rows[0], rows[1], rows[2], rows[3]);

    let (sidebar, main) = if area.width >= MIN_WIDTH_FOR_SIDEBAR {
        let cols =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)]).split(body);
        (Some(cols[0]), cols[1])
    } else {
        (None, body)
    };

    let (chat, log) = if show_log {
        let panes =
            Layout::vertical([Constraint::Min(1), Constraint::Length(LOG_STRIP_HEIGHT)]).split(main);
        (panes[0], Some(panes[1]))
    } else {
        (main, None)
    };

    Regions {
        status,
        sidebar,
        chat,
        log,
        input,
        footer,
    }
}
