use std::mem::take;

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

pub const NEW_DIR_CMD: &str = "new-dir ";
pub const NEW_FILE_CMD: &str = "new-file ";
pub const SEARCH_CMD: &str = "search ";
pub const SHELL_CMD: &str = "sh ";
pub const PATH_CMD: &str = "path";
pub const MOVE_CMD: &str = "mv";
pub const COPY_CMD: &str = "cp";

const COMMANDS: &[&str] = &[
    COPY_CMD,
    MOVE_CMD,
    NEW_DIR_CMD,
    NEW_FILE_CMD,
    PATH_CMD,
    SEARCH_CMD,
    SHELL_CMD,
];

fn suggestion_lines(input: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut line = Line::default();
    for command in COMMANDS
        .iter()
        .map(|command| command.trim_end())
        .filter(|command| command.starts_with(input))
    {
        let suggestion = format!(":{command}");
        if !line.spans.is_empty() && line.width() + 2 + suggestion.len() > width {
            lines.push(take(&mut line));
        }
        if !line.spans.is_empty() {
            line.push_span("  ");
        }
        line.push_span(suggestion);
    }
    if !line.spans.is_empty() {
        lines.push(line);
    }
    lines
}

pub fn render(frame: &mut Frame<'_>, input: &str, available: Rect) {
    if available.width < 3 || available.height < 3 {
        return;
    }
    let lines = suggestion_lines(input, available.width.saturating_sub(2) as usize);
    if lines.is_empty() {
        return;
    }
    let height = (lines.len() as u16 + 2).min(available.height);
    let area = Rect::new(
        available.x,
        available.bottom() - height,
        available.width,
        height,
    );
    let block = Block::default()
        .title("Commands")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(180, 180, 180)))
        .style(Style::default().fg(Color::White).bg(Color::Rgb(24, 26, 28)));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(Color::Cyan)),
        inner,
    );
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    #[test]
    fn suggestions_filter_command_names() {
        for (input, expected) in [
            ("new", ":new-dir  :new-file"),
            ("s", ":search  :sh"),
            ("cp", ":cp"),
        ] {
            let lines: Vec<String> = suggestion_lines(input, 80)
                .iter()
                .map(ToString::to_string)
                .collect();
            assert_eq!(lines, vec![expected]);
        }
        for input in ["unknown", "new-file test", "search ", "界"] {
            assert!(suggestion_lines(input, 80).is_empty());
        }
    }

    #[test]
    fn suggestions_wrap_above_input() -> Result<()> {
        for (width, rows) in [(80, 1), (20, 3)] {
            let mut terminal = Terminal::new(TestBackend::new(width, 12))?;
            terminal.draw(|frame| {
                frame.render_widget(Paragraph::new(":"), Rect::new(0, 11, width, 1));
                render(frame, "", Rect::new(0, 1, width, 10));
            })?;
            let buffer = terminal.backend().buffer();
            let top = 11 - rows - 2;
            assert_eq!(buffer[(0, top)].symbol(), "┌");
            assert_eq!(buffer[(width - 1, top)].symbol(), "┐");
            assert_eq!(buffer[(0, 10)].symbol(), "└");
            assert_eq!(buffer[(width - 1, 10)].symbol(), "┘");
            assert_eq!(buffer[(0, 11)].symbol(), ":");
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            for command in COMMANDS {
                assert!(text.contains(&format!(":{}", command.trim_end())));
            }
        }
        for (width, height) in [(0, 0), (1, 1), (2, 2), (10, 3)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| render(frame, "", frame.area()))?;
        }
        Ok(())
    }
}
