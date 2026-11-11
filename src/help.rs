use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

const HOTKEYS: &[(&str, &str)] = &[
    ("l, Up", "Move up"),
    ("k, Down", "Move down"),
    ("j", "Parent directory"),
    (";", "Enter directory"),
    ("Left", "Collapse directory"),
    ("Right", "Expand directory"),
    ("x", "Toggle expansion"),
    ("g g", "First item"),
    ("g e", "Last item"),
    ("/", "Search current list"),
    ("f", "Find all files"),
    ("o", "Open"),
    ("i", "Preview"),
    ("Space", "Mark item"),
    ("n", "New file"),
    ("Shift+n", "New directory"),
    ("r", "Rename"),
    ("y", "Copy"),
    ("c", "Cut"),
    ("p", "Paste"),
    ("Ctrl+p", "Paste in root directory"),
    ("Shift+c", "Clear marks and clipboard"),
    ("Shift+t", "Trash"),
    ("e", "Edit"),
    ("Shift+e", "Edit in window"),
    ("Ctrl+e", "Edit in terminal"),
    ("s", "Shell"),
    ("Shift+s", "Shell in window"),
    ("Ctrl+s", "Shell in root directory"),
    ("Shift+v", "VS Code"),
    ("Ctrl+v", "VS Code in root directory"),
    ("Shift+f", "File manager"),
    ("Shift+i", "Image viewer"),
    ("Shift+d", "Drag and drop"),
    ("Shift+l", "Git log"),
    ("Shift+z", "Toggle hidden files"),
    ("Ctrl+r", "Refresh"),
    (":", "Command entry"),
    ("h", "Confirm file chooser"),
    ("q", "Quit"),
    ("Shift+q", "Quit and change directory"),
    ("?", "Show hotkeys"),
    ("Enter", "Run command (entry mode)"),
    ("Esc", "Cancel command (entry mode)"),
];

#[derive(Default)]
pub struct Help {
    pub visible: bool,
    offset: usize,
}

impl Help {
    pub fn open(&mut self) {
        self.visible = true;
        self.offset = 0;
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('?') => self.visible = false,
                        KeyCode::Down | KeyCode::Char('k') => self.scroll_down(),
                        KeyCode::Up | KeyCode::Char('l') => {
                            self.offset = self.offset.saturating_sub(1);
                        }
                        _ => {}
                    }
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => self.scroll_down(),
                MouseEventKind::ScrollUp => self.offset = self.offset.saturating_sub(1),
                _ => {}
            },
            _ => {}
        }
    }

    fn scroll_down(&mut self) {
        self.offset = (self.offset + 1).min(HOTKEYS.len());
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let size = frame.area();
        let columns = if size.width >= 96 { 2 } else { 1 };
        let rows = HOTKEYS.len().div_ceil(columns);
        let width = (columns as u16 * 47 + 2).min(size.width);
        let height = (rows as u16 + 3).min(size.height.saturating_sub(2));
        let area = Rect::new(
            size.x + (size.width - width) / 2,
            size.y + (size.height - height) / 2,
            width,
            height,
        );
        let style = Style::default().fg(Color::White).bg(Color::Rgb(39, 42, 45));
        let block = Block::default()
            .title("Help")
            .borders(Borders::ALL)
            .style(style);
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        let visible_rows = inner.height.saturating_sub(1) as usize;
        self.offset = self.offset.min(rows.saturating_sub(visible_rows));
        let column_width = inner.width / columns as u16;
        for column in 0..columns {
            let lines: Vec<Line> = (self.offset..rows)
                .take(visible_rows)
                .filter_map(|row| HOTKEYS.get(column * rows + row))
                .map(|(keys, action)| {
                    Line::from(vec![
                        Span::styled(format!("{keys:<12}"), Style::default().fg(Color::Cyan)),
                        Span::raw(*action),
                    ])
                })
                .collect();
            let area = Rect::new(
                inner.x + column as u16 * column_width,
                inner.y,
                column_width,
                visible_rows as u16,
            );
            frame.render_widget(Paragraph::new(lines), area);
        }
        if inner.height > 0 {
            let footer = format!(
                "Esc, ?: Close | k,l,Up,Down: Scroll {}/{}",
                self.offset + 1,
                rows.saturating_sub(visible_rows) + 1
            );
            let area = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
            frame.render_widget(Paragraph::new(footer), area);
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use crossterm::event::KeyEvent;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    #[test]
    fn popup_handles_keys() {
        let mut help = Help::default();
        help.open();
        for code in [KeyCode::Char('q'), KeyCode::Char('f'), KeyCode::Char('k')] {
            help.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        }
        assert!(help.visible);
        assert_eq!(help.offset, 1);
        help.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!help.visible);
        help.open();
        assert_eq!(help.offset, 0);
        help.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::SHIFT,
        )));
        assert!(!help.visible);
    }

    #[test]
    fn popup_fits_resized_terminal() -> Result<()> {
        let mut help = Help::default();
        for (width, height) in [(120, 40), (80, 24), (30, 10), (1, 1), (0, 0)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            help.offset = HOTKEYS.len();
            terminal.draw(|frame| help.render(frame))?;
            if width == 120 {
                let buffer = terminal.backend().buffer();
                let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                assert!(text.contains("Help"));
                assert!(text.contains("Find all files"));
                assert!(text.contains("Show hotkeys"));
            }
        }
        Ok(())
    }
}
