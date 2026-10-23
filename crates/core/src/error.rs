//! One error type for the whole engine. Every message is written for the
//! person using the app: what happened and what to do about it.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// A problem the user can act on (wrong folder, missing tool, ...).
    User(String),
    /// The model provider refused or failed for good.
    Llm(String),
    /// The spending limit was reached.
    Budget(String),
    /// The user pressed Stop.
    Cancelled,
    /// A model reply hit the length limit (internal: the caller splits the work).
    Truncated,
    Io(std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::User(m) | Error::Llm(m) | Error::Budget(m) => f.write_str(m),
            Error::Cancelled => f.write_str("Stopped. Everything translated so far is saved."),
            Error::Truncated => f.write_str("A reply was cut off by the length limit."),
            Error::Io(e) => match e.kind() {
                std::io::ErrorKind::PermissionDenied => f.write_str(
                    "Windows won't let Yakusoku change those files. Close the game (and anything else using its files), then try again.",
                ),
                std::io::ErrorKind::NotFound => write!(f, "A file or folder is missing: {e}"),
                _ => write!(f, "File error: {e}"),
            },
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::User(format!("Couldn't read a data file: {e}"))
    }
}

/// Shorthand for a user-facing error.
pub fn user<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::User(msg.into()))
}
