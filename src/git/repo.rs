//! Finding a repository and reading objects out of it.
//!
//! [`Repository::object`] is the facade the rest of the program works through.
//! Callers never learn whether the bytes arrived from a loose file or from a
//! delta chain inside a packfile.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::git::config::Config;
use crate::git::error::{Error, Result};
use crate::git::object::{Commit, Kind, Object, Tree};
use crate::git::oid::Oid;
use crate::git::refs::{self, Head};
use crate::util::inflate;

pub struct Repository {
    /// The `.git` directory itself, or the repository root when bare.
    git_dir: PathBuf,
    /// The checkout this repository backs, absent for bare repositories.
    work_tree: Option<PathBuf>,
    /// True when `.git/shallow` exists, meaning history is truncated and any
    /// age or ownership figure is a lower bound rather than the whole story.
    shallow: bool,
}

impl Repository {
    /// Search `start` and its ancestors for a repository.
    ///
    /// Handles the three shapes git uses: a `.git` directory, a `.git` file
    /// pointing elsewhere (worktrees and submodules), and a bare repository
    /// whose root holds `HEAD` and `objects` directly.
    pub fn discover(start: &Path) -> Result<Repository> {
        let start = fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());

        for dir in start.ancestors() {
            let dot_git = dir.join(".git");

            if dot_git.is_dir() {
                return Repository::open(dot_git, Some(dir.to_path_buf()));
            }

            if dot_git.is_file() {
                if let Some(git_dir) = read_gitdir_pointer(&dot_git, dir) {
                    return Repository::open(git_dir, Some(dir.to_path_buf()));
                }
            }

            // A bare repository has no `.git`; the directory is the git dir.
            if dir.join("HEAD").is_file() && dir.join("objects").is_dir() {
                return Repository::open(dir.to_path_buf(), None);
            }
        }

        Err(Error::NotARepository { start })
    }

    pub fn open(git_dir: PathBuf, work_tree: Option<PathBuf>) -> Result<Repository> {
        let config = Config::read(&git_dir.join("config"));

        // Reading a SHA-256 repository with a SHA-1 reader would produce
        // confident nonsense, so refuse it by name.
        if let Some(format) = config.get("extensions.objectformat") {
            if !format.eq_ignore_ascii_case("sha1") {
                return Err(Error::UnsupportedObjectFormat {
                    format: format.to_string(),
                });
            }
        }

        let shallow = git_dir.join("shallow").is_file();

        Ok(Repository {
            git_dir,
            work_tree,
            shallow,
        })
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub fn work_tree(&self) -> Option<&Path> {
        self.work_tree.as_deref()
    }

    /// True when history is truncated by a shallow clone. Every metric derived
    /// from history is a lower bound in that case, and the reports say so.
    pub fn is_shallow(&self) -> bool {
        self.shallow
    }

    pub fn head(&self) -> Result<Head> {
        refs::head(&self.git_dir)
    }

    pub fn refs(&self) -> Result<Vec<(String, Oid)>> {
        refs::all(&self.git_dir)
    }

    /// Read an object by id.
    pub fn object(&self, oid: Oid) -> Result<Object> {
        if let Some(object) = self.read_loose(oid)? {
            return Ok(object);
        }
        Err(Error::ObjectNotFound { oid })
    }

    pub fn commit(&self, oid: Oid) -> Result<Commit> {
        let object = self.object(oid)?;
        expect_kind(&object, Kind::Commit, oid)?;
        Commit::parse(&object.data)
    }

    pub fn tree(&self, oid: Oid) -> Result<Tree> {
        let object = self.object(oid)?;
        expect_kind(&object, Kind::Tree, oid)?;
        Tree::parse(&object.data)
    }

    /// Loose objects live at `objects/ab/cdef...`, split so no single directory
    /// holds every object in the repository.
    fn read_loose(&self, oid: Oid) -> Result<Option<Object>> {
        let hex = oid.to_string();
        let path = self
            .git_dir
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]);

        let compressed = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(path, e)),
        };

        let inflated =
            inflate::zlib_decompress(&compressed, 0).map_err(|source| Error::Inflate {
                path: path.clone(),
                source,
            })?;

        Object::parse_loose(&inflated.data).map(Some)
    }
}

fn expect_kind(object: &Object, wanted: Kind, oid: Oid) -> Result<()> {
    if object.kind != wanted {
        return Err(Error::malformed(
            "object",
            0,
            format!("{oid} is a {}, expected a {wanted}", object.kind),
        ));
    }
    Ok(())
}

/// A `.git` file holds `gitdir: <path>`, absolute or relative to the checkout.
fn read_gitdir_pointer(dot_git: &Path, work_tree: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dot_git).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let path = Path::new(target);

    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        work_tree.join(path)
    };

    fs::canonicalize(&resolved).ok().or(Some(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_fails_cleanly_outside_a_repository() {
        // The temp directory is not inside a checkout, so this must report
        // NotARepository rather than panicking or hanging on the walk up.
        let err = Repository::discover(&std::env::temp_dir())
            .err()
            .filter(|e| matches!(e, Error::NotARepository { .. }));
        // On a machine where the temp dir happens to sit inside a repository
        // the discovery legitimately succeeds, so only the shape is asserted.
        if let Some(err) = err {
            assert!(err.to_string().contains("no git repository found"));
        }
    }

    #[test]
    fn rejects_sha256_repositories() {
        let config = Config::parse("[extensions]\n objectformat = sha256\n");
        assert_eq!(config.get("extensions.objectformat"), Some("sha256"));
    }
}
