//! Walking history and turning it into per-file change events.
//!
//! Everything the reports need comes from one pass: which files each commit
//! touched, who made it, and when. Paths and authors are interned to integers,
//! so the analyses that follow compare and count without hashing strings.
//!
//! Merge commits are excluded by default. A merge records no original work -
//! its changes already appear in the commits it joins - and counting them
//! inflates every file a large merge touches. `git log` follows the same
//! convention for file history, and `--include-merges` turns it off.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::git::error::Result;
use crate::git::object::{Signature, TreeEntry};
use crate::git::oid::Oid;
use crate::git::repo::{Reader, Repository};

/// A person, keyed by email because people change how they spell their name
/// far more often than they change their address.
#[derive(Debug, Clone)]
pub struct Author {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// A file that arrived at this path carrying content that left another one
    /// in the same commit. Only exact renames are detected; see [`detect_renames`].
    Renamed,
}

/// One file-level change, before renames are collapsed.
struct Change {
    path: u32,
    kind: ChangeKind,
    /// The blob at this path. For a deletion, the content that left.
    content: Oid,
}

/// Collapse exact renames: the same blob leaving one path and arriving at
/// another within a single commit becomes one [`ChangeKind::Renamed`] at the
/// new path, matching what `git log --name-only` reports.
///
/// Only exact matches are found. Git also detects renames where the file was
/// edited on the way, scoring similarity against a threshold. strata does not,
/// so a file moved *and* modified in one commit still reads as a delete plus an
/// add. That is a documented limit, not an oversight - see README.
fn detect_renames(changes: &mut Vec<Change>) {
    let mut departed: HashMap<Oid, Vec<usize>> = HashMap::new();
    for (index, change) in changes.iter().enumerate() {
        if change.kind == ChangeKind::Deleted {
            departed.entry(change.content).or_default().push(index);
        }
    }
    if departed.is_empty() {
        return;
    }

    let mut consumed = vec![false; changes.len()];
    for change in changes.iter_mut() {
        if change.kind != ChangeKind::Added {
            continue;
        }
        // Each departure pairs with at most one arrival, so a blob copied to
        // three new paths reports one rename and two additions.
        if let Some(source) = departed.get_mut(&change.content).and_then(Vec::pop) {
            consumed[source] = true;
            change.kind = ChangeKind::Renamed;
        }
    }

    let mut kept = Vec::with_capacity(changes.len());
    for (change, dropped) in changes.drain(..).zip(consumed) {
        if !dropped {
            kept.push(change);
        }
    }
    *changes = kept;
}

#[derive(Debug, Clone)]
pub struct CommitRecord {
    pub oid: Oid,
    pub author: u32,
    /// Author time in seconds since the epoch. Author time rather than commit
    /// time, so a rebase does not restate when the work happened.
    pub time: i64,
    pub summary: String,
    pub is_merge: bool,
    /// True when the commit touched more files than the coupling threshold.
    ///
    /// Such a commit still counts toward churn, ownership and age - it is real
    /// work by a real person. It is only excluded from coupling, where a mass
    /// rename or a vendored import would otherwise couple every file it moved
    /// to every other.
    pub too_broad_for_coupling: bool,
    /// Files this commit touched, as indices into [`History::paths`].
    pub touched: Vec<(u32, ChangeKind)>,
}

/// The result of one pass over history.
pub struct History {
    pub authors: Vec<Author>,
    pub paths: Vec<String>,
    pub commits: Vec<CommitRecord>,
    /// Commits that could not be read, usually because a shallow clone cut
    /// history off. Reported rather than silently skipped.
    pub unreadable: usize,
}

impl History {
    pub fn path(&self, index: u32) -> &str {
        &self.paths[index as usize]
    }

    pub fn author(&self, index: u32) -> &Author {
        &self.authors[index as usize]
    }

    /// Commits that count as work: non-merges, unless merges were requested.
    pub fn working_commits(&self) -> impl Iterator<Item = &CommitRecord> {
        self.commits.iter()
    }

    pub fn newest_time(&self) -> i64 {
        self.commits.iter().map(|c| c.time).max().unwrap_or(0)
    }

    pub fn oldest_time(&self) -> i64 {
        self.commits.iter().map(|c| c.time).min().unwrap_or(0)
    }
}

/// What to include in a walk.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Ignore commits older than this epoch second.
    pub since: Option<i64>,
    /// Keep merge commits, which are dropped by default.
    pub include_merges: bool,
    /// Skip commits touching more than this many files. A mass rename or a
    /// vendored-tree import is not evidence that its files belong together,
    /// and it would otherwise dominate the coupling report.
    pub max_files_per_commit: usize,
}

impl Options {
    pub const DEFAULT_MAX_FILES: usize = 50;

