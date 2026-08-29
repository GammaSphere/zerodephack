//! Presenting paths to people.

use std::path::Path;

/// Windows verbatim prefix. `fs::canonicalize` returns paths in this form, and
/// while every filesystem call accepts it, nobody wants to read it in an error
/// message.
const VERBATIM_PREFIX: &str = r"\\?\";
/// The UNC flavour, for network shares.
const VERBATIM_UNC_PREFIX: &str = r"\\?\UNC\";

/// A path as a person would write it.
///
/// Strips the Windows verbatim prefix for display only. The original path is
/// what gets used for filesystem calls, because the prefix is what makes paths
/// longer than 260 characters work.
pub fn display(path: &Path) -> String {
    let text = path.to_string_lossy();

    if let Some(rest) = text.strip_prefix(VERBATIM_UNC_PREFIX) {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(VERBATIM_PREFIX) {
        return rest.to_string();
    }

    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn strips_the_verbatim_prefix() {
        assert_eq!(
            display(&PathBuf::from(r"\\?\C:\projects\strata")),
            r"C:\projects\strata"
        );
    }

    #[test]
    fn restores_the_unc_form() {
        assert_eq!(
            display(&PathBuf::from(r"\\?\UNC\server\share\file")),
            r"\\server\share\file"
        );
    }

    #[test]
    fn leaves_ordinary_paths_alone() {
        assert_eq!(display(&PathBuf::from("/home/dev/strata")), "/home/dev/strata");
        assert_eq!(display(&PathBuf::from(r"C:\projects")), r"C:\projects");
        assert_eq!(display(&PathBuf::from("relative/path")), "relative/path");
    }
}
