//! Custom error types for the fm application.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Main error type for the fm application.
#[derive(Debug, Error)]
pub enum FmError {
    #[error("Failed to read directory '{path}': {source}")]
    DirectoryRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Failed to read file metadata for '{path}': {source}")]
    MetadataRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Failed to read symlink '{path}': {source}")]
    SymlinkRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Failed to execute command '{command}': {message}")]
    CommandExecution { command: String, message: String },

    #[error("Command '{command}' failed with exit code {exit_code:?}")]
    CommandFailed {
        command: String,
        exit_code: Option<i32>,
    },

    #[error("Failed to connect to server at '{path}': {source}")]
    ServerConnection {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Server communication error: {message}")]
    ServerCommunication { message: String },

    #[error("Invalid path: {message}")]
    InvalidPath { message: String },

    #[error("Terminal error: {message}")]
    Terminal { message: String },

    #[error("Channel send error: {message}")]
    ChannelSend { message: String },

    #[error("Configuration error: {message}")]
    Config { message: String },

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result type alias for FmError.
pub type FmResult<T> = Result<T, FmError>;

impl FmError {
    /// Create a DirectoryRead error.
    pub fn directory_read(path: impl Into<PathBuf>, source: io::Error) -> Self {
        FmError::DirectoryRead {
            path: path.into(),
            source,
        }
    }

    /// Create a CommandExecution error.
    pub fn command_exec(command: impl Into<String>, message: impl Into<String>) -> Self {
        FmError::CommandExecution {
            command: command.into(),
            message: message.into(),
        }
    }

    /// Create a CommandFailed error.
    pub fn command_failed(command: impl Into<String>, exit_code: Option<i32>) -> Self {
        FmError::CommandFailed {
            command: command.into(),
            exit_code,
        }
    }

    /// Create a ServerCommunication error.
    pub fn server_comm(message: impl Into<String>) -> Self {
        FmError::ServerCommunication {
            message: message.into(),
        }
    }

    /// Create an InvalidPath error.
    pub fn invalid_path(message: impl Into<String>) -> Self {
        FmError::InvalidPath {
            message: message.into(),
        }
    }

    /// Create a Terminal error.
    pub fn terminal(message: impl Into<String>) -> Self {
        FmError::Terminal {
            message: message.into(),
        }
    }

    /// Create a ChannelSend error.
    pub fn channel_send(message: impl Into<String>) -> Self {
        FmError::ChannelSend {
            message: message.into(),
        }
    }
}