    pub fn new() -> Options {
        Options {
            since: None,
            include_merges: false,
            max_files_per_commit: Options::DEFAULT_MAX_FILES,
        }
    }
}

/// Walk history from `tip`, diffing each commit against its first parent.
pub fn walk(repo: &Repository, tip: Oid, options: &Options) -> Result<History> {
    let mut reader = repo.reader()?;

    let mut authors = Interner::new();
    let mut paths = Interner::new();
    let mut commits = Vec::new();
    let mut unreadable = 0usize;

    // Breadth-first over the parent graph. Order does not matter for any of the
    // metrics, only that each commit is visited exactly once.
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut queue = vec![tip];

    while let Some(oid) = queue.pop() {
        if !seen.insert(oid) {
            continue;
        }

        let Ok(commit) = reader.commit(oid) else {
            // A shallow clone's boundary commits name parents that are not
            // present. That is expected, not corruption.
            unreadable += 1;
            continue;
        };

        queue.extend(commit.parents.iter().copied());

        if let Some(since) = options.since
            && commit.author.time < since
        {
            continue;
        }

        let is_merge = commit.parents.len() > 1;
        if is_merge && !options.include_merges {
            continue;
        }

        // Diff against the first parent. For a root commit there is nothing to
        // compare against, so every file in the tree counts as added.
        let parent_tree = match commit.parents.first() {
            Some(&parent) => reader.commit(parent).ok().map(|p| p.tree),
            None => None,
        };

        let mut changes = Vec::new();
        diff_trees(
            &mut reader,
            parent_tree,
            Some(commit.tree),
            "",
            &mut paths,
            &mut changes,
        )?;
        detect_renames(&mut changes);
        let changes: Vec<(u32, ChangeKind)> =
            changes.into_iter().map(|c| (c.path, c.kind)).collect();

        commits.push(CommitRecord {
            oid,
            author: intern_author(&mut authors, &commit.author),
            time: commit.author.time,
            summary: commit.summary,
            is_merge,
            too_broad_for_coupling: changes.len() > options.max_files_per_commit,
            touched: changes,
        });
    }

    Ok(History {
        authors: authors.into_authors(),
        paths: paths.into_values(),
        commits,
        unreadable,
    })
}

/// Recursively diff two trees, appending changed file paths.
///
/// Subtrees with equal object ids are identical by construction, so the whole
/// branch is skipped. That prune is what keeps a full-history walk affordable:
/// most commits touch one corner of the tree and leave the rest untouched.
fn diff_trees(
    reader: &mut Reader<'_>,
    old: Option<Oid>,
    new: Option<Oid>,
    prefix: &str,
    paths: &mut Interner,
    out: &mut Vec<Change>,
) -> Result<()> {
    if old == new {
        return Ok(());
    }

    let old_entries = match old {
        Some(oid) => entries_by_name(reader, oid)?,
        None => BTreeMap::new(),
    };
    let new_entries = match new {
        Some(oid) => entries_by_name(reader, oid)?,
        None => BTreeMap::new(),
    };

    let names: HashSet<&Vec<u8>> = old_entries.keys().chain(new_entries.keys()).collect();

    for name in names {
        let before = old_entries.get(name);
        let after = new_entries.get(name);

        // Git filenames are byte strings; only display needs them as text.
        let display = String::from_utf8_lossy(name);
        let path = if prefix.is_empty() {
            display.into_owned()
        } else {
            format!("{prefix}/{display}")
        };

        match (before, after) {
            (Some(a), Some(b)) if a.oid == b.oid && a.mode == b.mode => {}

            (Some(a), Some(b)) if a.is_dir() && b.is_dir() => {
                diff_trees(reader, Some(a.oid), Some(b.oid), &path, paths, out)?;
            }

            // A file replaced by a directory, or the reverse, reads as the old
            // path going away and the new one arriving.
            (Some(a), Some(b)) if a.is_dir() != b.is_dir() => {
                collect_files(reader, a, &path, paths, out, ChangeKind::Deleted)?;
                collect_files(reader, b, &path, paths, out, ChangeKind::Added)?;
            }

            (Some(_), Some(b)) => {
                if !b.is_submodule() {
                    out.push(Change {
                        path: paths.intern(&path),
                        kind: ChangeKind::Modified,
                        content: b.oid,
                    });
                }
            }

            (None, Some(b)) => collect_files(reader, b, &path, paths, out, ChangeKind::Added)?,
            (Some(a), None) => collect_files(reader, a, &path, paths, out, ChangeKind::Deleted)?,
            (None, None) => unreachable!("a name came from one of the two trees"),
        }
    }

    Ok(())
}

