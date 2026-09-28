//! Interpret mouse events against the last frame; never execute UI operations.
use crate::terminal_host::status::Opens;
use ratatui::{
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
};

pub(super) struct Targets<'a> {
    pub area: Rect,
    pub offset: usize,
    pub owner: &'a [Option<String>],
    pub jump: Option<Rect>,
    pub status: &'a [(Rect, Opens)],
    pub links: &'a [(Rect, String)],
}

#[derive(Debug, PartialEq)]
pub(super) enum Intent {
    PanelUp(usize),
    PanelDown(usize),
    Scroll(isize),
    Status(Opens),
    Jump,
    Link(String),
    Card(String),
}

pub(super) fn interpret(
    event: MouseEvent,
    panel_visible: bool,
    targets: Targets<'_>,
) -> Option<Intent> {
    match event.kind {
        MouseEventKind::ScrollUp if panel_visible => Some(Intent::PanelUp(3)),
        MouseEventKind::ScrollDown if panel_visible => Some(Intent::PanelDown(3)),
        MouseEventKind::ScrollUp => Some(Intent::Scroll(3)),
        MouseEventKind::ScrollDown => Some(Intent::Scroll(-3)),
        MouseEventKind::Down(MouseButton::Left) => {
            let (col, row) = (event.column, event.row);
            if let Some(opens) = targets.status_at(col, row) {
                Some(Intent::Status(opens))
            } else if targets.jump_at(col, row) {
                Some(Intent::Jump)
            } else if let Some(url) = targets.url_at(col, row) {
                Some(Intent::Link(url.to_owned()))
            } else {
                targets.card_at(col, row).map(Intent::Card)
            }
        }
        _ => None,
    }
}

pub(super) fn rect_has(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

impl<'a> Targets<'a> {
    pub fn url_at(&self, col: u16, row: u16) -> Option<&'a str> {
        if self.jump_at(col, row) {
            return None;
        }
        self.links
            .iter()
            .find(|(rect, _)| rect_has(*rect, col, row))
            .map(|(_, url)| url.as_str())
    }
    pub fn card_at(&self, col: u16, row: u16) -> Option<String> {
        if !rect_has(self.area, col, row) {
            return None;
        }
        let idx = self.offset + (row - self.area.y) as usize;
        self.owner.get(idx).cloned().flatten()
    }
    pub fn jump_at(&self, col: u16, row: u16) -> bool {
        self.jump.is_some_and(|r| rect_has(r, col, row))
    }
    pub fn status_at(&self, col: u16, row: u16) -> Option<Opens> {
        self.status
            .iter()
            .find(|(r, _)| rect_has(*r, col, row))
            .map(|(_, opens)| *opens)
    }
}

#[cfg(test)]
#[path = "mouse_intent/tests.rs"]
mod tests;
