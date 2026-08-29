//! Resolving HEAD and the ref namespace.
//!
//! Refs live in two places at once: as individual files under `refs/`, and
//! packed together in `packed-refs` after `git gc`. A ref present in both wins
//! from the loose file, because that is the one git wrote most recently.

use std::fs;
use std::path::Path;

use crate::git::error::Result;
use crate::git::oid::Oid;

/// How many symbolic refs to follow before assuming a cycle.
const MAX_SYMBOLIC_DEPTH: usize = 8;

/// Where HEAD points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// The usual case: HEAD names a branch that exists.
    Branch { name: String, target: Oid },
    /// A detached HEAD holds an object id directly.
    Detached(Oid),
    /// A repository with no commits yet: HEAD names a branch that has none.
    Unborn { name: String },
}

impl Head {
    pub fn target(&self) -> Option<Oid> {
        match self {
            Head::Branch { target, .. } => Some(*target),
            Head::Detached(oid) => Some(*oid),
            Head::Unborn { .. } => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Head::Branch { name, .. } => name.trim_start_matches("refs/heads/").to_string(),
            Head::Detached(oid) => format!("detached at {}", oid.short()),
            Head::Unborn { name } => {
                format!("{} (no commits yet)", name.trim_start_matches("refs/heads/"))
            }
        }
    }
}

/// Read HEAD, following symbolic refs to an object id.
pub fn head(git_dir: &Path) -> Result<Head> {
    let raw = match fs::read_to_string(git_dir.join("HEAD")) {
        Ok(text) => text,
        // A repository with no HEAD is not one we can walk, but it is also not
        // a crash: report it as unborn and let the caller say something useful.
        Err(_) => {
            return Ok(Head::Unborn {
                name: "HEAD".to_string(),
            });
        }
    };
    let raw = raw.trim();

    let Some(mut name) = raw.strip_prefix("ref:").map(str::trim) else {
        // Not symbolic, so HEAD holds an object id directly.
        return Ok(match Oid::parse_hex(raw.as_bytes()) {
            Some(oid) => Head::Detached(oid),
            None => Head::Unborn {
                name: raw.to_string(),
            },
        });
    };

    let mut owned;
    for _ in 0..MAX_SYMBOLIC_DEPTH {
        match resolve(git_dir, name)? {
            Some(Target::Object(target)) => {
                return Ok(Head::Branch {
                    name: name.to_string(),
                    target,
                });
            }
            Some(Target::Symbolic(next)) => {
                owned = next;
                name = &owned;
            }
            None => {
                return Ok(Head::Unborn {
                    name: name.to_string(),
                });
            }
        }
    }

    Ok(Head::Unborn {
        name: name.to_string(),
    })
}

enum Target {
    Object(Oid),
    Symbolic(String),
}

/// Look a ref up by full name, checking loose storage before `packed-refs`.
fn resolve(git_dir: &Path, name: &str) -> Result<Option<Target>> {
    // Refuse to walk outside the repository, in case a ref file has been
    // hand-edited to something like `ref: ../../etc/passwd`.
    if name.contains("..") || Path::new(name).is_absolute() {
        return Ok(None);
    }

    if let Ok(text) = fs::read_to_string(git_dir.join(name)) {
        let text = text.trim();
        return Ok(match text.strip_prefix("ref:") {
            Some(next) => Some(Target::Symbolic(next.trim().to_string())),
            None => Oid::parse_hex(text.as_bytes()).map(Target::Object),
        });
    }

    Ok(packed(git_dir)?
        .into_iter()
        .find(|(refname, _)| refname == name)
        .map(|(_, oid)| Target::Object(oid)))
}

/// Every ref in the repository, loose and packed, sorted by name.
pub fn all(git_dir: &Path) -> Result<Vec<(String, Oid)>> {
    let mut refs = packed(git_dir)?;

    // Loose refs override packed ones, so collect them second and overwrite.
    let refs_dir = git_dir.join("refs");
    let mut loose = Vec::new();
    collect_loose(&refs_dir, &refs_dir, &mut loose);
    for (name, oid) in loose {
        match refs.iter_mut().find(|(existing, _)| *existing == name) {
            Some(slot) => slot.1 = oid,
            None => refs.push((name, oid)),
        }
    }

    refs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(refs)
}

fn collect_loose(root: &Path, dir: &Path, out: &mut Vec<(String, Oid)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_loose(root, &path, out);
            continue;
        }

        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(oid) = Oid::parse_hex(text.trim().as_bytes()) else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };

        // Ref names always use forward slashes, whatever the platform.
        let name = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        out.push((format!("refs/{name}"), oid));
    }
}

/// Parse `packed-refs`: one `<oid> <name>` per line, with `^<oid>` peel lines
/// after annotated tags that are skipped here.
fn packed(git_dir: &Path) -> Result<Vec<(String, Oid)>> {
    let Ok(text) = fs::read_to_string(git_dir.join("packed-refs")) else {
        return Ok(Vec::new());
    };

    let mut refs = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with('^') || line.trim().is_empty() {
            continue;
        }
        let Some((oid, name)) = line.split_once(' ') else {
            continue;
        };
        if let Some(oid) = Oid::parse_hex(oid.trim().as_bytes()) {
            refs.push((name.trim().to_string(), oid));
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_describes_each_state() {
        let oid = Oid::parse_hex(b"4b825dc642cb6eb9a060e54bf8d69288fbee4904").unwrap();
        assert_eq!(
            Head::Branch {
                name: "refs/heads/main".into(),
                target: oid
            }
            .describe(),
            "main"
        );
        assert_eq!(Head::Detached(oid).describe(), "detached at 4b825dc");
        assert_eq!(
            Head::Unborn {
                name: "refs/heads/main".into()
            }
            .describe(),
            "main (no commits yet)"
        );
    }

    #[test]
    fn unborn_head_has_no_target() {
        assert!(
            Head::Unborn {
                name: "refs/heads/main".into()
            }
            .target()
            .is_none()
        );
    }
}
