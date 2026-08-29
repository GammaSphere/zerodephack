//! Reading the git object database.
//!
//! The layer below `analysis` and above `util`. Callers work through
//! [`repo::Repository`], which resolves an object id without revealing whether
//! the bytes came from a loose file or from a delta chain inside a packfile.

pub mod config;
pub mod error;
pub mod object;
pub mod pack;
pub mod oid;
pub mod refs;
pub mod sha1;
pub mod repo;

pub use error::{Error, Result};
pub use oid::Oid;
pub use repo::{Reader, Repository};
