//! Parsing the four git object types.
//!
//! Objects are stored as `<type> <size>\0<payload>`, compressed. This module
//! takes the decompressed bytes and gives them structure.
//!
//! Paths inside trees are byte strings, not text. Git makes no guarantee they
//! are UTF-8, and repositories with latin-1 or shift-jis filenames are real, so
//! [`TreeEntry::name`] stays as bytes and only becomes a `String` at the point
//! something is printed.

use std::fmt;

use crate::git::error::{Error, Result};
use crate::git::oid::Oid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl Kind {
    pub fn from_bytes(text: &[u8]) -> Option<Self> {
        match text {
            b"commit" => Some(Kind::Commit),
            b"tree" => Some(Kind::Tree),
            b"blob" => Some(Kind::Blob),
            b"tag" => Some(Kind::Tag),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Commit => "commit",
            Kind::Tree => "tree",
            Kind::Blob => "blob",
            Kind::Tag => "tag",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A decompressed object with its header removed.
#[derive(Debug, Clone)]
pub struct Object {
    pub kind: Kind,
    pub data: Vec<u8>,
}

impl Object {
    /// Split the `<type> <size>\0` header off a decompressed loose object and
    /// check that the recorded size matches what actually follows.
    pub fn parse_loose(bytes: &[u8]) -> Result<Object> {
        let nul = bytes
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::malformed("object header", 0, "no NUL terminator"))?;

        let header = &bytes[..nul];
        let space = header.iter().position(|&b| b == b' ').ok_or_else(|| {
            Error::malformed("object header", 0, "no space between type and size")
        })?;

        let kind = Kind::from_bytes(&header[..space]).ok_or_else(|| {
            Error::malformed(
                "object header",
                0,
                format!("unknown type {}", String::from_utf8_lossy(&header[..space])),
            )
        })?;

        let size: usize = std::str::from_utf8(&header[space + 1..])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                Error::malformed(
                    "object header",
                    space + 1,
                    format!(
                        "size {} is not a number",
                        String::from_utf8_lossy(&header[space + 1..])
                    ),
                )
            })?;

        let payload = &bytes[nul + 1..];
        if payload.len() != size {
            return Err(Error::malformed(
                "object header",
                nul + 1,
                format!("header claims {size} bytes, found {}", payload.len()),
            ));
        }

        Ok(Object {
            kind,
            data: payload.to_vec(),
        })
    }
}

/// Who did something, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// Seconds since the Unix epoch, UTC.
    pub time: i64,
    /// Minutes east of UTC, as recorded by the committer's machine.
    pub offset_minutes: i32,
}

impl Signature {
    /// Parse `Name <email> 1724928000 +0200`.
    ///
    /// Names routinely contain spaces, and occasionally angle brackets, so the
    /// email is located from the last `<` rather than the first.
    fn parse(line: &[u8]) -> Result<Signature> {
        let open = line
            .iter()
            .rposition(|&b| b == b'<')
            .ok_or_else(|| Error::malformed("signature", 0, "no email"))?;
        let close = line[open..]
            .iter()
            .position(|&b| b == b'>')
            .map(|p| p + open)
            .ok_or_else(|| Error::malformed("signature", open, "unterminated email"))?;

        let name = String::from_utf8_lossy(&line[..open]).trim().to_string();
        let email = String::from_utf8_lossy(&line[open + 1..close]).to_string();

        // What follows is " <epoch> <zone>". A missing timestamp is tolerated:
        // some imported histories omit it, and losing one commit's date is
        // better than refusing to read the repository.
        let rest = String::from_utf8_lossy(&line[close + 1..]);
        let mut fields = rest.split_whitespace();
        let time = fields.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let offset_minutes = fields.next().and_then(parse_zone).unwrap_or(0);

        Ok(Signature {
            name,
            email,
            time,
            offset_minutes,
        })
    }

    /// The identity used for attribution. Email is the stable key: people
    /// change how they spell their name far more often than their address.
    pub fn identity_key(&self) -> String {
        self.email.to_lowercase()
    }
}

