use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Copy,
    Move,
    NewDirectory,
    NewFile,
    Path,
    Quit,
    Search,
    Shell,
}

impl Command {
    pub fn name(self) -> &'static str {
        match self {
            Self::Copy => "cp",
            Self::Move => "mv",
            Self::NewDirectory => "new-dir",
            Self::NewFile => "new-file",
            Self::Path => "path",
            Self::Quit => "quit",
            Self::Search => "search",
            Self::Shell => "sh",
        }
    }

    pub fn takes_argument(self) -> bool {
        matches!(
            self,
            Self::NewDirectory | Self::NewFile | Self::Search | Self::Shell
        )
    }
}

const COMMANDS: &[Command] = &[
    Command::Copy,
    Command::Move,
    Command::NewDirectory,
    Command::NewFile,
    Command::Path,
    Command::Quit,
    Command::Search,
    Command::Shell,
];

struct Grid {
    width: usize,
    columns: usize,
    rows: usize,
}

impl Grid {
    fn new(entries: &[Command], width: u16) -> Self {
        let item = entries
            .iter()
            .map(|command| command.name().len() + 1)
            .max()
            .unwrap_or(0);
        let columns = (usize::from(width.saturating_sub(1)) / (item + 2))
            .max(1)
            .min(entries.len().max(1));
        Self {
            width: item,
            columns,
            rows: entries.len().div_ceil(columns),
        }
    }
}

#[derive(Default)]
pub struct Suggestions {
    query: String,
    selected: usize,
}

fn matching_commands(input: &str) -> Vec<Command> {
    let typed = input
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (exact, prefixed): (Vec<_>, Vec<_>) = COMMANDS
        .iter()
        .copied()
        .filter(|command| command.name().starts_with(&typed))
        .partition(|command| command.name() == typed);
    exact.into_iter().chain(prefixed).collect()
}

impl Suggestions {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn update(&mut self, input: &str) {
        if self.query != input {
            self.query = input.to_owned();
            self.selected = 0;
        }
    }

