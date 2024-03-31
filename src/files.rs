#![allow(unused)]
use std::cell::RefCell;
use std::cmp::{Eq, Ord, Ordering, PartialEq, PartialOrd};
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{fs, io, path, process};

use anyhow::{anyhow, Error, Result};
use git2::{DiffOptions, Repository};
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::ListItem;

use crate::Config;

#[derive(Clone, Debug)]
pub struct File {
    pub path: PathBuf,
    pub metadata: fs::Metadata,
    pub metadata_extra: fs::Metadata,
    pub descendants: Vec<File>,
}

impl Ord for File {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.metadata.is_dir() && !other.metadata.is_dir() {
            Ordering::Less
        } else if other.metadata.is_dir() && !self.metadata.is_dir() {
            Ordering::Greater
        } else {
            // TODO: Can I not use the lossy conversion here in order to lowercase a path?
            self.path
                .to_string_lossy()
                .to_lowercase()
                .cmp(&other.path.to_string_lossy().to_lowercase())
        }
    }
}

impl PartialOrd for File {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for File {}

impl PartialEq for File {
    fn eq(&self, other: &Self) -> bool {
        self.path.eq(&other.path)
    }
}

pub struct FileIterator {
    root: File,
    index: usize,
}

/// The iterator implementation walks the multi-way file tree.
impl Iterator for FileIterator {
    type Item = File;

    fn next(&mut self) -> Option<Self::Item> {
        let file = file_at_index(&mut self.root, &mut 0, self.index)?;
        self.index += 1;
        Some(file.clone())
    }
}

impl IntoIterator for File {
    type Item = File;
    type IntoIter = FileIterator;

    fn into_iter(self) -> Self::IntoIter {
        FileIterator { root: self, index: 0 }
    }
}

pub struct FileIteratorRef<'a> {
    stack: Vec<&'a File>,
}

impl<'a> FileIteratorRef<'a> {
    fn new(root: &'a File) -> FileIteratorRef<'a> {
        let stack = vec![root];
        FileIteratorRef { stack }
    }
}

impl<'a> Iterator for FileIteratorRef<'a> {
    type Item = &'a File;

    fn next(&mut self) -> Option<Self::Item> {
        let file: Self::Item = self.stack.pop()?;
        if !file.descendants.is_empty() {
            for descendant in file.descendants.iter().rev() {
                self.stack.push(descendant);
            }
        }
        Some(file)
    }
}

pub fn file_at_index<'a>(file: &'a mut File, current: &mut usize, index: usize) -> Option<&'a mut File> {
    if *current == index {
        return Some(file)
    }
    *current += 1;
    if !file.descendants.is_empty() {
        for descendant in &mut file.descendants {
            if let Some(file) = file_at_index(descendant, current, index) {
                return Some(file)
            }
        }
    }
    None
}

pub fn apply_to_all<'a>(
    file: &'a mut File,
    arg: Option<String>,
    f: fn(&'a mut File, Option<String>) -> &'a mut File,
) {
    let file = f(file, arg.clone());
    if !file.descendants.is_empty() {
        for descendant in &mut file.descendants {
            apply_to_all(descendant, arg.clone(), f);
        }
    }
}

impl File {
    pub fn is_empty(&self) -> bool {
        self.metadata.is_dir() && fs::read_dir(&self.path).expect("could not read dir").count() == 0
    }

    pub fn info_count<'a>(&self) -> Result<Span<'a>, Error> {
        if self.metadata.is_dir() {
            let mut count = 0;
            match fs::read_dir(self.path.clone()) {
                Ok(entries) => {
                    for entry in entries {
                        count += 1;
                    }
                }
                Err(error) => {
                    if error.kind() == io::ErrorKind::PermissionDenied {
                        return Ok(Span::styled("0", Style::default().fg(Color::Red)))
                    }
                }
            }
            Ok(Span::styled(
                format!("{}", count),
                Style::default().fg(Color::Blue),
            ))
        } else {
            Ok(Span::styled(format!("{}", self.metadata.len()), Style::default()))
        }
    }

    pub fn iter(&self) -> FileIteratorRef {
        FileIteratorRef::new(self)
    }

    pub fn is_video(&self) -> bool {
        if let Some(extension) = self.path.extension() {
            if extension == "mkv"
                || extension == "mp4"
                || extension == "webm"
                || extension == "wav"
                || extension == "avi"
            {
                return true
            }
        }
        false
    }

    pub fn is_audio(&self) -> bool {
        if let Some(extension) = self.path.extension() {
            if extension == "mp3" {
                return true
            }
        }
        false
    }

    pub fn is_image(&self) -> bool {
        if let Some(extension) = self.path.extension() {
            if extension == "jpg"
                || extension == "jpeg"
                || extension == "png"
                || extension == "svg"
                || extension == "webp"
                || extension == "gif"
            {
                return true
            }
        }
        false
    }

    pub fn is_archive(&self) -> bool {
        if let Some(extension) = self.path.extension() {
            if extension == "zip" || extension == "tar" || extension == "gz" {
                return true
            }
        }
        false
    }

    pub fn is_document(&self) -> bool {
        if let Some(extension) = self.path.extension() {
            if extension == "epub"
                || extension == "pdf"
                || extension == "mobi"
                || extension == "ipynb"
                || extension == "azw"
            {
                return true
            }
        }
        false
    }

    pub fn is_executable(&self) -> bool {
        self.metadata.permissions().mode() & 0o111 != 0
    }

    pub fn count(&self) -> u32 {
        let mut count = 0;
        count_files(self, &mut count);
        count
    }
}

fn count_files(file: &File, count: &mut u32) {
    *count += 1;
    if !file.descendants.is_empty() {
        for descendant in &file.descendants {
            count_files(descendant, count);
        }
    }
}

fn git_modified<'a>(file: Box<File>) -> Result<Span<'a>, Error> {
    if let Ok(repo) = Repository::open(".") {
        let repo_path = repo.path().parent().expect("failed to read repo path");
        let submodule_path = file
            .path
            .strip_prefix(repo_path)
            .expect("failed to strip prefix on repo path");
        if let Ok(submodule) = repo.find_submodule(&submodule_path.to_string_lossy()) {
            return Ok(Span::styled(" S", Style::default().fg(Color::Cyan)))
        }
    } else {
        return Ok(Span::raw(""))
    }

    // Check if file is a submodule.
    let output = process::Command::new("fm-git-submodule")
        .arg(file.path.clone())
        .output()
        .expect("failed to execute command");

    let output_str = String::from_utf8_lossy(&output.stdout);

    if output_str == " S" {
        return Ok(Span::styled(" S", Style::default().fg(Color::Cyan)))
    }
    // Check git file status.
    let output = process::Command::new("fm-git-status")
        .arg(file.path.clone())
        .output()
        .expect("failed to execute command");

    let output_str = String::from_utf8_lossy(&output.stdout);
    if output_str.is_empty() {
        Ok(Span::styled("", Style::default().fg(Color::Yellow)))
    } else if output_str.eq("U") {
        Ok(Span::styled(
            format!(" {}", output_str),
            Style::default().fg(Color::Red),
        ))
    } else {
        Ok(Span::styled(
            format!(" {}", output_str),
            Style::default().fg(Color::Yellow),
        ))
    }
}