/// Parse `+0200` or `-0730` into minutes east of UTC.
fn parse_zone(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i32 = text[1..3].parse().ok()?;
    let minutes: i32 = text[3..5].parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

#[derive(Debug, Clone)]
pub struct Commit {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    pub author: Signature,
    pub committer: Signature,
    /// First line of the message, which is all any of the reports show.
    pub summary: String,
}

impl Commit {
    pub fn parse(data: &[u8]) -> Result<Commit> {
        let mut tree = None;
        let mut parents = Vec::new();
        let mut author = None;
        let mut committer = None;

        let mut offset = 0;
        loop {
            let line_end = match find(&data[offset..], b'\n') {
                Some(p) => offset + p,
                None => {
                    return Err(Error::malformed(
                        "commit",
                        offset,
                        "headers are not terminated",
                    ));
                }
            };

            // A blank line ends the headers; everything after is the message.
            if line_end == offset {
                offset += 1;
                break;
            }

            let line = &data[offset..line_end];
            offset = line_end + 1;

            // Header values may continue onto following lines, which begin with
            // a space. `gpgsig` does this for every signature block, so skipping
            // continuations is required, not optional.
            if line.first() == Some(&b' ') {
                continue;
            }

            let Some(space) = find(line, b' ') else {
                continue;
            };
            let (key, value) = (&line[..space], &line[space + 1..]);

            match key {
                b"tree" => tree = Oid::parse_hex(value),
                b"parent" => {
                    if let Some(oid) = Oid::parse_hex(value) {
                        parents.push(oid);
                    }
                }
                b"author" => author = Some(Signature::parse(value)?),
                b"committer" => committer = Some(Signature::parse(value)?),
                _ => {}
            }
        }

        let summary = data[offset..]
            .split(|&b| b == b'\n')
            .map(|line| String::from_utf8_lossy(line).trim().to_string())
            .find(|line| !line.is_empty())
            .unwrap_or_default();

        let author = author.ok_or_else(|| Error::malformed("commit", 0, "no author header"))?;
        // A commit without a committer is malformed, but the author is a fine
        // stand-in and refusing to read history over it helps nobody.
        let committer = committer.unwrap_or_else(|| author.clone());

        Ok(Commit {
            tree: tree.ok_or_else(|| Error::malformed("commit", 0, "no tree header"))?,
            parents,
            author,
            committer,
            summary,
        })
    }
}

/// One entry in a tree: a file, a subdirectory, a symlink or a submodule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Octal mode. 0o40000 is a directory, 0o160000 a submodule.
    pub mode: u32,
    /// Raw bytes, because git filenames are not guaranteed to be UTF-8.
    pub name: Vec<u8>,
    pub oid: Oid,
}

impl TreeEntry {
    pub fn is_dir(&self) -> bool {
        self.mode == 0o040000
    }

    /// Submodules appear as tree entries whose object lives in another
    /// repository, so their contents can never be read from here.
    pub fn is_submodule(&self) -> bool {
        self.mode == 0o160000
    }
}

#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// Parse a run of `<octal mode> <name>\0<20-byte oid>` records.
    pub fn parse(data: &[u8]) -> Result<Tree> {
        let mut entries = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let start = offset;
            let space = find(&data[offset..], b' ')
                .map(|p| p + offset)
                .ok_or_else(|| Error::malformed("tree", start, "entry has no mode separator"))?;

            let mode = std::str::from_utf8(&data[offset..space])
                .ok()
                .and_then(|s| u32::from_str_radix(s, 8).ok())
                .ok_or_else(|| {
                    Error::malformed(
                        "tree",
                        start,
                        format!(
                            "mode {} is not octal",
                            String::from_utf8_lossy(&data[offset..space])
                        ),
                    )
                })?;

            let nul = find(&data[space + 1..], 0)
                .map(|p| p + space + 1)
                .ok_or_else(|| Error::malformed("tree", start, "entry name is not terminated"))?;
            let name = data[space + 1..nul].to_vec();

            let oid_start = nul + 1;
            let oid_end = oid_start + Oid::LEN;
            let oid = data
                .get(oid_start..oid_end)
                .and_then(Oid::from_bytes)
                .ok_or_else(|| {
                    Error::malformed("tree", oid_start, "entry is truncated before its object id")
                })?;

            entries.push(TreeEntry { mode, name, oid });
            offset = oid_end;
        }

        Ok(Tree { entries })
    }
}

