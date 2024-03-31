use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{BufReader, Cursor, ErrorKind, Read, Stdout, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::{env, fs, io, path, process, vec};

use anyhow::{anyhow, Error, Result};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use crossterm::cursor::{position, Hide};
use crossterm::event::MouseButton::{Left, Middle, Right};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::style::{self, ResetColor, Stylize};
use crossterm::terminal::{
    disable_raw_mode,
    enable_raw_mode,
    Clear,
    ClearType,
    EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{cursor, execute, ExecutableCommand};
use fs4::FileExt;
use log::{error, info, warn};
use prost::Message;
use tui::backend::CrosstermBackend;
use tui::layout::{Alignment, Constraint, Direction, Layout};
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use tui::Terminal;

use crate::{dbgf, files, proto, Config, File};

type CrossTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub struct Application<'a> {
    pub terminal: &'a mut CrossTerminal,
    pub files: File,
    pub copied: HashSet<PathBuf>,
    pub cut: HashSet<PathBuf>,
    pub marked: HashSet<PathBuf>,
    pub expanded: HashSet<PathBuf>,
    pub files_previous: PathBuf,
    pub list_state: ListState,
    pub configuration: Config,
    pub command_bar: CommandBar,
    pub status: Status,
    pub updater: Sender<()>,
}

pub struct Status {
    pub git_status: Arc<Mutex<String>>,
    pub commit_count: Arc<Mutex<String>>,
    pub code_lines: Arc<Mutex<String>>,
    pub git_modules: Arc<Mutex<HashSet<PathBuf>>>,
}

pub struct CommandBar {
    pub command_entry_mode: bool,
    pub prompt_text: String,
    pub input_text: String,
}

impl<'a> Application<'a> {
    pub fn new(terminal: &'a mut CrossTerminal, config: Config, root: File, sender: Sender<()>) -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Application {
            terminal,
            files: root,
            copied: HashSet::new(),
            cut: HashSet::new(),
            marked: HashSet::new(),
            expanded: HashSet::new(),
            files_previous: PathBuf::new(),
            list_state: state,
            configuration: config,
            command_bar: CommandBar {
                command_entry_mode: false,
                prompt_text: ":".into(),
                input_text: String::default(),
            },
            status: Status {
                git_status: Arc::new(Mutex::new(String::default())),
                commit_count: Arc::new(Mutex::new(String::default())),
                code_lines: Arc::new(Mutex::new(String::default())),
                git_modules: Arc::new(Mutex::new(HashSet::new())),
            },
            updater: sender,
        }
    }

    pub fn set_title(&mut self) -> Result<(), Error> {
        write!(
            self.terminal.backend_mut(),
            "\x1B]0;{}\x07",
            self.files
                .path
                .file_name()
                .unwrap_or(OsStr::new("/"))
                .to_string_lossy()
        )?;
        Ok(())
    }

    pub fn git_modules_call() -> HashSet<PathBuf> {
        if let Ok(output) = process::Command::new("fm-git-modules").output() {
            let output = String::from_utf8_lossy(&output.stdout).to_string();
            output.lines().map(PathBuf::from).collect()
        } else {
            HashSet::new()
        }
    }

    pub fn draw(&mut self) -> Result<(), Error> {
        self.terminal.autoresize()?; // TODO: Might be unnecessary since terminal.draw() already does this.
        let size = self.terminal.get_frame().size();
        let mut git_modules = HashSet::new();
        if let Ok(modules) = self.status.git_modules.lock() {
            git_modules = modules.clone();
        }
        let frame_width = self.terminal.get_frame().size().width as usize;
        let files: Vec<ListItem> = self.item_list(0, frame_width, &git_modules, &self.configuration)?;
        let pathbar = self.pathbar()?;
        let statusbar = self.statusbar(
            size.width as usize,
            self.command_bar.command_entry_mode,
            self.command_bar.input_text.clone(),
        )?;

        let _ = self.terminal.draw(|frame| {
            // Draw each visible file in the tree until we run out of space on the screen.
            let filelist = List::new(files)
                .style(Style::default().fg(Color::White))
                .highlight_style(Style::default().bg(Color::Rgb(39, 42, 45)))
                .highlight_symbol("");

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Length(1),
                        Constraint::Length(size.height.saturating_sub(2)),
                        Constraint::Length(1),
                    ]
                    .as_ref(),
                )
                .split(size);

            frame.render_widget(pathbar, chunks[0]);
            frame.render_stateful_widget(filelist, chunks[1], &mut self.list_state);
            frame.render_widget(statusbar, chunks[2]);
        })?;
        if self.command_bar.command_entry_mode {
            execute!(
                self.terminal.backend_mut(),
                cursor::MoveTo(
                    (self.command_bar.input_text.len() + self.command_bar.prompt_text.len()) as u16,
                    size.height
                ),
                cursor::Show
            )?;
        }
        Ok(())
    }

    pub fn item_list(
        &self,
        indent: usize,
        frame_width: usize,
        git_modules: &HashSet<PathBuf>,
        config: &Config,
    ) -> Result<Vec<ListItem<'a>>, Error> {
        let root = &self.files;
        self.item_file(indent, frame_width, git_modules, config, root)
    }

    fn item_file(
        &self,
        indent: usize,
        frame_width: usize,
        git_modules: &HashSet<PathBuf>,
        config: &Config,
        file: &File,
    ) -> Result<Vec<ListItem<'a>>> {
        let mut items: Vec<ListItem> = Vec::new();
        if !file.descendants.is_empty() {
            for descendant in &file.descendants {
                let mut mark_span = Span::styled(" ", Style::default());
                if self.cut.contains(&descendant.path) {
                    mark_span = Span::styled("●", Style::default().fg(Color::Red));
                } else if self.copied.contains(&descendant.path) {
                    mark_span = Span::styled("●", Style::default().fg(Color::Yellow));
                } else if self.marked.contains(&descendant.path) {
                    mark_span = Span::styled("●", Style::default().fg(Color::Magenta));
                }
                let guide_span = Span::styled("│", Style::default().fg(Color::Rgb(53, 57, 62)));
                let separator_span = Span::raw(" ");
                let mut indent_span = Spans::from(vec![]);
                //let mut indent_span = Spans::from(vec![guide_span.clone(), mark_span.clone(),
                // separator_span.clone()]);
                for _ in (0..indent) {
                    let final_span = indent_span.clone();
                    indent_span = Spans::from(vec![
                        guide_span.clone(),
                        separator_span.clone(),
                        separator_span.clone(),
                    ]);
                    indent_span.0.extend(final_span.0);
                }
                //let git_modified_span = git_modified(descendant.clone())?;
                let mut count_span = descendant.info_count()?;
                let item_name = descendant
                    .path
                    .file_name()
                    .ok_or(anyhow!("invalid path"))?
                    .to_string_lossy();
                let icon_width = 3;
                let item_pad_width = frame_width
                    - (icon_width
                        + indent_span.width()
                        + item_name.len()
                        + separator_span.width()
                        + mark_span.width()
                        + separator_span.width()
                        + count_span.width()); // TODO: What if frame has not enough space.
                let item_pad_span = Span::raw(format!("{:<item_pad_width$}", " "));
                let item_span: Span;
                if descendant.metadata_extra.is_symlink() {
                    count_span.style = Style::default().fg(Color::Cyan);
                    item_span = Span::styled(
                        format!("{}  {}", config.style.link.icon, item_name),
                        Style::default().fg(Color::Cyan),
                    );
                } else if descendant.is_video() {
                    count_span.style = Style::default().fg(Color::Magenta);
                    item_span = Span::styled(
                        format!("{}  {}", config.style.video.icon, item_name),
                        Style::default().fg(Color::Magenta),
                    );
                } else if descendant.is_audio() {
                    count_span.style = Style::default().fg(Color::Cyan);
                    item_span = Span::styled(
                        format!("{}  {}", config.style.audio.icon, item_name),
                        Style::default().fg(Color::Cyan),
                    );
                } else if descendant.is_image() {
                    count_span.style = Style::default().fg(Color::Magenta);
                    item_span = Span::styled(
                        format!("{}  {}", config.style.image.icon, item_name),
                        Style::default().fg(Color::Magenta),
                    );
                } else if descendant.is_archive() {
                    count_span.style = Style::default().fg(Color::Red);
                    item_span = Span::styled(
                        format!("{}  {}", config.style.archive.icon, item_name),
                        Style::default().fg(Color::Red),
                    );
                } else if descendant.is_document() {
                    count_span.style = Style::default().fg(Color::White);
                    item_span = Span::styled(
                        format!("{}  {}", config.style.document.icon, item_name),
                        Style::default().fg(Color::White),
                    );
                } else if descendant.metadata.is_dir() {
                    if git_modules.contains(&descendant.path) {
                        count_span.style = Style::default().fg(Color::Cyan);
                        item_span = Span::styled(
                            format!("{}  {}", config.style.directory.icon, item_name),
                            Style::default().fg(Color::Cyan),
                        );
                    } else {
                        count_span.style = Style::default().fg(Color::Blue);
                        item_span = Span::styled(
                            format!("{}  {}", config.style.directory.icon, item_name),
                            Style::default().fg(Color::Blue),
                        );
                    }
                } else if descendant.is_executable() {
                    count_span.style = Style::default().fg(Color::Green);
                    item_span = Span::styled(
                        format!("{}  {}", config.style.file.icon, item_name),
                        Style::default().fg(Color::Green),
                    );
                } else {
                    count_span.style = Style::default();
                    item_span = Span::raw(format!("{}  {}", config.style.file.icon, item_name));
                }
                let list_item = Spans::from(vec![
                    item_span,
                    separator_span.clone(),
                    mark_span,
                    item_pad_span,
                    separator_span.clone(),
                    count_span,
                ]);
                indent_span.0.extend(list_item.clone().0);
                items.push(ListItem::new(indent_span));
                if !descendant.descendants.is_empty() {
                    let mut descendant_items =
                        self.item_file(indent + 1, frame_width, git_modules, config, descendant)?;
                    items.append(&mut descendant_items);
                }
            }
        }
        Ok(items)
    }

    pub fn statusbar(
        &mut self,
        width: usize,
        commandbar: bool,
        input: String,
    ) -> Result<Paragraph<'a>, Error> {
        let statusbar: Paragraph;
        if commandbar {
            statusbar = Paragraph::new(vec![Spans::from(vec![
                Span::styled(self.command_bar.prompt_text.clone(), Style::default()),
                Span::styled(input, Style::default()),
            ])])
            .block(Block::default().borders(Borders::NONE))
            .style(Style::default().fg(Color::White).bg(Color::Rgb(39, 42, 45)))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true });
        } else {
            let mut git_status_span = Spans::from("");
            let mut module_count_span = Spans::from("");
            //let mut commit_count_span = Spans::from("");
            let mut code_lines_span = Spans::from("");
            if let Ok(output) = self.status.git_status.lock() {
                git_status_span = Application::status_git_status_span(output.clone());
            }
            //if let Ok(output) = self.commit_count.lock() {
            //    commit_count_span = Application::status_commit_count_span(output.clone());
            //}
            if let Ok(output) = self.status.code_lines.lock() {
                code_lines_span = Application::status_code_lines_span(output.clone());
            }
            if git_status_span.width() != 0 {
                let mut module_count = 0;
                if let Ok(modules) = self.status.git_modules.lock() {
                    module_count = modules.iter().count();
                }
                module_count_span = Spans::from(vec![
                    Span::styled(
                        format!("{}  ", self.configuration.style.directory.icon),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(format!("{}  ", module_count), Style::default().fg(Color::Cyan)),
                ]);
            }
            let link_target_span = self.status_link_target();
            let pad_width = width
                - (git_status_span.width()
                    + module_count_span.width()
                    //+ commit_count_span.width()
                    + code_lines_span.width()
                    + link_target_span.width());
            let mut status_span = Spans::from(vec![]);
            status_span.0.extend(git_status_span.0);
            status_span.0.extend(module_count_span.0);
            //status_span.0.extend(commit_count_span.0);
            status_span.0.extend(code_lines_span.0);
            status_span.0.extend(vec![
                link_target_span,
                Span::styled(
                    format!("{:>pad_width$}", self.status_position().content),
                    Style::default(),
                ),
            ]);
            statusbar = Paragraph::new(status_span)
                .block(Block::default().borders(Borders::NONE))
                .style(Style::default().fg(Color::White).bg(Color::Rgb(39, 42, 45)))
                .alignment(Alignment::Left)
                .wrap(Wrap { trim: false });
        }
        Ok(statusbar)
    }

    pub fn status_position(&self) -> Span<'a> {
        let total_count = self.files.count().saturating_sub(1);
        let icon = "  ";
        if let Some(selected) = self.list_state.selected() {
            let current_location = selected + 1;
            Span::styled(
                format!("{}{}/{}", icon, current_location, total_count),
                Style::default(),
            )
        } else {
            Span::styled(format!("{}{}/{}", icon, 0, total_count), Style::default())
        }
    }

    pub fn status_link_target(&self) -> Span<'a> {
        let icon = "  ";
        if let Some(selected) = self.selected() {
            if selected.metadata_extra.is_symlink() {
                let target = fs::read_link(selected.path).expect("could not read link");
                return Span::styled(
                    format!("{}{}  ", icon, target.to_string_lossy()),
                    Style::default(),
                );
            }
        }
        Span::styled("", Style::default())
    }

    pub fn status_code_lines_call() -> String {
        if let Ok(output) = process::Command::new("fm-code-lines").output() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::default()
        }
    }

    pub fn status_code_lines_span(output: String) -> Spans<'a> {
        if output.is_empty() {
            Spans::from("")
        } else {
            Spans::from(vec![Span::styled(format!("  {}  ", output), Style::default())])
        }
    }

    pub fn status_commit_count_call() -> String {
        if let Ok(output) = process::Command::new("fm-commit-count").output() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::default()
        }
    }

    pub fn status_commit_count_span(output: String) -> Spans<'a> {
        if output.is_empty() {
            Spans::from("")
        } else {
            Spans::from(vec![Span::styled(format!("  {}  ", output), Style::default())])
        }
    }

    pub fn status_git_status_call() -> String {
        if let Ok(output) = process::Command::new("fm-git-status").output() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::default()
        }
    }

    pub fn status_git_status_span(output: String) -> Spans<'a> {
        let status_lines: Vec<String> = output.lines().map(|s| s.to_owned()).collect();
        if status_lines.len() == 5 {
            Spans::from(vec![
                Span::styled("  ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{} ", status_lines[0].clone()),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(status_lines[1].clone() + " ", Style::default().fg(Color::Magenta)),
                Span::styled(status_lines[2].clone() + " ", Style::default().fg(Color::Green)),
                Span::styled(status_lines[3].clone() + " ", Style::default().fg(Color::Yellow)),
                Span::styled(status_lines[4].clone() + "  ", Style::default().fg(Color::Red)),
            ])
        } else if status_lines.len() == 2 {
            Spans::from(vec![
                Span::styled("  ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{} ", status_lines[0].clone()),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(status_lines[1].clone() + "  ", Style::default().fg(Color::Yellow)),
            ])
        } else if status_lines.len() == 1 {
            Spans::from(vec![
                Span::styled("  ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{}  ", status_lines[0].clone()),
                    Style::default().fg(Color::Green),
                ),
            ])
        } else {
            Spans::from("")
        }
    }

    pub fn selected(&self) -> Option<File> {
        let selected = self.list_state.selected()?;
        self.files.clone().into_iter().nth(selected + 1)
    }

    pub fn marked(&self) -> Vec<PathBuf> {
        let mut marked = Vec::new();
        for file in self.files.iter() {
            if self.marked.contains(&file.path) {
                marked.push(file.path.clone());
            }
        }
        marked
    }

    pub fn pathbar(&self) -> Result<Paragraph<'a>, Error> {
        let path = env::current_dir()?;
        let path = path
            .to_string_lossy()
            .replacen("/home/admin/", "/", 1)
            .replacen('/', "  ", 1)
            .replace('/', "    ");
        let pathbar = Paragraph::new(vec![Spans::from(vec![
            Span::styled(path, Style::default().fg(Color::White)),
            //Span::styled("  sample    ghi    qux", Style::default()),
        ])])
        .block(Block::default().borders(Borders::NONE))
        .style(Style::default().fg(Color::White).bg(Color::Rgb(39, 42, 45)))
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true });
        Ok(pathbar)
    }

    pub fn down(&mut self) {
        self.list_state.select(self.list_state.selected().map(|index| {
            if (index + 1) >= (self.files.count() - 1) as usize {
                return index;
            }
            index + 1
        }));
    }

    pub fn up(&mut self) {
        self.list_state
            .select(self.list_state.selected().map(|index| index.saturating_sub(1)));
    }

    pub fn bottom(&mut self) {
        self.list_state.select(Some(self.files.count() as usize - 2));
    }

    pub fn top(&mut self) {
        self.list_state.select(Some(0));
    }

    pub fn collapse(&mut self) {
        let mut collapsed: Option<PathBuf> = None;
        if let Some(selected) = self.selected_mut() {
            if selected.metadata.is_dir() {
                selected.descendants = vec![];
                collapsed = Some(selected.path.clone());
            }
        }
        if let Some(path) = collapsed {
            self.expanded.remove(&path);
        }
    }

    pub fn expand(&mut self) {
        let mut expanded: Option<PathBuf> = None;
        let show_hidden = self.configuration.show_hidden;
        if let Some(selected) = self.selected_mut() {
            if selected.metadata.is_dir() {
                let root = Application::read_dir(selected.path.clone(), show_hidden)
                    .expect("could not read directory");
                *selected = root;
                expanded = Some(selected.path.clone());
            }
        }
        if let Some(path) = expanded {
            self.expanded.insert(path);
        }
    }

    pub fn expand_toggle(&mut self) {
        if let Some(selected) = self.selected() {
            if self.expanded.contains(&selected.path) {
                self.collapse();
            } else {
                self.expand();
            }
        }
    }

    pub fn refresh(&mut self) {
        self.synchronize().expect("synchronization failed");
        self.updater.send(()).expect("could not send to a channel");
        if let Some(selected) = self.list_state.selected() {
            // Re-read the whole tree at current root.
            self.files = self
                .read_tree(self.files.path.clone())
                .expect("could not refresh");
            // If there are no files left, select nothing.
            // If the last file was deleted, select the new last file.
            let count = self.files.count();
            if count <= 1 {
                self.list_state.select(None);
            } else if selected > (count - 2) as usize {
                self.list_state.select(Some((count - 2) as usize));
            }
        } else {
            self.files = self
                .read_tree(self.files.path.clone())
                .expect("could not refresh");
            let count = self.files.count();
            if count > 1 {
                self.list_state.select(Some(0));
            }
        }
    }

    pub fn selected_mut(&mut self) -> Option<&mut File> {
        let selected = self.list_state.selected()?;
        find_target_file(&mut self.files, &mut 0, selected + 1)
    }

    pub fn change_root(&mut self) -> Result<(), Error> {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                env::set_current_dir(selected.path.clone());
                self.updater.send(())?;
                let root = Application::read_dir(selected.path.clone(), self.configuration.show_hidden)?;
                self.files = root;
                if selected.is_empty() {
                    self.list_state.select(None);
                } else {
                    self.list_state.select(Some(0));
                }
                self.expanded = HashSet::new();
                self.set_title()?;
            }
        }
        Ok(())
    }

    pub fn previous_root(&mut self) -> Result<(), Error> {
        // Go back to the previous root position.
        // If there is no previous root saved, go up one level.
        let root = self.files.path.clone();
        if let Some(path) = root.parent() {
            env::set_current_dir(path)?;
            self.updater.send(())?;
            let root = Application::read_dir(path.to_owned(), self.configuration.show_hidden)?;
            self.files_previous = self.files.path.clone();
            self.files = root;
            self.list_state.select(Some(0));
            self.expanded = HashSet::new();
            self.set_title()?;
        }
        // Position the current line on the child from which we moved.
        if let Some(name) = self.files_previous.file_name() {
            self.search_exact(name.to_string_lossy().to_string());
        }
        Ok(())
    }

    pub fn task_reload() {
        //
    }

    pub fn mark(&mut self) {
        if let Some(selected) = self.selected() {
            if !self.copied.contains(&selected.path) && !self.cut.contains(&selected.path) {
                if self.marked.contains(&selected.path) {
                    self.marked.remove(&selected.path);
                } else {
                    self.marked.insert(selected.path.clone());
                }
                self.down();
            }
        }
    }

    pub fn new_dir(&mut self, name: String) {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                let mut child = process::Command::new("fm-new-dir")
                    .arg(selected.path)
                    .arg(name)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
                self.refresh();
            } else if let Some(parent) = selected.path.parent() {
                let mut child = process::Command::new("fm-new-dir")
                    .arg(parent)
                    .arg(name)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
                self.refresh();
            }
        } else {
            let mut child = process::Command::new("fm-new-dir")
                .arg(self.files.path.clone())
                .arg(name)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
            self.refresh();
        }
    }

    pub fn new_file(&mut self, name: String) {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                let mut child = process::Command::new("fm-new-file")
                    .arg(selected.path)
                    .arg(name)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
                self.refresh();
            } else if let Some(parent) = selected.path.parent() {
                let mut child = process::Command::new("fm-new-file")
                    .arg(parent)
                    .arg(name)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
                self.refresh();
            }
        } else {
            let mut child = process::Command::new("fm-new-file")
                .arg(self.files.path.clone())
                .arg(name)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
            self.refresh();
        }
    }

    pub fn copy(&mut self) {
        let marked = self.marked();
        if marked.is_empty() {
            if let Some(selected) = self.selected() {
                self.copied.insert(selected.path.clone());
            }
        } else {
            self.copied.extend(self.marked.iter().cloned());
            self.marked.clear();
        }
        self.send_copied().expect("could not send copied");
    }

    pub fn cut(&mut self) {
        let marked = self.marked();
        if marked.is_empty() {
            if let Some(selected) = self.selected() {
                self.cut.insert(selected.path.clone());
            }
        } else {
            self.cut.extend(self.marked.iter().cloned());
            self.marked.clear();
        }
        // TODO: Handle the error.
        self.send_cut().expect("could not send cut");
    }

    pub fn synchronize(&mut self) -> Result<(), Error> {
        let socket_path = "/tmp/fm.sock";

        // Get the copy list from the server.
        {
            let mut client = UnixStream::connect(socket_path)?;
            let request = proto::Request {
                command: proto::Command::GetCopy.into(),
                files: vec![],
            };
            let response = send_server_request(&mut client, &request);

            if let Ok(proto::Response { status, files }) = response {
                if status != "success" {
                    return Err(anyhow!("server command did not succeed"));
                }
                self.copied = files.into_iter().map(PathBuf::from).collect();
            } else {
                return Err(anyhow!("failed to decode server response"));
            }
        }

        // Get the cut list from the server.
        {
            let mut client = UnixStream::connect(socket_path)?;
            let request = proto::Request {
                command: proto::Command::GetCut.into(),
                files: vec![],
            };

            let response = send_server_request(&mut client, &request);
            if let Ok(proto::Response { status, files }) = response {
                if status != "success" {
                    return Err(anyhow!("server command did not succeed"));
                }
                self.cut = files.into_iter().map(PathBuf::from).collect();
            } else {
                return Err(anyhow!("failed to decode server response"));
            }
        }
        Ok(())
    }

    pub fn send_copied(&self) -> Result<(), Error> {
        let mut copy_list: Vec<String> = vec![];
        for path in self.copied.iter() {
            copy_list.push(path.to_string_lossy().into());
        }
        let socket_path = "/tmp/fm.sock";
        let mut client = UnixStream::connect(socket_path)?;

        let request = proto::Request {
            command: proto::Command::Copy.into(),
            files: copy_list,
        };

        let response = send_server_request(&mut client, &request);

        if let Ok(proto::Response { status, files }) = response {
            if status != "success" {
                return Err(anyhow!("server command did not succeed"));
            }
        } else {
            return Err(anyhow!("failed to decode server response"));
        }

        Ok(())
    }

    pub fn send_cut(&self) -> Result<(), Error> {
        let mut cut_list: Vec<String> = vec![];
        for path in self.cut.iter() {
            cut_list.push(path.to_string_lossy().into());
        }
        let socket_path = "/tmp/fm.sock";
        let mut client = UnixStream::connect(socket_path)?;

        let request = proto::Request {
            command: proto::Command::Cut.into(),
            files: cut_list,
        };

        let response = send_server_request(&mut client, &request);

        if let Ok(proto::Response { status, files }) = response {
            if status != "success" {
                return Err(anyhow!("server command did not succeed"));
            }
        } else {
            return Err(anyhow!("failed to decode server response"));
        }

        Ok(())
    }

    pub fn send_clear(&self) -> Result<(), Error> {
        let socket_path = "/tmp/fm.sock";
        let mut client = UnixStream::connect(socket_path)?;

        let request = proto::Request {
            command: proto::Command::Clear.into(),
            files: vec![],
        };

        let response = send_server_request(&mut client, &request);

        if let Ok(proto::Response { status, files }) = response {
            if status != "success" {
                return Err(anyhow!("server command did not succeed"));
            }
        } else {
            return Err(anyhow!("failed to decode server response"));
        }

        Ok(())
    }

    pub fn cmd_path(&self) {
        if let Some(selected) = self.selected() {
            let mut child = process::Command::new("fm-cmd-path")
                .arg(selected.path)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
        }
    }

    pub fn cmd_mv(&mut self) {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                let mut child = process::Command::new("fm-cmd-mv")
                    .arg(selected.path)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            } else if let Some(parent) = selected.path.parent() {
                let child = process::Command::new("fm-cmd-mv")
                    .arg(parent)
                    .spawn()
                    .expect("failed to execute process");
            }
        } else {
            let mut child = process::Command::new("fm-cmd-mv")
                .arg(self.files.path.clone())
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
        }
        self.refresh();
    }

    pub fn cmd_cp(&mut self) {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                let mut child = process::Command::new("fm-cmd-cp")
                    .arg(selected.path)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            } else if let Some(parent) = selected.path.parent() {
                let mut child = process::Command::new("fm-cmd-cp")
                    .arg(parent)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            }
        } else {
            let mut child = process::Command::new("fm-cmd-cp")
                .arg(self.files.path.clone())
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
        }
        self.refresh();
    }

    pub fn paste(&mut self) {
        // When something is selected in the file list.
        if let Some(selected) = self.selected() {
            // When directory is selected.
            if selected.metadata.is_dir() {
                self.synchronize().expect("synchronization failed");
                for path in self.copied.iter() {
                    let mut child = process::Command::new("fm-paste")
                        .arg("copy")
                        .arg(path.clone())
                        .arg(selected.path.clone())
                        .spawn()
                        .expect("failed to execute process");
                    child.wait().expect("child process failed");
                }
                for path in self.cut.iter() {
                    let mut child = process::Command::new("fm-paste")
                        .arg("cut")
                        .arg(path.clone())
                        .arg(selected.path.clone())
                        .spawn()
                        .expect("failed to execute process");
                    child.wait().expect("child process failed");
                }
                self.clear_files();
                self.refresh();
            // When file is selected.
            } else if let Some(parent) = selected.path.parent() {
                self.synchronize().expect("synchronization failed");
                for path in self.copied.iter() {
                    let mut child = process::Command::new("fm-paste")
                        .arg("copy")
                        .arg(path.clone())
                        .arg(parent)
                        .spawn()
                        .expect("failed to execute process");
                    child.wait().expect("child process failed");
                }
                for path in self.cut.iter() {
                    let mut child = process::Command::new("fm-paste")
                        .arg("cut")
                        .arg(path.clone())
                        .arg(parent)
                        .spawn()
                        .expect("failed to execute process");
                    child.wait().expect("child process failed");
                }
                self.clear_files();
                self.refresh();
            }
        // When nothing is selected (in rare cases).
        } else {
            self.synchronize().expect("synchronization failed");
            let current = &self.files.path;
            for path in self.copied.iter() {
                let mut child = process::Command::new("fm-paste")
                    .arg("copy")
                    .arg(path.clone())
                    .arg(current.clone())
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            }
            for path in self.cut.iter() {
                let mut child = process::Command::new("fm-paste")
                    .arg("cut")
                    .arg(path.clone())
                    .arg(current.clone())
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            }
            self.clear_files();
            self.refresh();
        }
    }

    pub fn clear(&self) {
        // Clear the output buffer.
    }

    pub fn clear_files(&mut self) {
        self.marked = HashSet::new();
        self.cut = HashSet::new();
        self.copied = HashSet::new();
        // TODO: Handle the error.
        self.send_clear().expect("clear failed");
        self.refresh();
    }

    pub fn cmd_pre(&mut self) {
        disable_raw_mode().expect("could not disable raw mode");
        let size = self.terminal.size().expect("could not get terminal size");
        execute!(
            self.terminal.backend_mut(),
            cursor::MoveTo(0, 0),
            cursor::Show,
            DisableMouseCapture,
            Clear(ClearType::All),
        )
        .unwrap();
    }

    pub fn cmd_post(&mut self) {
        execute!(
            self.terminal.backend_mut(),
            Clear(ClearType::All),
            cursor::MoveTo(0, 0),
            EnterAlternateScreen,
            cursor::Hide,
            EnableMouseCapture
        )
        .unwrap();
        enable_raw_mode().expect("could not enable raw mode");
        let _ = self.terminal.clear();
        let _ = self.draw();
        let _ = self.set_title();
    }

    pub fn trash(&mut self) {
        let marked = self.marked();
        if marked.is_empty() {
            if let Some(selected) = self.selected() {
                let mut child = process::Command::new("fm-trash")
                    .arg(format!("\"{}\"", selected.path.display()))
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
                self.refresh();
            }
        } else {
            let mut marked_str = String::default();
            for path in marked {
                if marked_str.is_empty() {
                    marked_str = format!("\"{}\"", path.display());
                } else {
                    marked_str = format!("{} \"{}\"", marked_str, path.display());
                }
            }
            let mut child = process::Command::new("fm-trash")
                .arg(marked_str)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
            self.refresh();
        }
    }

    pub fn preview(&mut self) {
        if let Some(selected) = self.selected() {
            self.cmd_pre();

            let mut child = process::Command::new("fm-preview")
                .arg(selected.path)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
            self.cmd_post();
        }
    }

    pub fn open(&self) {
        if let Some(selected) = self.selected() {
            let mut child = process::Command::new("fm-open")
                .arg(selected.path)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
        }
    }

    pub fn rename(&mut self) {
        self.cmd_pre();
        let marked = self.marked();
        if marked.is_empty() {
            if let Some(selected) = self.selected() {
                let mut child = process::Command::new("fm-rename")
                    .arg(format!("\"{}\"", selected.path.to_string_lossy()))
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            }
        } else {
            let mut marked_str = String::default();
            for path in marked {
                if marked_str.is_empty() {
                    marked_str = format!("\"{}\"", path.to_string_lossy());
                } else {
                    marked_str = format!("{} \"{}\"", marked_str, path.to_string_lossy());
                }
            }
            let mut child = process::Command::new("fm-rename")
                .arg(marked_str)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
        }
        self.cmd_post();
        self.refresh();
    }

    pub fn edit(&mut self) {
        if let Some(selected) = self.selected() {
            self.cmd_pre();
            let mut child = process::Command::new("vim")
                .arg(selected.path)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
            self.cmd_post();
        }
    }

    pub fn editx(&self) {
        if let Some(selected) = self.selected() {
            let mut child = process::Command::new("window-edit")
                .arg(selected.path)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
        }
    }

    pub fn file_manager(&self) {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                let mut child = process::Command::new("directory.default")
                    .arg(selected.path.clone())
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            } else if let Some(parent) = selected.path.parent() {
                let mut child = process::Command::new("directory.default")
                    .arg(parent)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            }
        }
    }

    pub fn shell(&mut self) {
        self.cmd_pre();
        self.terminal
            .backend_mut()
            .execute(LeaveAlternateScreen)
            .expect("could not leave alternate screen");
        let mut child = process::Command::new("fm-shell")
            .spawn()
            .expect("failed to execute process");
        child.wait().expect("child process failed");
        self.cmd_post();
    }

    pub fn shellx(&self) {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                let mut child = process::Command::new("fm-shellx")
                    .arg(selected.path.clone())
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            } else if let Some(parent) = selected.path.parent() {
                let mut child = process::Command::new("fm-shellx")
                    .arg(parent)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            }
        }
    }

    pub fn shellx_root(&self) {
        let mut child = process::Command::new("fm-shellx")
            .arg(self.files.path.clone())
            .spawn()
            .expect("failed to execute process");
        child.wait().expect("child process failed");
    }

    pub fn images(&self) {
        let mut child = process::Command::new("fm-images")
            .spawn()
            .expect("failed to execute process");
        child.wait().expect("child process failed");
    }

    pub fn search(&mut self, input: String) {
        for (i, file) in self.files.clone().into_iter().enumerate() {
            if let Some(name) = file.path.file_name() {
                if name
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&input.to_lowercase())
                {
                    self.list_state.select(Some(i.saturating_sub(1)));
                    break;
                }
            }
        }
    }

    pub fn search_exact(&mut self, input: String) {
        for (i, file) in self.files.clone().into_iter().enumerate() {
            if let Some(name) = file.path.file_name() {
                if name.to_string_lossy().to_lowercase() == input.to_lowercase() {
                    self.list_state.select(Some(i.saturating_sub(1)));
                    break;
                }
            }
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.configuration.show_hidden = !self.configuration.show_hidden;
        self.refresh();
    }

    pub fn search_all(&mut self) {
        self.cmd_pre();
        let mut child = process::Command::new("fm-search-all")
            .arg(self.files.path.clone())
            .spawn()
            .expect("failed to execute process");
        child.wait().expect("child process failed");
        //let path = String::from_utf8_lossy(&output.stdout);
        let contents = fs::read_to_string("/tmp/fm-search-all").expect("cannot read /tmp/fm-search-all");
        let path = PathBuf::from(contents);
        // TODO: Expand the tree based on the obtained path and select the file or dir.
        self.cmd_post();
    }

    pub fn vscode(&self) {
        let mut child = process::Command::new("vscode.default")
            .arg(self.files.path.clone())
            .spawn()
            .expect("failed to execute process");
        child.wait().expect("child process failed");
    }

    pub fn drag_and_drop(&self) {
        let marked = self.marked();
        if marked.is_empty() {
            if let Some(selected) = self.selected() {
                let mut child = process::Command::new("fm-drag-and-drop")
                    .arg(selected.path)
                    .spawn()
                    .expect("failed to execute process");
                child.wait().expect("child process failed");
            }
        } else {
            let mut marked_str = String::default();
            for path in marked {
                if marked_str.is_empty() {
                    marked_str = format!("\"{}\"", path.to_string_lossy());
                } else {
                    marked_str = format!("{} \"{}\"", marked_str, path.to_string_lossy());
                }
            }
            let mut child = process::Command::new("fm-drag-and-drop")
                .arg(marked_str)
                .spawn()
                .expect("failed to execute process");
            child.wait().expect("child process failed");
        }
    }

    pub fn git_log(&mut self) {
        self.cmd_pre();
        let mut child = process::Command::new("fm-git-log")
            .spawn()
            .expect("failed to execute process");
        child.wait().expect("child process failed");
        self.cmd_post();
    }

    pub fn read_dir(dir: PathBuf, show_hidden: bool) -> Result<File> {
        let metadata = fs::metadata(&dir)?;
        let metadata_extra = fs::symlink_metadata(&dir)?;
        let mut descendants: Vec<File> = Vec::new();

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !show_hidden {
                if let Some(name) = entry.path().file_name() {
                    if name.to_string_lossy().starts_with('.') {
                        continue;
                    }
                }
            }

            let metadata = match fs::metadata(entry.path()) {
                Err(error) => match error.kind() {
                    ErrorKind::NotFound => {
                        error!("could not read file metadata: {}", entry.path().display());
                        continue;
                    }
                    _ => return Err(error.into()),
                },
                Ok(metadata) => metadata,
            };
            let metadata_extra = match fs::symlink_metadata(entry.path()) {
                Err(error) => match error.kind() {
                    ErrorKind::NotFound => {
                        error!("could not read file symlink metadata: {}", entry.path().display());
                        continue;
                    }
                    _ => return Err(error.into()),
                },
                Ok(metadata) => metadata,
            };

            let descendant = File {
                path: entry.path(),
                metadata,
                metadata_extra,
                descendants: vec![],
            };
            descendants.push(descendant);
        }
        descendants.sort();

        Ok(File {
            path: dir,
            metadata,
            metadata_extra,
            descendants,
        })
    }

    pub fn read_tree(&self, dir: PathBuf) -> Result<File, Error> {
        let metadata = fs::metadata(&dir)?;
        let metadata_extra = fs::symlink_metadata(&dir)?;
        let mut descendants: Vec<File> = Vec::new();

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !self.configuration.show_hidden {
                if let Some(name) = entry.path().file_name() {
                    if name.to_string_lossy().starts_with('.') {
                        continue;
                    }
                }
            }
            let metadata = fs::metadata(entry.path())?;
            let metadata_extra = fs::symlink_metadata(entry.path())?;

            let descendant = if metadata.is_dir() && self.expanded.contains(&entry.path()) {
                self.read_tree(entry.path())?
            } else {
                File {
                    path: entry.path(),
                    metadata,
                    metadata_extra,
                    descendants: vec![],
                }
            };
            descendants.push(descendant);
        }
        descendants.sort();

        Ok(File {
            path: dir,
            metadata,
            metadata_extra,
            descendants,
        })
    }

    pub fn quit(&mut self) -> Result<(), Error> {
        disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            ResetColor,
            LeaveAlternateScreen,
            cursor::Show,
        )?;
        process::exit(0);
    }

    pub fn quit_and_print(&mut self, output_path: String, paths: Vec<String>) -> Result<(), Error> {
        disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            ResetColor,
            LeaveAlternateScreen,
            cursor::Show,
        )?;
        let mut output_file = fs::File::create(output_path)?;
        for path in paths {
            writeln!(output_file, "{}", path);
        }
        process::exit(0);
    }

    pub fn quit_change(&mut self, last_dir_path: Option<&String>) -> Result<(), Error> {
        if let Some(path) = last_dir_path {
            let mut tmp = fs::File::create(path)?;
            tmp.write_all(self.files.path.to_string_lossy().as_bytes())?;
        }
        self.quit()
    }

    pub fn quit_print_dir(&mut self, output_path: String) -> Result<(), Error> {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_dir() {
                self.quit_and_print(output_path, vec![selected.path.to_string_lossy().into()])?;
            }
        }
        Ok(())
    }

    pub fn quit_print_marked(&mut self, output_path: String) -> Result<(), Error> {
        if !self.marked.is_empty() {
            let mut paths = vec![];
            for path in &self.marked {
                if path.is_file() {
                    paths.push(path.to_string_lossy().into());
                }
            }
            self.quit_and_print(output_path, paths)?;
        }
        Ok(())
    }

    pub fn quit_print_file(&mut self, output_path: String) -> Result<(), Error> {
        if let Some(selected) = self.selected() {
            if selected.metadata.is_file() {
                self.quit_and_print(output_path, vec![selected.path.to_string_lossy().into()]);
            }
        }
        Ok(())
    }

    pub fn save_cut_path(cut_path: String) -> Result<(), Error> {
        let file_path = "/home/admin/.local/share/fm/cut";

        let mut file_handle = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(file_path)?;

        file_handle.lock_exclusive()?;

        file_handle.write_all(cut_path.as_bytes())?;
        file_handle.sync_all()?;
        file_handle.unlock()?;
        Ok(())
    }
}

fn find_target_file<'a>(file: &'a mut File, current: &mut usize, target: usize) -> Option<&'a mut File> {
    if *current == target {
        return Some(file);
    }
    *current += 1;
    if !file.descendants.is_empty() {
        for descendant in &mut file.descendants {
            if let Some(descendant) = find_target_file(descendant, current, target) {
                return Some(descendant);
            }
        }
    }
    None
}

fn send_server_request(client: &mut UnixStream, request: &proto::Request) -> Result<proto::Response, Error> {
    let mut request_buffer = Vec::with_capacity(request.encoded_len());
    request
        .encode(&mut request_buffer)
        .expect("could not encode request");

    client.write_u32::<BigEndian>(request_buffer.len() as u32)?;
    client.write_all(&request_buffer)?;

    // Read the server response.
    let response_length = client.read_u32::<BigEndian>()? as usize;
    let mut response_buffer = vec![0; response_length];

    client.read_exact(&mut response_buffer)?;

    let mut response_cursor = Cursor::new(response_buffer);
    Ok(proto::Response::decode(&mut response_cursor)?)
}
