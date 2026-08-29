//! Errors raised while reading a repository.
//!
//! Replaces what `thiserror` would generate. The `Display` implementations are
//! written to be read by a person at a terminal, so they name the file and the
//! byte offset wherever one is known.

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::git::oid::Oid;
use crate::util::inflate::InflateError;
use crate::util::paths;

#[derive(Debug)]
pub enum Error {
    /// No `.git` directory was found at or above the starting path. This is the
    /// case the tool degrades gracefully on rather than treating as a crash.
    NotARepository { start: PathBuf },
    /// The repository uses SHA-256 object names, which this build cannot read.
    UnsupportedObjectFormat { format: String },
    /// An object was referenced but is present in neither loose storage nor any
    /// packfile. Shallow clones make this expected rather than exceptional.
    ObjectNotFound { oid: Oid },
    /// A file could not be read.
    Io { path: PathBuf, source: io::Error },
    /// A zlib stream failed to decode.
    Inflate { path: PathBuf, source: InflateError },
    /// A structure was malformed. `offset` is relative to the start of the
    /// decompressed object, or of the file when nothing was decompressed.
    Malformed {
        what: &'static str,
        offset: usize,
        detail: String,
    },
}

impl Error {
    pub fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> Self {
        Error::Malformed {
            what,
            offset,
            detail: detail.into(),
        }
    }

    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotARepository { start } => write!(
                f,
                "no git repository found at or above {}",
                paths::display(start)
            ),
            Error::UnsupportedObjectFormat { format } => write!(
                f,
                "repository uses the {format} object format; strata reads sha1 repositories only"
            ),
            Error::ObjectNotFound { oid } => {
                write!(f, "object {oid} is not present in this repository")
            }
            Error::Io { path, source } => write!(f, "{}: {source}", paths::display(path)),
            Error::Inflate { path, source } => {
                write!(
                    f,
                    "{}: corrupt compressed data {source}",
                    paths::display(path)
                )
            }
            Error::Malformed {
                what,
                offset,
                detail,
            } => write!(f, "malformed {what} at byte {offset}: {detail}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Inflate { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
