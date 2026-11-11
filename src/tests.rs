use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Result;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::Rect;
use ratatui::widgets::List;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::application::Application;
use crate::config::{Config, DEFAULT_CONFIG};
use crate::files::File;

fn with_application(test: impl FnOnce(&mut Application<'_>) -> Result<()>) -> Result<()> {
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )?;
    let metadata = fs::metadata("Cargo.toml")?;
    let root = File {
        path: PathBuf::from("."),
        metadata: fs::metadata(".")?,
        metadata_extra: fs::symlink_metadata(".")?,
        descendants: (0..40)
            .map(|index| File {
                path: PathBuf::from(format!("file-{index:02}-界-e\u{301}.rs")),
                metadata: metadata.clone(),
                metadata_extra: metadata.clone(),
                descendants: Vec::new(),
            })
            .collect(),
    };
    let config: Config = toml::from_str(DEFAULT_CONFIG)?;
    let (sender, _receiver) = mpsc::channel();
    test(&mut Application::new(&mut terminal, config, root, sender))
}

#[test]
fn unicode_rows_preserve_scroll_offset() -> Result<()> {
    with_application(|app| {
        let mut terminal = Terminal::new(TestBackend::new(60, 5))?;
        app.bottom();
        let items = app.item_list(0, 60, &HashSet::new(), &app.configuration)?;
        terminal.draw(|frame| {
            frame.render_stateful_widget(List::new(items), frame.area(), &mut app.list_state);
        })?;
        assert_eq!(app.list_state.selected(), Some(39));
        assert_eq!(app.list_state.offset(), 35);
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("file-35-界"));
        assert!(text.contains("file-39-界"));
        assert_ne!(buffer[(59, 0)].symbol(), " ");
        assert_ne!(buffer[(59, 4)].symbol(), " ");
        app.list_state.select(Some(app.list_state.offset() + 2));
        assert_eq!(
            app.selected().map(|file| file.path),
            Some(PathBuf::from("file-37-界-e\u{301}.rs"))
        );
        Ok(())
    })
}

#[test]
fn status_fits_narrow_terminals() -> Result<()> {
    with_application(|app| {
        *app.status
            .git_status
            .lock()
            .map_err(|error| anyhow::anyhow!(error.to_string()))? = "main".repeat(20);
        for width in [0, 1, 10, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width, 1))?;
            let status = app.statusbar(width as usize, false, String::new())?;
            terminal.draw(|frame| frame.render_widget(status, frame.area()))?;
        }
        Ok(())
    })
}