    pub fn navigate(&mut self, input: &str, key: KeyEvent, width: u16) -> bool {
        self.update(input);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let (delta, across) = match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => (-1, false),
            KeyCode::Down | KeyCode::Tab => (1, false),
            KeyCode::Up | KeyCode::BackTab => (-1, false),
            KeyCode::Char('n' | 'k') if control => (1, false),
            KeyCode::Char('p' | 'l') if control => (-1, false),
            KeyCode::Right => (1, true),
            KeyCode::Left => (-1, true),
            KeyCode::Char(';') if control => (1, true),
            KeyCode::Char('j') if control => (-1, true),
            _ => return false,
        };
        let entries = matching_commands(input);
        if entries.is_empty() {
            return true;
        }
        if across {
            let rows = Grid::new(&entries, width).rows as isize;
            let target = self.selected as isize + delta * rows;
            if (0..entries.len() as isize).contains(&target) {
                self.selected = target as usize;
            }
        } else {
            self.selected =
                (self.selected as isize + delta).rem_euclid(entries.len() as isize) as usize;
        }
        true
    }

    pub fn resolve<'a>(&mut self, input: &'a str) -> Option<(Command, &'a str)> {
        self.update(input);
        if input.trim().is_empty() {
            return None;
        }
        let command = *matching_commands(input).get(self.selected)?;
        let argument = input
            .trim()
            .split_once(char::is_whitespace)
            .map_or("", |(_, rest)| rest.trim());
        (command.takes_argument() || argument.is_empty()).then_some((command, argument))
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, input: &str, available: Rect) {
        self.update(input);
        let entries = matching_commands(input);
        if entries.is_empty() || available.width < 3 || available.height < 3 {
            return;
        }
        let grid = Grid::new(&entries, available.width);
        let height = (grid.rows as u16 + 2).min(available.height);
        let area = Rect::new(
            available.x,
            available.bottom() - height,
            available.width,
            height,
        );
        let popup = Style::default().fg(Color::White).bg(Color::Rgb(24, 26, 28));
        let selected = popup.bg(Color::Rgb(39, 42, 45));
        let block = Block::default()
            .title("Commands")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(180, 180, 180)))
            .style(popup);
        let inner = block.inner(area);
        let selected_row = self.selected % grid.rows;
        let scroll = selected_row.saturating_sub(inner.height.saturating_sub(1) as usize);
        let lines: Vec<Line> = (scroll..grid.rows)
            .map(|row| {
                let mut spans = vec![Span::raw(" ")];
                for column in 0..grid.columns {
                    let index = column * grid.rows + row;
                    if let Some(command) = entries.get(index) {
                        if column > 0 {
                            spans.push(Span::raw("  "));
                        }
                        let label = format!(":{}", command.name());
                        spans.push(Span::styled(
                            format!("{label:<width$}", width = grid.width),
                            if index == self.selected {
                                selected
                            } else {
                                popup
                            },
                        ));
                    }
                }
                Line::from(spans)
            })
            .collect();
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    #[test]
    fn commands_match_names_with_arguments() {
        assert_eq!(
            matching_commands(" NEW name "),
            vec![Command::NewDirectory, Command::NewFile]
        );
        assert_eq!(
            matching_commands("s"),
            vec![Command::Search, Command::Shell]
        );
        assert_eq!(
            matching_commands("sh printf 'hello world'"),
            vec![Command::Shell]
        );
        assert_eq!(matching_commands("Q"), vec![Command::Quit]);
        for input in ["unknown", "界"] {
            assert!(matching_commands(input).is_empty());
        }
        let mut suggestions = Suggestions::default();
        assert_eq!(
            suggestions.resolve(" SE hello world "),
            Some((Command::Search, "hello world"))
        );
        assert_eq!(suggestions.resolve("q"), Some((Command::Quit, "")));
        assert!(suggestions.resolve(" ").is_none());
        assert!(suggestions.resolve("quit unexpected").is_none());
    }

    #[test]
    fn grid_fits_above_input() -> Result<()> {
        for (width, rows) in [(100, 1), (30, 4), (20, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, 12))?;
            terminal.draw(|frame| {
                frame.render_widget(Paragraph::new(":"), Rect::new(0, 11, width, 1));
                Suggestions::default().render(frame, "", Rect::new(0, 1, width, 10));
            })?;
            let buffer = terminal.backend().buffer();
            let top = 11 - rows - 2;
            assert_eq!(buffer[(0, top)].symbol(), "┌");
            assert_eq!(buffer[(width - 1, top)].symbol(), "┐");
            assert_eq!(buffer[(0, 10)].symbol(), "└");
            assert_eq!(buffer[(width - 1, 10)].symbol(), "┘");
            assert_eq!(buffer[(0, 11)].symbol(), ":");
            assert_eq!(buffer[(2, top + 1)].fg, Color::White);
            assert_eq!(buffer[(10, top + 1)].bg, Color::Rgb(39, 42, 45));
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            for command in COMMANDS {
                assert!(text.contains(&format!(":{}", command.name())));
            }
        }
        for (width, height) in [(0, 0), (1, 1), (2, 2), (10, 3)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| Suggestions::default().render(frame, "", frame.area()))?;
        }
        Ok(())
    }

    #[test]
    fn navigation_follows_grid_columns() {
        let mut suggestions = Suggestions::default();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        suggestions.navigate("n", key(KeyCode::Tab), 30);
        assert_eq!(suggestions.resolve("n"), Some((Command::NewFile, "")));
        suggestions.navigate("n", key(KeyCode::Tab), 30);
        assert_eq!(suggestions.resolve("n"), Some((Command::NewDirectory, "")));
        suggestions.navigate("n", key(KeyCode::BackTab), 30);
        assert_eq!(suggestions.resolve("n"), Some((Command::NewFile, "")));
        suggestions.navigate("", key(KeyCode::Right), 30);
        assert_eq!(suggestions.selected, 4);
        suggestions.navigate("", key(KeyCode::Right), 30);
        assert_eq!(suggestions.selected, 4);
        suggestions.navigate("", key(KeyCode::Left), 30);
        assert_eq!(suggestions.selected, 0);
        suggestions.navigate("", key(KeyCode::Up), 30);
        assert_eq!(suggestions.selected, 7);
        suggestions.navigate("", key(KeyCode::Down), 30);
        assert_eq!(suggestions.selected, 0);
        suggestions.navigate("unknown", key(KeyCode::BackTab), 30);
        assert_eq!(suggestions.selected, 0);
    }

    #[test]
    fn selection_stays_visible() -> Result<()> {
        let mut suggestions = Suggestions::default();
        suggestions.navigate("", KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE), 20);
        let mut terminal = Terminal::new(TestBackend::new(20, 3))?;
        terminal.draw(|frame| suggestions.render(frame, "", frame.area()))?;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), ":");
        assert_eq!(buffer[(2, 1)].bg, Color::Rgb(39, 42, 45));
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains(":sh"));
        Ok(())
    }
}
