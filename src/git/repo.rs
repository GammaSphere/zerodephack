//! Finding a repository and reading objects out of it.
//!
//! [`Repository`] is immutable once opened, so it can be shared across threads.
//! Reading needs mutable state - open file handles and a delta cache - so that
//! lives in [`Reader`], and each worker thread makes its own.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::git::config::Config;
use crate::git::error::{Error, Result};
use crate::git::object::{Commit, Kind, Object, Tree};
use crate::git::oid::Oid;
use crate::git::pack::{Pack, PackReader};
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
    packs: Vec<Pack>,
    /// Packs that failed to load. Kept rather than discarded so a run can warn
    /// that its numbers may be incomplete.
    pack_problems: Vec<String>,
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
        let (packs, problems) = Pack::discover(&git_dir);

        Ok(Repository {
            git_dir,
            work_tree,
            shallow,
            packs,
            pack_problems: problems.iter().map(|e| e.to_string()).collect(),
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

    /// Packs that could not be loaded, described for a warning line.
    pub fn pack_problems(&self) -> &[String] {
        &self.pack_problems
    }

    /// Total objects across every pack, which is most of them after a gc.
    pub fn packed_object_count(&self) -> usize {
        self.packs.iter().map(|p| p.index().len()).sum()
    }

    /// Every object id held in a pack, in index order. Used by the object-layer
    /// conformance test, which reads all of them and compares against git.
    pub fn packed_oids(&self) -> Vec<Oid> {
        self.packs
            .iter()
            .flat_map(|p| p.index().oids().iter().copied())
            .collect()
    }

    pub fn head(&self) -> Result<Head> {
        refs::head(&self.git_dir)
    }

    pub fn refs(&self) -> Result<Vec<(String, Oid)>> {
        refs::all(&self.git_dir)
    }

    /// Open a reader. Each thread needs its own: readers hold file handles and
    /// a delta cache, neither of which is shareable.
    pub fn reader(&self) -> Result<Reader<'_>> {
        let packs = self
            .packs
            .iter()
            .map(Pack::reader)
            .collect::<Result<Vec<_>>>()?;
        Ok(Reader { repo: self, packs })
    }
}

/// A handle for reading objects. Holds the open packfiles and their caches.
pub struct Reader<'a> {
    repo: &'a Repository,
    packs: Vec<PackReader<'a>>,
}

impl Reader<'_> {
    /// Read an object by id, from loose storage or from any pack.
    ///
    /// Loose objects are checked first. After `git gc` writes a pack, the loose
    /// copy may linger until it is pruned, and both hold the same bytes, so
    /// order is a matter of cost rather than correctness: one stat beats a
    /// binary search per pack.
    pub fn object(&mut self, oid: Oid) -> Result<Object> {
        if let Some(object) = self.read_loose(oid)? {
            return Ok(object);
        }

        for pack in &mut self.packs {
            if let Some((kind, data)) = pack.object(oid)? {
                return Ok(Object { kind, data });
            }
        }

        Err(Error::ObjectNotFound { oid })
    }

    pub fn commit(&mut self, oid: Oid) -> Result<Commit> {
        let object = self.object(oid)?;
        expect_kind(&object, Kind::Commit, oid)?;
        Commit::parse(&object.data)
    }

    pub fn tree(&mut self, oid: Oid) -> Result<Tree> {
        let object = self.object(oid)?;
        expect_kind(&object, Kind::Tree, oid)?;
        Tree::parse(&object.data)
    }

    /// True when the object exists, without paying to reconstruct it.
    pub fn contains(&self, oid: Oid) -> bool {
        if self.loose_path(oid).is_file() {
            return true;
        }
        self.packs.iter().any(|p| p.contains(oid))
    }

    fn loose_path(&self, oid: Oid) -> PathBuf {
        let hex = oid.to_string();
        self.repo
            .git_dir
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
    }

    /// Loose objects live at `objects/ab/cdef...`, split so no single directory
    /// holds every object in the repository.
    fn read_loose(&self, oid: Oid) -> Result<Option<Object>> {
        let path = self.loose_path(oid);

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
    fn discovery_reports_a_clear_error_outside_a_repository() {
        // Walking up from the filesystem root can never find a repository, so
        // this pins the message rather than depending on where tests run.
        let Err(err) = Repository::discover(Path::new("/")) else {
            panic!("the filesystem root is not a repository");
        };
        assert!(
            matches!(err, Error::NotARepository { .. }),
            "unexpected error {err}"
        );
        assert!(err.to_string().contains("no git repository found"));
    }
}
