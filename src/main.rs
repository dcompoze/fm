#![allow(unused)]
use core::time;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{BufReader, Cursor, Read, Stdout, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
use std::{env, fs, io, os, path, process, thread, vec};

use anyhow::{anyhow, Error, Result};
use application::Application;
use clap::{arg, Arg, ArgAction, Command};
use commands::{COPY_CMD, MOVE_CMD, NEW_DIR_CMD, NEW_FILE_CMD, PATH_CMD, SEARCH_CMD, SHELL_CMD};
use config::Config;
use crossterm::cursor::{position, Hide};
use crossterm::event::MouseButton::{Left, Middle, Right};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, ModifierKeyCode, MouseEvent, MouseEventKind,
};
use crossterm::style::{self, ResetColor, Stylize};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen, ScrollDown, ScrollUp,
};
use crossterm::tty::IsTty;
use crossterm::{cursor, execute, queue, terminal, ExecutableCommand, QueueableCommand};
use files::File;
use fm::clipboard::get_socket_path;
pub(crate) use fm::proto;
use futures::StreamExt;
use log::{error, info, warn};
use prost::Message;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream as TokioUnixStream;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task;

mod application;
mod commands;
mod config;
pub(crate) mod error;
pub(crate) mod files;
mod help;

#[cfg(test)]
mod tests;

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
        .arg(
            Arg::new("dir")
                .help("Directory to open")
                .required(false)
                .index(1),
        )
        .get_matches();

    // Get useful system information.
    let user = whoami::username()?;
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
            .truncate(false)
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
    sleep(Duration::from_millis(50)); // Fixes bad size on terminal open.
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
    if let Err(error) = app.synchronize() {
        error!(
            "Clipboard server is unavailable, starting in local mode: {}",
            error
        );
    }
    let (clipboard_notification_sender, mut clipboard_notifications) = tokio_mpsc::channel(1);
    task::spawn(clipboard_notification_listener(
        clipboard_notification_sender,
        app.clipboard_client_id().to_owned(),
    ));
    let mut event_reader = EventStream::new();

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
        app.reconnect_clipboard_if_due();
        app.draw()?;

        let reconnect_wait = app.clipboard_reconnect_wait();
        let wait = reconnect_wait.unwrap_or(Duration::from_secs(24 * 60 * 60));
        let event = tokio::select! {
            event = event_reader.next() => {
                match event {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => return Err(error.into()),
                    None => break,
                }
            }
            notification = clipboard_notifications.recv() => {
                if notification.is_none() {
                    return Err(anyhow!("clipboard notification channel closed"));
                }
                if let Err(error) = app.synchronize() {
                    error!("Could not synchronize clipboard after server notification: {}", error);
                }
                continue;
            }
            _ = tokio::time::sleep(wait), if reconnect_wait.is_some() => {
                continue;
            }
        };

        if app.help.visible {
            app.help.handle_event(event);
        } else if app.command_bar.command_entry_mode {
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
                        if app.command_bar.input_text.starts_with(NEW_DIR_CMD) {
                            app.new_dir(app.command_bar.command_parameters(NEW_DIR_CMD));
                        } else if app.command_bar.input_text.starts_with(NEW_FILE_CMD) {
                            app.new_file(app.command_bar.command_parameters(NEW_FILE_CMD));
                        } else if app.command_bar.input_text.starts_with(SEARCH_CMD) {
                            app.search(app.command_bar.command_parameters(SEARCH_CMD));
                        } else if app.command_bar.input_text.starts_with(SHELL_CMD) {
                            app.cmd_shell(app.command_bar.command_parameters(SHELL_CMD));
                        } else if app.command_bar.input_text == PATH_CMD {
                            app.cmd_path();
                        } else if app.command_bar.input_text == MOVE_CMD {
                            app.cmd_mv();
                        } else if app.command_bar.input_text == COPY_CMD {
                            app.cmd_cp();
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
                Event::Resize(_, _) => {}
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
                        let height = app.terminal.get_frame().area().height;
                        let file_count = app.files.count();
                        if row > 0 && (row as u32) < file_count && row < height {
                            let offset = app.list_state.offset();
                            let clicked = (row - 1) as usize + offset;
                            app.list_state.select(Some(clicked));
                        }
                    }
                    MouseEventKind::Up(Left) => {}
                    MouseEventKind::Down(Right) => {
                        let height = app.terminal.get_frame().area().height;
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
                        let height = app.terminal.get_frame().area().height;
                        let file_count = app.files.count();
                        if row == 0 {
                            app.previous_root()?;
                        } else if (row as u32) < file_count && row < height {
                            let offset = app.list_state.offset();
                            let clicked = (row - 1) as usize + offset;
                            app.list_state.select(Some(clicked));
                            app.change_root().expect("change root failed");
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
                Event::Key(key) if key.kind == KeyEventKind::Release => {}
                Event::Key(KeyEvent {
                    code, modifiers, ..
                }) => match (code, modifiers) {
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
                        } else if let Some(output_path) =
                            cmd.get_one::<String>("file-chooser-single")
                        {
                            app.quit_print_file(output_path.clone())?;
                        } else if let Some(output_path) =
                            cmd.get_one::<String>("file-chooser-multiple")
                        {
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
                    (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                        app.editnox();
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
                    (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                        app.vscode_root();
                    }
                    (KeyCode::Char('T'), KeyModifiers::SHIFT) => {
                        app.trash();
                    }
                    (KeyCode::Char('I'), KeyModifiers::SHIFT) => {
                        app.images();
                    }
                    (KeyCode::Char('/'), KeyModifiers::NONE) => {
                        app.command_bar.input_text = SEARCH_CMD.into();
                        app.command_bar.command_entry_mode = true;
                    }
                    (KeyCode::Char('?'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                        app.help.open();
                    }
                    (KeyCode::Char('f'), KeyModifiers::NONE) => {
                        app.search_all()?;
                    }
                    (KeyCode::Char('D'), KeyModifiers::SHIFT) => {
                        app.drag_and_drop();
                    }
                    (KeyCode::Char('L'), KeyModifiers::SHIFT) => {
                        app.git_log();
                    }
                    (KeyCode::Char('N'), KeyModifiers::SHIFT) => {
                        app.command_bar.input_text = NEW_DIR_CMD.into();
                        app.command_bar.command_entry_mode = true;
                    }
                    (KeyCode::Char('n'), KeyModifiers::NONE) => {
                        app.command_bar.input_text = NEW_FILE_CMD.into();
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
                        app.paste(false);
                    }
                    (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                        app.paste(true);
                    }
                    (KeyCode::Char('g'), KeyModifiers::NONE) => {
                        let next_event = tokio::select! {
                            event = event_reader.next() => event.transpose()?,
                            notification = clipboard_notifications.recv() => {
                                if notification.is_none() {
                                    return Err(anyhow!("clipboard notification channel closed"));
                                }
                                if let Err(error) = app.synchronize() {
                                    error!(
                                        "Could not synchronize clipboard after server notification: {}",
                                        error
                                    );
                                }
                                None
                            }
                        };
                        match next_event {
                            Some(Event::Key(KeyEvent {
                                code: KeyCode::Char('g'),
                                ..
                            })) => {
                                app.top();
                            }
                            Some(Event::Key(KeyEvent {
                                code: KeyCode::Char('e'),
                                ..
                            })) => {
                                app.bottom();
                            }
                            _ => {}
                        }
                    }
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

async fn clipboard_notification_listener(sender: tokio_mpsc::Sender<()>, client_id: String) {
    const INITIAL_DELAY: Duration = Duration::from_secs(1);
    const MAX_DELAY: Duration = Duration::from_secs(5 * 60);

    let mut retry_delay = INITIAL_DELAY;
    loop {
        let socket_path = get_socket_path();
        match TokioUnixStream::connect(&socket_path).await {
            Ok(mut stream) => {
                retry_delay = INITIAL_DELAY;
                if let Err(error) =
                    consume_clipboard_notifications(&mut stream, &sender, &client_id).await
                {
                    warn!("Clipboard notification subscription ended: {}", error);
                }
            }
            Err(error) => {
                warn!(
                    "Could not subscribe to clipboard notifications at '{}': {}",
                    socket_path.display(),
                    error
                );
            }
        }

        tokio::time::sleep(retry_delay).await;
        retry_delay = retry_delay
            .checked_mul(2)
            .unwrap_or(MAX_DELAY)
            .min(MAX_DELAY);
    }
}

async fn consume_clipboard_notifications(
    stream: &mut TokioUnixStream,
    sender: &tokio_mpsc::Sender<()>,
    client_id: &str,
) -> Result<()> {
    let request = proto::Request {
        command: proto::Command::Subscribe.into(),
        files: vec![],
        clipboard_items: vec![],
        clear_timestamp: None,
        client_id: client_id.to_owned(),
    };
    let mut request_buffer = Vec::with_capacity(request.encoded_len());
    request.encode(&mut request_buffer)?;
    stream.write_u32(request_buffer.len() as u32).await?;
    stream.write_all(&request_buffer).await?;

    loop {
        let response_length = stream.read_u32().await? as usize;
        let mut response_buffer = vec![0; response_length];
        stream.read_exact(&mut response_buffer).await?;
        let response = proto::Response::decode(&mut Cursor::new(response_buffer))?;
        if response.status != "success" {
            return Err(anyhow!(
                "clipboard subscription failed: {}",
                response.status
            ));
        }

        match sender.try_send(()) {
            Ok(()) | Err(tokio_mpsc::error::TrySendError::Full(())) => {}
            Err(tokio_mpsc::error::TrySendError::Closed(())) => {
                return Err(anyhow!("clipboard notification receiver closed"));
            }
        }
    }
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
