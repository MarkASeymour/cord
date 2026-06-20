use ratatui::layout::{Constraint, Layout, Rect};

/// Split the screen into status, conversation, system log, input, and footer.
/// The conversation and system log share the middle, 2 to 1, so chat keeps the
/// bulk of the space and the diagnostic log stays a glanceable strip.
pub fn split(area: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // status
        Constraint::Fill(2),   // conversation
        Constraint::Fill(1),   // system log
        Constraint::Length(1), // input
        Constraint::Length(1), // footer
    ])
    .split(area);
    (chunks[0], chunks[1], chunks[2], chunks[3], chunks[4])
}
