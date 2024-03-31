use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Error, Result};
use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = include_str!("../desktop/config.toml");

#[derive(Debug, Deserialize)]
pub struct Config {
    pub icon_spacing: u8,
    pub status_line_spacing: u8,
    pub path_line_spacing: u8,
    pub indent_first_level: bool,
    pub selection_symbol: String,
    pub indent_guide: String,
    pub indent_spaces: u8,
    pub mouse: bool,
    pub show_hidden: bool,
    pub shell: Vec<String>,
    pub info: Vec<String>,
    pub status: Status,
    pub keys: Keys,
    pub style: Style,
    pub files: Vec<Files>,
}

#[derive(Debug, Deserialize)]
pub struct Status {
    pub left: Vec<String>,
    pub center: Vec<String>,
    pub right: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Keys {
    pub l: String,
    pub up: String,
    pub k: String,
    pub down: String,
    pub j: String,
    pub left: String,
    #[serde(rename = ";")]
    pub semicolon: String,
    pub right: String,
    pub a: String,
    pub f: String,
    pub n: String,
    #[serde(rename = "N")]
    pub shift_n: String,
    #[serde(rename = "V")]
    pub shift_v: String,
    pub e: String,
    #[serde(rename = "E")]
    pub shift_e: String,
    pub s: String,
    #[serde(rename = "S")]
    pub shift_s: String,
    #[serde(rename = "C-s")]
    pub ctrl_s: String,
    pub space: String,
    pub y: String,
    pub c: String,
    pub p: String,
    #[serde(rename = "C")]
    pub shift_c: String,
    #[serde(rename = "T")]
    pub shift_t: String,
    pub escape: String,
    #[serde(rename = ":")]
    pub colon: String,
    pub q: String,
    #[serde(rename = "Q")]
    pub shift_q: String,
    pub i: String,
    pub o: String,
    pub r: String,
    #[serde(rename = "/")]
    pub slash: String,
    #[serde(rename = "?")]
    pub question_mark: String,
    #[serde(rename = "I")]
    pub shift_i: String,
    #[serde(rename = "D")]
    pub shift_d: String,
    #[serde(rename = "L")]
    pub shift_l: String,
    #[serde(rename = "C-r")]
    pub ctrl_r: String,
    pub g: Goto,
}

#[derive(Debug, Deserialize)]
pub struct Goto {
    pub g: String,
    pub e: String,
}

#[derive(Debug, Deserialize)]
pub struct Style {
    pub default: InterfaceStyle,
    pub cursor_line: InterfaceStyle,
    pub status_line: InterfaceStyle,
    pub command_line: InterfaceStyle,
    pub path_line: InterfaceStyle,
    pub directory: FilesStyle,
    pub file: FilesStyle,
    pub archive: FilesStyle,
    pub video: FilesStyle,
    pub audio: FilesStyle,
    pub image: FilesStyle,
    pub document: FilesStyle,
    pub link: FilesStyle,
    pub executable: FilesStyle,
}

#[derive(Debug, Deserialize)]
pub struct InterfaceStyle {
    pub fg: String,
    pub bg: String,
    pub modifiers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct FilesStyle {
    pub icon: String,
    pub fg: String,
    pub bg: String,
    pub modifiers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Files {
    pub extensions: Vec<String>,
    pub style: String,
}

pub fn read_config<P: AsRef<Path>>(path: P) -> Result<Config> {
    let mut file = File::open(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let config: Config = toml::from_str(&contents)?;
    Ok(config)
}

impl Config {
    pub fn set_key(&mut self, key: &str, value: &str) -> Result<()> {
        if key == "show_hidden" {
            self.show_hidden = value == "true";
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_config() {
        let _ = read_config("./desktop/config.toml").unwrap();
    }
}
