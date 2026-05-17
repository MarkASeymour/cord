use ratatui::layout::{Constraint, Layout, Rect};

pub fn split(area: Rect) -> (Rect, Rect, Rect, Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    (chunks[0], chunks[1], chunks[2], chunks[3])
}