fn find(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(hex: &str) -> Oid {
        Oid::parse_hex(hex.as_bytes()).expect("valid oid")
    }

    #[test]
    fn parses_a_loose_header() {
        let object = Object::parse_loose(b"blob 5\0hello").expect("valid object");
        assert_eq!(object.kind, Kind::Blob);
        assert_eq!(object.data, b"hello");
    }

    #[test]
    fn rejects_a_header_whose_size_lies() {
        let err = Object::parse_loose(b"blob 99\0hello").expect_err("size must be checked");
        assert!(err.to_string().contains("claims 99 bytes"), "{err}");
    }

    #[test]
    fn rejects_an_unknown_object_type() {
        assert!(Object::parse_loose(b"widget 5\0hello").is_err());
    }

    #[test]
    fn parses_a_commit_with_two_parents() {
        let raw = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
                    parent 1111111111111111111111111111111111111111\n\
                    parent 2222222222222222222222222222222222222222\n\
                    author Ada Lovelace <ada@example.com> 1724928000 +0200\n\
                    committer Ada Lovelace <ada@example.com> 1724928100 -0730\n\
                    \n\
                    Merge the thing\n\nWith a longer body.\n";

        let commit = Commit::parse(raw).expect("valid commit");
        assert_eq!(commit.tree, oid("4b825dc642cb6eb9a060e54bf8d69288fbee4904"));
        assert_eq!(commit.parents.len(), 2);
        assert_eq!(commit.author.name, "Ada Lovelace");
        assert_eq!(commit.author.email, "ada@example.com");
        assert_eq!(commit.author.time, 1724928000);
        assert_eq!(commit.author.offset_minutes, 120);
        assert_eq!(commit.committer.offset_minutes, -450);
        assert_eq!(commit.summary, "Merge the thing");
    }

    #[test]
    fn skips_multi_line_headers() {
        // gpgsig continuation lines begin with a space and must not be mistaken
        // for headers, or the committer line after them is never seen.
        let raw = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
                    author Ada <ada@example.com> 1724928000 +0000\n\
                    gpgsig -----BEGIN PGP SIGNATURE-----\n\
                    \x20\n\
                    \x20iQIzBAABCgAdFiEE\n\
                    \x20-----END PGP SIGNATURE-----\n\
                    committer Bob <bob@example.com> 1724928001 +0000\n\
                    \n\
                    Signed commit\n";

        let commit = Commit::parse(raw).expect("valid commit");
        assert_eq!(commit.committer.email, "bob@example.com");
        assert_eq!(commit.summary, "Signed commit");
    }

    #[test]
    fn tolerates_an_empty_message() {
        let raw = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
                    author Ada <ada@example.com> 1724928000 +0000\n\
                    committer Ada <ada@example.com> 1724928000 +0000\n\
                    \n";
        let commit = Commit::parse(raw).expect("valid commit");
        assert_eq!(commit.summary, "");
        assert!(commit.parents.is_empty());
    }

    #[test]
    fn parses_a_tree_including_non_utf8_names() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"100644 README.md\0");
        raw.extend_from_slice(oid("1111111111111111111111111111111111111111").as_bytes());
        raw.extend_from_slice(b"40000 src\0");
        raw.extend_from_slice(oid("2222222222222222222222222222222222222222").as_bytes());
        // A latin-1 filename, which is legal in git and not valid UTF-8.
        raw.extend_from_slice(b"100644 caf\xe9.txt\0");
        raw.extend_from_slice(oid("3333333333333333333333333333333333333333").as_bytes());

        let tree = Tree::parse(&raw).expect("valid tree");
        assert_eq!(tree.entries.len(), 3);
        assert_eq!(tree.entries[0].mode, 0o100644);
        assert!(tree.entries[1].is_dir());
        assert_eq!(tree.entries[2].name, b"caf\xe9.txt");
        assert!(String::from_utf8(tree.entries[2].name.clone()).is_err());
    }

    #[test]
    fn rejects_a_truncated_tree() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"100644 README.md\0");
        raw.extend_from_slice(&[0u8; 10]); // half an object id
        let err = Tree::parse(&raw).expect_err("truncation must be caught");
        assert!(err.to_string().contains("truncated"), "{err}");
    }

    #[test]
    fn empty_tree_is_valid() {
        assert!(Tree::parse(b"").expect("empty tree").entries.is_empty());
    }
}
