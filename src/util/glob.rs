//! Glob matching for path filters.
//!
//! Replaces `globset` and `glob`. Supports the three wildcards people actually
//! type at a shell:
//!
//! - `?` matches one character, but never a `/`
//! - `*` matches any run of characters within a single path segment
//! - `**` matches across segments, so `src/**/mod.rs` reaches any depth
//!
//! One convenience follows git's pathspec behaviour: a pattern containing no
//! `/` at all is matched at any depth, so `*.rs` finds `src/git/pack.rs` rather
//! than only top-level files. That is what everyone means when they type it.
//!
//! No character classes and no brace expansion. Both are easy to add and
//! neither has come up; leaving them out keeps the matcher small enough to read
//! in one sitting.

/// A compiled pattern. Compiling is only a borrow, but the type documents the
/// "match this against many paths" shape and keeps the depth rule in one place.
pub struct Pattern {
    pattern: Vec<u8>,
    /// True when the pattern has no separator and so applies at any depth.
    any_depth: bool,
}

impl Pattern {
    pub fn new(pattern: &str) -> Pattern {
        Pattern {
            pattern: pattern.as_bytes().to_vec(),
            any_depth: !pattern.contains('/'),
        }
    }

    pub fn matches(&self, path: &str) -> bool {
        let path = path.as_bytes();

        if match_here(&self.pattern, path) {
            return true;
        }

        // A bare pattern also applies to each path's final segment.
        if self.any_depth
            && let Some(position) = path.iter().rposition(|&b| b == b'/')
        {
            return match_here(&self.pattern, &path[position + 1..]);
        }

        false
    }
}

/// Match a pattern against a path, backtracking on wildcards.
///
/// Worst-case behaviour on adversarial patterns like `a*a*a*a*b` is
/// exponential, the same trap a naive regex engine has. Path filters are typed
/// by hand and paths are short, so the simple recursion is the right trade -
/// but it is a trade, not an oversight.
fn match_here(pattern: &[u8], path: &[u8]) -> bool {
    let Some(&first) = pattern.first() else {
        return path.is_empty();
    };

    match first {
        b'*' if pattern.get(1) == Some(&b'*') => {
            // `**` crosses separators. `**/` should also match zero
            // directories, so `src/**/mod.rs` still finds `src/mod.rs`.
            let mut rest = &pattern[2..];
            if rest.first() == Some(&b'/') {
                if match_here(&rest[1..], path) {
                    return true;
                }
                rest = &rest[1..];
            }

            (0..=path.len()).any(|skip| match_here(rest, &path[skip..]))
        }

        b'*' => {
            // A single star stays inside one segment.
            let rest = &pattern[1..];
            let mut taken = 0;
            loop {
                if match_here(rest, &path[taken..]) {
                    return true;
                }
                if taken >= path.len() || path[taken] == b'/' {
                    return false;
                }
                taken += 1;
            }
        }

        b'?' => !path.is_empty() && path[0] != b'/' && match_here(&pattern[1..], &path[1..]),

        literal => !path.is_empty() && path[0] == literal && match_here(&pattern[1..], &path[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        Pattern::new(pattern).matches(path)
    }

    #[test]
    fn matches_literals_exactly() {
        assert!(matches("src/main.rs", "src/main.rs"));
        assert!(!matches("src/main.rs", "src/main.rs.bak"));
        assert!(!matches("src/main.rs", "other/src/main.rs"));
    }

    #[test]
    fn a_star_stays_within_one_segment() {
        assert!(matches("src/*.rs", "src/main.rs"));
        assert!(!matches("src/*.rs", "src/git/pack.rs"), "must not cross /");
        assert!(matches("src/*/pack.rs", "src/git/pack.rs"));
    }

    #[test]
    fn a_double_star_crosses_segments() {
        assert!(matches("src/**/pack.rs", "src/git/pack.rs"));
        assert!(matches("src/**/pack.rs", "src/a/b/c/pack.rs"));
        // Zero directories is still a match, which is the case people forget.
        assert!(matches("src/**/pack.rs", "src/pack.rs"));
        assert!(matches("**", "anything/at/all"));
    }

    #[test]
    fn a_question_mark_takes_one_character() {
        assert!(matches("src/mai?.rs", "src/main.rs"));
        assert!(!matches("src/mai?.rs", "src/mainn.rs"));
        assert!(!matches("a?b", "a/b"), "must not match a separator");
    }

    #[test]
    fn a_bare_pattern_applies_at_any_depth() {
        // Git's pathspec behaviour, and what everyone means by it.
        assert!(matches("*.rs", "main.rs"));
        assert!(matches("*.rs", "src/git/pack.rs"));
        assert!(matches("Cargo.toml", "Cargo.toml"));
        assert!(!matches("*.rs", "src/git/pack.py"));
    }

    #[test]
    fn a_rooted_pattern_does_not_float() {
        // Once a separator appears, the pattern is anchored at the root.
        assert!(matches("src/*.rs", "src/main.rs"));
        assert!(!matches("src/*.rs", "vendor/src/main.rs"));
    }

    #[test]
    fn handles_empty_input() {
        assert!(matches("", ""));
        assert!(!matches("", "a"));
        assert!(matches("*", ""));
        assert!(!matches("a", ""));
    }
}
