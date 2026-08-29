//! The files that exist right now.
//!
//! History says how often a file changed; a hotspot needs to know how big it is
//! today. A file with two hundred revisions and eight lines is a changelog, not
//! a problem. This module walks the tree at HEAD once and measures what is
//! actually there.

use std::collections::HashMap;

use crate::git::error::Result;
use crate::git::oid::Oid;
use crate::git::repo::{Reader, Repository};

/// How much of a blob to inspect before deciding whether it is text. Git uses
/// the same trick: a NUL byte early on means binary.
const BINARY_SNIFF_BYTES: usize = 8000;

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub path: String,
    pub bytes: usize,
    /// Newline-separated lines, or zero for a binary file.
    pub lines: usize,
    pub is_binary: bool,
}

/// Every file reachable from a commit's tree, measured.
pub struct Snapshot {
    pub files: Vec<FileInfo>,
    by_path: HashMap<String, usize>,
}

impl Snapshot {
    pub fn get(&self, path: &str) -> Option<&FileInfo> {
        self.by_path.get(path).map(|&i| &self.files[i])
    }

    /// True when the path still exists. Files deleted long ago keep appearing
    /// in history, and listing them as hotspots to refactor helps nobody.
    pub fn contains(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }

    pub fn total_lines(&self) -> usize {
        self.files.iter().map(|f| f.lines).sum()
    }

    /// Keep only the files a predicate accepts, rebuilding the lookup index.
    ///
    /// The index holds positions, so dropping files without rebuilding it would
    /// leave every later path pointing at the wrong entry.
    pub fn retain(&mut self, keep: impl Fn(&FileInfo) -> bool) {
        self.files.retain(&keep);
        self.by_path = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.clone(), index))
            .collect();
    }
}

/// Measure every file in the tree of `commit`.
pub fn take(repo: &Repository, commit: Oid) -> Result<Snapshot> {
    let mut reader = repo.reader()?;
    let tree = reader.commit(commit)?.tree;

    let mut files = Vec::new();
    collect(&mut reader, tree, "", &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let by_path = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.path.clone(), index))
        .collect();

    Ok(Snapshot { files, by_path })
}

fn collect(
    reader: &mut Reader<'_>,
    tree: Oid,
    prefix: &str,
    out: &mut Vec<FileInfo>,
) -> Result<()> {
    let tree = reader.tree(tree)?;

    for entry in tree.entries {
        // A submodule's contents live in another repository entirely.
        if entry.is_submodule() {
            continue;
        }

        let name = String::from_utf8_lossy(&entry.name);
        let path = if prefix.is_empty() {
            name.into_owned()
        } else {
            format!("{prefix}/{name}")
        };

        if entry.is_dir() {
            collect(reader, entry.oid, &path, out)?;
            continue;
        }

        // An unreadable blob should not sink the whole report; record it as
        // empty and move on.
        let Ok(object) = reader.object(entry.oid) else {
            continue;
        };

        out.push(measure(path, &object.data));
    }

    Ok(())
}

fn measure(path: String, data: &[u8]) -> FileInfo {
    let head = &data[..data.len().min(BINARY_SNIFF_BYTES)];
    let is_binary = head.contains(&0);

    // Count newlines, then add a line for trailing content with no final
    // newline. An empty file has no lines at all.
    let lines = if is_binary || data.is_empty() {
        0
    } else {
        let newlines = data.iter().filter(|&&b| b == b'\n').count();
        if data.last() == Some(&b'\n') {
            newlines
        } else {
            newlines + 1
        }
    };

    FileInfo {
        path,
        bytes: data.len(),
        lines,
        is_binary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines_with_and_without_a_trailing_newline() {
        assert_eq!(measure("a".into(), b"one\ntwo\n").lines, 2);
        assert_eq!(measure("a".into(), b"one\ntwo").lines, 2);
        assert_eq!(measure("a".into(), b"one").lines, 1);
        assert_eq!(measure("a".into(), b"\n").lines, 1);
    }

    #[test]
    fn an_empty_file_has_no_lines() {
        let file = measure("a".into(), b"");
        assert_eq!(file.lines, 0);
        assert_eq!(file.bytes, 0);
        assert!(!file.is_binary);
    }

    #[test]
    fn detects_binary_content() {
        let file = measure("a.png".into(), b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR");
        assert!(file.is_binary);
        assert_eq!(file.lines, 0, "binary files are not line-counted");
        assert!(file.bytes > 0, "but their size is still known");
    }

    #[test]
    fn a_nul_beyond_the_sniff_window_does_not_flip_the_verdict() {
        // Matching git: only the start of the file is inspected, so a text file
        // with a stray NUL deep inside still counts as text.
        let mut data = vec![b'x'; BINARY_SNIFF_BYTES + 100];
        data.push(0);
        assert!(!measure("a".into(), &data).is_binary);
    }
}
