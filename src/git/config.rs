//! Just enough of the git config format to answer the questions strata asks.
//!
//! Replaces `rust-ini`. Git's config is INI with subsections
//! (`[remote "origin"]`), continuation-free values, `#` and `;` comments, and
//! keys that are case-insensitive while subsection names are not.
//!
//! This reader deliberately does not implement include directives, value
//! quoting escapes beyond the common ones, or type coercion. It exists to read
//! `extensions.objectformat` and `core.bare`, and it says so rather than
//! pretending to be a general parser.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// A flattened view: `section.key` or `section.subsection.key` to value.
#[derive(Debug, Default)]
pub struct Config {
    entries: HashMap<String, String>,
}

impl Config {
    /// Read a config file. A missing file is not an error - a repository
    /// without one simply has no overrides.
    pub fn read(path: &Path) -> Config {
        match fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(_) => Config::default(),
        }
    }

    pub fn parse(text: &str) -> Config {
        let mut entries = HashMap::new();
        let mut prefix = String::new();

        for line in text.lines() {
            let line = strip_comment(line).trim();
            if line.is_empty() {
                continue;
            }

            if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                prefix = parse_section_header(header);
                continue;
            }

            if prefix.is_empty() {
                continue;
            }

            // A key with no `=` is shorthand for a true boolean.
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), unquote(v.trim())),
                None => (line, "true".to_string()),
            };

            entries.insert(format!("{prefix}.{}", key.to_lowercase()), value);
        }

        Config { entries }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    pub fn bool(&self, key: &str) -> bool {
        matches!(
            self.get(key).map(str::to_lowercase).as_deref(),
            Some("true" | "yes" | "on" | "1")
        )
    }
}

/// `core` stays `core`; `remote "origin"` becomes `remote.origin`. Section names
/// fold to lowercase, subsection names keep their case, as git specifies.
fn parse_section_header(header: &str) -> String {
    match header.split_once(char::is_whitespace) {
        Some((section, subsection)) => {
            let subsection = subsection.trim().trim_matches('"');
            format!("{}.{}", section.trim().to_lowercase(), subsection)
        }
        None => header.trim().to_lowercase(),
    }
}

/// Remove a trailing comment, respecting quotes so a `#` inside a value stays.
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '#' | ';' if !in_quotes => return &line[..index],
            _ => {}
        }
    }
    line
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].replace("\\\"", "\"")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sections_and_subsections() {
        let config = Config::parse(
            r#"
[core]
    bare = false
    repositoryformatversion = 1
[remote "origin"]
    url = git@example.com:a/b.git
[extensions]
    objectFormat = sha256
"#,
        );

        assert_eq!(config.get("core.repositoryformatversion"), Some("1"));
        assert_eq!(
            config.get("remote.origin.url"),
            Some("git@example.com:a/b.git")
        );
        // Keys fold to lowercase, so the camelCase git writes still matches.
        assert_eq!(config.get("extensions.objectformat"), Some("sha256"));
        assert!(!config.bool("core.bare"));
    }

    #[test]
    fn handles_comments_and_bare_booleans() {
        let config = Config::parse(
            r#"
# a comment
[core] ; trailing
    bare        ; no value means true
    editor = "vim #1"
"#,
        );
        assert!(config.bool("core.bare"));
        // A `#` inside quotes is part of the value, not a comment.
        assert_eq!(config.get("core.editor"), Some("vim #1"));
    }

    #[test]
    fn missing_file_yields_empty_config() {
        let config = Config::read(Path::new("does/not/exist"));
        assert_eq!(config.get("core.bare"), None);
        assert!(!config.bool("core.bare"));
    }
}
