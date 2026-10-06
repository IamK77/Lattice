//! A setup-owned alternate screen, separate from the main TUI's lifetime.
use ratatui::crossterm::{
    cursor::{MoveTo, Show},
    event::DisableBracketedPaste,
    execute,
    style::ResetColor,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct Screen {
    active: bool,
    was_raw: bool,
}
impl Screen {
    pub fn enter() -> io::Result<Self> {
        let mut screen = Self {
            active: true,
            was_raw: terminal::is_raw_mode_enabled()?,
        };
        if let Err(error) = execute!(io::stderr(), EnterAlternateScreen) {
            let _ = screen.close();
            return Err(error);
        }
        Ok(screen)
    }
    pub fn close(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // Attempt both cleanup operations even if one fails. Never erase the
        // primary buffer: leaving the alternate restores the caller's screen.
        let modes = if self.was_raw {
            terminal::enable_raw_mode()
        } else {
            terminal::disable_raw_mode()
        };
        let display = execute!(
            io::stderr(),
            DisableBracketedPaste,
            ResetColor,
            Show,
            LeaveAlternateScreen
        );
        if modes.is_ok() && display.is_ok() {
            self.active = false;
        }
        modes.and(display)
    }
    pub fn size(&self) -> io::Result<(usize, usize)> {
        terminal::size().map(|(w, h)| (usize::from(w), usize::from(h)))
    }
    pub fn draw(&mut self, lines: &[String]) -> io::Result<()> {
        let mut out = io::stderr().lock();
        execute!(out, ResetColor, MoveTo(0, 0), Clear(ClearType::All))?;
        for line in lines {
            write!(out, "{line}\r\n")?;
        }
        out.flush()
    }
    pub fn busy(&mut self, lines: &[String]) -> io::Result<()> {
        // Inquire restores cooked mode after each answer. Keep Ctrl+C as input
        // while an audited synchronous request is in flight, rather than letting
        // SIGINT terminate the process before terminal restoration can run.
        terminal::enable_raw_mode()?;
        self.draw(lines)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub(super) fn safe(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}
/// Bound a menu label without changing the value selected by its index.
pub(super) fn clip(text: &str, width: usize) -> String {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    if text.width() <= width {
        return text;
    }
    let mut result = String::new();
    for grapheme in text.graphemes(true) {
        if result.width() + grapheme.width() >= width {
            break;
        }
        result.push_str(grapheme);
    }
    if width > 0 {
        result.push('…');
    }
    result
}
/// Wrap by display width and grapheme boundaries, leaving the terminal's last
/// column unused so rendering never relies on delayed automatic line wrapping.
pub(super) fn wrap(text: &str, columns: usize) -> Vec<String> {
    let width = columns.saturating_sub(1).max(1);
    let safe = safe(text);
    let mut lines = vec![];
    for paragraph in safe.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for grapheme in paragraph.graphemes(true) {
            let size = grapheme.width();
            if used + size > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push_str(grapheme);
            used += size;
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapping_is_display_width_aware_and_never_emits_terminal_controls() {
        let input = "Model 模型 e\u{301}\n\u{1b}[2J";
        let lines = wrap(input, 10);
        assert!(lines.iter().all(|line| line.width() < 10));
        assert!(!lines.join("").contains('\u{1b}'));
        assert_eq!(lines.join(""), safe(input).replace('\n', ""));
        assert!(lines.iter().any(|line| line.contains("e\u{301}")));
    }
}