/// Record every file at or below an entry, used when a whole subtree appears
/// or disappears in one commit.
fn collect_files(
    reader: &mut Reader<'_>,
    entry: &TreeEntry,
    path: &str,
    paths: &mut Interner,
    out: &mut Vec<Change>,
    kind: ChangeKind,
) -> Result<()> {
    // A submodule's contents live in another repository and can never be read
    // from here, so it counts as one entry rather than a tree.
    if entry.is_submodule() {
        return Ok(());
    }

    if !entry.is_dir() {
        out.push(Change {
            path: paths.intern(path),
            kind,
            content: entry.oid,
        });
        return Ok(());
    }

    let Ok(tree) = reader.tree(entry.oid) else {
        return Ok(());
    };

    for child in tree.entries {
        let name = String::from_utf8_lossy(&child.name);
        let child_path = format!("{path}/{name}");
        collect_files(reader, &child, &child_path, paths, out, kind)?;
    }

    Ok(())
}

fn entries_by_name(reader: &mut Reader<'_>, oid: Oid) -> Result<BTreeMap<Vec<u8>, TreeEntry>> {
    let tree = reader.tree(oid)?;
    Ok(tree
        .entries
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect())
}

fn intern_author(interner: &mut Interner, signature: &Signature) -> u32 {
    let key = signature.identity_key();
    let index = interner.intern(&key);
    interner.label(index, &signature.name);
    index
}

/// Maps strings to small integers, keeping one copy of each.
struct Interner {
    values: Vec<String>,
    labels: Vec<String>,
    lookup: HashMap<String, u32>,
}

impl Interner {
    fn new() -> Interner {
        Interner {
            values: Vec::new(),
            labels: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    fn intern(&mut self, value: &str) -> u32 {
        if let Some(&index) = self.lookup.get(value) {
            return index;
        }
        let index = self.values.len() as u32;
        self.values.push(value.to_string());
        self.labels.push(String::new());
        self.lookup.insert(value.to_string(), index);
        index
    }

    /// Attach a display name to an interned key. The first non-empty one wins,
    /// so an author is shown under the name they used earliest.
    fn label(&mut self, index: u32, label: &str) {
        let slot = &mut self.labels[index as usize];
        if slot.is_empty() && !label.is_empty() {
            *slot = label.to_string();
        }
    }

    fn into_values(self) -> Vec<String> {
        self.values
    }

    fn into_authors(self) -> Vec<Author> {
        self.values
            .into_iter()
            .zip(self.labels)
            .map(|(email, name)| Author {
                name: if name.is_empty() { email.clone() } else { name },
                email,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> Oid {
        Oid::from_bytes(&[byte; Oid::LEN]).expect("twenty bytes")
    }

    fn change(path: u32, kind: ChangeKind, content: u8) -> Change {
        Change {
            path,
            kind,
            content: oid(content),
        }
    }

    #[test]
    fn pairs_a_deletion_with_an_addition_of_the_same_content() {
        let mut changes = vec![
            change(0, ChangeKind::Deleted, 1),
            change(1, ChangeKind::Added, 1),
        ];
        detect_renames(&mut changes);

        assert_eq!(changes.len(), 1, "the pair collapses to one entry");
        assert_eq!(changes[0].path, 1, "reported at the new path");
        assert_eq!(changes[0].kind, ChangeKind::Renamed);
    }

    #[test]
    fn leaves_unrelated_changes_alone() {
        let mut changes = vec![
            change(0, ChangeKind::Deleted, 1),
            change(1, ChangeKind::Added, 2),
            change(2, ChangeKind::Modified, 3),
        ];
        detect_renames(&mut changes);

        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].kind, ChangeKind::Deleted);
        assert_eq!(changes[1].kind, ChangeKind::Added);
        assert_eq!(changes[2].kind, ChangeKind::Modified);
    }

    #[test]
    fn one_deletion_feeds_only_one_rename() {
        // A blob deleted once and added at three paths is one rename and two
        // genuine additions - the file was copied, not moved three times.
        let mut changes = vec![
            change(0, ChangeKind::Deleted, 9),
            change(1, ChangeKind::Added, 9),
            change(2, ChangeKind::Added, 9),
            change(3, ChangeKind::Added, 9),
        ];
        detect_renames(&mut changes);

        let renamed = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Renamed)
            .count();
        let added = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Added)
            .count();
        assert_eq!(renamed, 1);
        assert_eq!(added, 2);
        assert_eq!(changes.len(), 3, "the deletion is consumed");
    }

    #[test]
    fn handles_no_deletions_without_scanning() {
        let mut changes = vec![
            change(0, ChangeKind::Added, 1),
            change(1, ChangeKind::Modified, 2),
        ];
        detect_renames(&mut changes);
        assert_eq!(changes.len(), 2);
    }
}
