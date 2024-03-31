#![allow(unused)]
use core::time;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{BufReader, Read, Stdout, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use std::{env, fs, io, os, path, process, thread, vec};

use anyhow::{anyhow, Error, Result};
use application::Application;
use clap::{arg, Arg, ArgAction, Command};
use config::Config;
use crossterm::cursor::{position, Hide};
use crossterm::event::MouseButton::{Left, Middle, Right};
use crossterm::event::{
    poll,
    read,
    DisableMouseCapture,
    EnableMouseCapture,
    Event,
    KeyCode,
    KeyEvent,
    KeyModifiers,
    ModifierKeyCode,
    MouseEvent,
    MouseEventKind,
};
use crossterm::style::{self, ResetColor, Stylize};
use crossterm::terminal::{
    disable_raw_mode,
    enable_raw_mode,
    Clear,
    ClearType,
    EnterAlternateScreen,
    LeaveAlternateScreen,
    ScrollDown,
    ScrollUp,
};
use crossterm::tty::IsTty;
use crossterm::{cursor, execute, queue, terminal, ExecutableCommand, QueueableCommand};
use files::File;
use fs4::FileExt;
use log::{error, info, warn};
use tokio::task;
use tui::backend::{Backend, CrosstermBackend};
use tui::layout::{Alignment, Constraint, Direction, Layout};
use tui::style::{Color, Modifier, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use tui::{Frame, Terminal};

mod application;
mod config;
pub(crate) mod files;

#[cfg(test)]
mod tests;

pub(crate) mod proto {
    include!("../proto/server.rs");
}

type CrossTerminal = Terminal<CrosstermBackend<io::Stdout>>;

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line flags and arguments.
    let cmd = Command::new("File manager")
        .version("1.0")
        .author("dcompoze")
        .disable_help_flag(false)
        .about("Tree-based terminal file manager")
        .arg(
            Arg::new("last-dir-path")
                .long("last-dir-path")
                .value_name("PATH")
                .help("File containing last dir location")
                .action(ArgAction::Set)
                .required(false),
        )
        .arg(
            Arg::new("file-chooser-dir")
                .long("file-chooser-dir")
                .value_name("PATH")
                .help("File chooser dir mode")
                .action(ArgAction::Set)
                .required(false),
        )
        .arg(
            Arg::new("file-chooser-single")
                .long("file-chooser-single")
                .value_name("PATH")
                .help("File chooser single file mode")
                .action(ArgAction::Set)
                .required(false),
        )
        .arg(
            Arg::new("file-chooser-multiple")
                .long("file-chooser-multiple")
                .value_name("PATH")
                .help("File chooser multiple file mode")
                .action(ArgAction::Set)
                .required(false),
        )
        .arg(
            Arg::new("override-config")
                .long("override-config")
                .value_name("KEY_VALUE")
                .help("Override a configuration value e.g. show_hidden=false")
                .action(ArgAction::Set)
                .required(false),
        )
        .arg(Arg::new("dir").help("Directory to open").required(false).index(1))
        .get_matches();

    // Get useful system information.
    let user = whoami::username();
    let home_dir = dirs::home_dir().unwrap_or(format!("/home/{}", user).into());
    let config_dir = dirs::config_dir().unwrap_or(home_dir.join(".config"));
    let data_dir = dirs::data_dir().unwrap_or(home_dir.join(".local/share"));
    let fm_config_dir = config_dir.join("fm");
    let fm_data_dir = data_dir.join("fm");
    let fm_config_file = fm_config_dir.join("config.toml");
    let fm_log_file = fm_data_dir.join("log");

    // Create program directories if they don't already exist.
    fs::create_dir_all(fm_config_dir)?;
    fs::create_dir_all(fm_data_dir)?;

    // Create a default configuration file if necessary.
    if !fm_config_file.exists() {
        fs::write(fm_config_file.clone(), config::DEFAULT_CONFIG)?;
    }

    // Set up the file logger.
    let log_file = Box::new(
        OpenOptions::new()
            .write(true)
            .read(true)
            .create(true)
            .open(fm_log_file)?,
    );
    env_logger::builder()
        .format(|buf, record| writeln!(buf, "{}: {}", record.level(), record.args()))
        .target(env_logger::Target::Pipe(log_file))
        .init();

    // Set current directory to the specified path.
    if let Some(dir) = cmd.get_one::<String>("dir") {
        env::set_current_dir(dir)?;
    }

    // Get stdin and stdout handles and construct a Terminal object.
    let mut backend = CrosstermBackend::new(io::stdout());
    enable_raw_mode()?;
    execute!(
        backend,
        Clear(ClearType::All),
        cursor::MoveTo(0, 0),
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )?;
    let mut terminal = Terminal::new(backend)?;

    // Get current location and load the configuration.
    let current_dir = env::current_dir()?;
    let mut configuration = config::read_config(fm_config_file)?;
    // Override configuration if specified.
    if let Some(key_vals) = cmd.get_many::<String>("override-config") {
        key_vals.for_each(|key_val| {
            if let Some((key, value)) = key_val.split_once('=') {
                configuration.set_key(key, value);
            }
        });
    }
    // Construct directory tree from current location.
    let root = Application::read_dir(current_dir, configuration.show_hidden)?;

    let (sender, receiver): (Sender<()>, Receiver<()>) = mpsc::channel();
    let mut app = Application::new(&mut terminal, configuration, root, sender);
    app.set_title()?;

    let git_status = Arc::clone(&app.status.git_status);
    let commit_count = Arc::clone(&app.status.commit_count);
    let code_lines = Arc::clone(&app.status.code_lines);
    let git_modules = Arc::clone(&app.status.git_modules);

    // Status information background task.
    task::spawn_blocking(move || loop {
        if let Ok(()) = receiver.recv() {
            let output = Application::status_git_status_call();
            if let Ok(mut git_status) = git_status.lock() {
                *git_status = output.clone();
            }
            if !output.is_empty() {
                let output = Application::status_commit_count_call();
                if let Ok(mut commit_count) = commit_count.lock() {
                    *commit_count = output;
                }
                let output = Application::status_code_lines_call();
                if let Ok(mut code_lines) = code_lines.lock() {
                    *code_lines = output;
                }
                let modules = Application::git_modules_call();
                if let Ok(mut git_modules) = git_modules.lock() {
                    *git_modules = modules;
                }
            } else {
                if let Ok(mut commit_count) = commit_count.lock() {
                    *commit_count = String::default();
                }
                if let Ok(mut code_lines) = code_lines.lock() {
                    *code_lines = String::default();
                }
                if let Ok(mut git_modules) = git_modules.lock() {
                    *git_modules = HashSet::new();
                }
            }
        }
    });

    app.updater.send(())?;

    // Process all input and window events.
    loop {
        app.draw()?;

        let event = read()?;

        if app.command_bar.command_entry_mode {
            if let Event::Key(key) = event {
                match key.code {
                    KeyCode::Esc => {
                        app.command_bar.input_text = String::default();
                        app.command_bar.prompt_text = ":".into();
                        app.command_bar.command_entry_mode = false;
                    }
                    KeyCode::Backspace => {
                        if app.command_bar.input_text == String::default() {
                            app.command_bar.prompt_text = ":".into();
                            app.command_bar.command_entry_mode = false;
                        } else {
                            app.command_bar.input_text.pop();
                        }
                    }
                    KeyCode::Enter => {
                        if app.command_bar.prompt_text == "new-dir:" {
                            app.new_dir(app.command_bar.input_text.clone());
                        } else if app.command_bar.prompt_text == "new-file:" {
                            app.new_file(app.command_bar.input_text.clone());
                        } else if app.command_bar.prompt_text == "search:" {
                            app.search(app.command_bar.input_text.clone());
                        } else {
                            match app.command_bar.input_text.as_str() {
                                // Commands that are useful to have but are not bound to a keybinding.
                                "path" => app.cmd_path(),
                                "mv" => app.cmd_mv(),
                                "cp" => app.cmd_cp(),
                                _ => {}
                            }
                        }
                        app.command_bar.input_text = String::default();
                        app.command_bar.prompt_text = ":".into();
                        app.command_bar.command_entry_mode = false;
                    }
                    KeyCode::Char(c) => {
                        app.command_bar.input_text.push(c);
                    }
                    _ => {}
                }
            }
        } else {
            match event {
                Event::Resize(_, _) => {
                    let (original_size, new_size) = flush_resize_events(event.clone());
                }
                Event::FocusGained => {}
                Event::FocusLost => {}
                Event::Paste(content) => {}
                Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers,
                }) => match kind {
                    MouseEventKind::Down(Left) => {
                        let height = app.terminal.get_frame().size().height;
                        let file_count = app.files.count();
                        if row > 0 && (row as u32) < file_count && row < height {
                            let offset = app.list_state.offset();
                            let clicked = (row - 1) as usize + offset;
                            app.list_state.select(Some(clicked));
                        }
                    }
                    MouseEventKind::Up(Left) => {}
                    MouseEventKind::Down(Right) => {
                        let height = app.terminal.get_frame().size().height;
                        let file_count = app.files.count();
                        if row > 0 && (row as u32) < file_count && row < height && file_count > 1 {
                            let offset = app.list_state.offset();
                            let clicked = (row - 1) as usize + offset;
                            app.list_state.select(Some(clicked));
                            if let Some(selected) = app.selected() {
                                if selected.metadata.is_dir() {
                                    if app.expanded.contains(&selected.path) {
                                        app.collapse()
                                    } else {
                                        app.expand();
                                    }
                                } else {
                                    app.open();
                                }
                            }
                        }
                    }
                    MouseEventKind::Up(Right) => {}
                    MouseEventKind::Down(Middle) => {
                        let height = app.terminal.get_frame().size().height;
                        let file_count = app.files.count();
                        if row == 0 {
                            app.previous_root()?;
                        } else if (row as u32) < file_count && row < height {
                            let offset = app.list_state.offset();
                            let clicked = (row - 1) as usize + offset;
                            app.list_state.select(Some(clicked));
                            app.change_root();
                        }
                    }
                    MouseEventKind::Up(Middle) => {}
                    MouseEventKind::Drag(button) => {}
                    MouseEventKind::Moved => {}
                    MouseEventKind::ScrollDown => {
                        app.down();
                    }
                    MouseEventKind::ScrollUp => {
                        app.up();
                    }
                    MouseEventKind::ScrollLeft => {}
                    MouseEventKind::ScrollRight => {}
                },
                Event::Key(KeyEvent { code, modifiers, .. }) => match (code, modifiers) {
                    (KeyCode::Char(':'), KeyModifiers::NONE) => {
                        app.command_bar.command_entry_mode = true;
                    }
                    (KeyCode::Esc, KeyModifiers::NONE) => {
                        app.clear();
                    }
                    (KeyCode::Char(';'), KeyModifiers::NONE) => {
                        app.change_root()?;
                    }
                    (KeyCode::Char('j'), KeyModifiers::NONE) => {
                        app.previous_root()?;
                    }
                    (KeyCode::Char('q'), KeyModifiers::NONE) => {
                        app.quit()?;
                    }
                    (KeyCode::Char('Q'), KeyModifiers::SHIFT) => {
                        app.quit_change(cmd.get_one::<String>("last-dir-path"))?;
                    }
                    (KeyCode::Char('h'), KeyModifiers::NONE) => {
                        if let Some(output_path) = cmd.get_one::<String>("file-chooser-dir") {
                            app.quit_print_dir(output_path.clone())?;
                        } else if let Some(output_path) = cmd.get_one::<String>("file-chooser-single") {
                            app.quit_print_file(output_path.clone())?;
                        } else if let Some(output_path) = cmd.get_one::<String>("file-chooser-multiple") {
                            app.quit_print_marked(output_path.clone())?;
                        }
                    }
                    (KeyCode::Down, KeyModifiers::NONE) => {
                        app.down();
                    }
                    (KeyCode::Char('k'), KeyModifiers::NONE) => {
                        app.down();
                    }
                    (KeyCode::Up, KeyModifiers::NONE) => {
                        app.up();
                    }
                    (KeyCode::Char('l'), KeyModifiers::NONE) => {
                        app.up();
                    }
                    (KeyCode::Char('x'), KeyModifiers::NONE) => {
                        app.expand_toggle();
                    }
                    (KeyCode::Left, KeyModifiers::NONE) => {
                        app.collapse();
                    }
                    (KeyCode::Right, KeyModifiers::NONE) => {
                        app.expand();
                    }
                    (KeyCode::Char(' '), KeyModifiers::NONE) => {
                        app.mark();
                    }
                    (KeyCode::Char('F'), KeyModifiers::SHIFT) => {
                        app.file_manager();
                    }
                    (KeyCode::Char('E'), KeyModifiers::SHIFT) => {
                        app.editx();
                    }
                    (KeyCode::Char('e'), KeyModifiers::NONE) => {
                        app.edit();
                    }
                    (KeyCode::Char('S'), KeyModifiers::SHIFT) => {
                        app.shellx();
                    }
                    (KeyCode::Char('s'), KeyModifiers::NONE) => {
                        app.shell();
                    }
                    (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                        app.shellx_root();
                    }
                    (KeyCode::Char('i'), KeyModifiers::NONE) => {
                        app.preview();
                    }
                    (KeyCode::Char('o'), KeyModifiers::NONE) => {
                        app.open();
                    }
                    (KeyCode::Char('r'), KeyModifiers::NONE) => {
                        app.rename();
                    }
                    (KeyCode::Char('V'), KeyModifiers::SHIFT) => {
                        app.vscode();
                    }
                    (KeyCode::Char('T'), KeyModifiers::SHIFT) => {
                        app.trash();
                    }
                    (KeyCode::Char('I'), KeyModifiers::SHIFT) => {
                        app.images();
                    }
                    (KeyCode::Char('/'), KeyModifiers::NONE) => {
                        app.command_bar.prompt_text = "search:".into();
                        app.command_bar.command_entry_mode = true;
                    }
                    (KeyCode::Char('?'), KeyModifiers::NONE) => {
                        app.search_all();
                    }
                    (KeyCode::Char('D'), KeyModifiers::SHIFT) => {
                        app.drag_and_drop();
                    }
                    (KeyCode::Char('L'), KeyModifiers::SHIFT) => {
                        app.git_log();
                    }
                    (KeyCode::Char('N'), KeyModifiers::SHIFT) => {
                        app.command_bar.prompt_text = "new-dir:".into();
                        app.command_bar.command_entry_mode = true;
                    }
                    (KeyCode::Char('n'), KeyModifiers::NONE) => {
                        app.command_bar.prompt_text = "new-file:".into();
                        app.command_bar.command_entry_mode = true;
                    }
                    (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                        app.refresh();
                    }
                    (KeyCode::Char('y'), KeyModifiers::NONE) => {
                        app.copy();
                    }
                    (KeyCode::Char('c'), KeyModifiers::NONE) => {
                        app.cut();
                    }
                    (KeyCode::Char('Z'), KeyModifiers::SHIFT) => {
                        app.toggle_hidden();
                    }
                    (KeyCode::Char('C'), KeyModifiers::SHIFT) => {
                        app.clear_files();
                    }
                    (KeyCode::Char('p'), KeyModifiers::NONE) => {
                        app.paste();
                    }
                    (KeyCode::Char('g'), KeyModifiers::NONE) => match read()? {
                        Event::Key(KeyEvent {
                            code: KeyCode::Char('g'),
                            ..
                        }) => {
                            app.top();
                        }
                        Event::Key(KeyEvent {
                            code: KeyCode::Char('e'),
                            ..
                        }) => {
                            app.bottom();
                        }
                        _ => {}
                    },
                    _ => {
                        dbgf!(format!("Unknown event: {:?} {:?}", code, modifiers));
                    }
                },
                _ => {}
            }
        }
    }
    Ok(())
}

// Resize events can occur in batches.
// With a simple loop they can be flushed.
// This function will keep the first and last resize event.
fn flush_resize_events(event: Event) -> ((u16, u16), (u16, u16)) {
    if let Event::Resize(x, y) = event {
        let mut last_resize = (x, y);
        while let Ok(true) = poll(Duration::from_millis(50)) {
            if let Ok(Event::Resize(x, y)) = read() {
                last_resize = (x, y);
            }
        }
        return ((x, y), last_resize);
    }
    ((0, 0), (0, 0))
}

#[macro_export]
macro_rules! dbgf {
    ($arg:expr) => {{
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .write(true)
            .open("/tmp/debuglog")
            .unwrap();
        writeln!(&mut file, "{:?}", $arg).unwrap();
    }};
}
