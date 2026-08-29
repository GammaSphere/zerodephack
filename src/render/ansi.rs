//! Terminal colour.
//!
//! Replaces `colored` and `owo-colors`. ANSI SGR escapes are a handful of bytes
//! and the standard library has had `IsTerminal` since 1.70, so the only real
//! work is deciding *whether* to colour.
//!
//! The rules, in the order they are checked:
//!
//! 1. `--no-color` on the command line always wins.
//! 2. `NO_COLOR` set to anything non-empty disables colour (no-color.org).
//! 3. `CLICOLOR_FORCE` set to anything but `0` forces it on, even in a pipe.
//! 4. Otherwise colour is on only when stdout is a terminal.
//!
//! Getting the pipe case right is what stops `strata hotspots > report.txt`
//! from filling the file with escape sequences.

use std::env;
use std::io::IsTerminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Dim,
    Bold,
    Red,
    Yellow,
    Green,
    Cyan,
    /// For the header row of a table.
    Header,
}

impl Style {
    fn code(self) -> &'static str {
        match self {
            Style::Dim => "2",
            Style::Bold => "1",
            Style::Red => "31",
            Style::Yellow => "33",
            Style::Green => "32",
            Style::Cyan => "36",
            Style::Header => "1;4",
        }
    }
}

/// Whether output should carry escape sequences.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    enabled: bool,
}

impl Palette {
    /// Decide from the environment and the `--no-color` flag.
    pub fn detect(forced_off: bool) -> Palette {
        if forced_off {
            return Palette { enabled: false };
        }

        if env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Palette { enabled: false };
        }

        if env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
            return Palette { enabled: true };
        }

        Palette {
            enabled: std::io::stdout().is_terminal(),
        }
    }

    pub fn plain() -> Palette {
        Palette { enabled: false }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Wrap `text` in an escape sequence, or return it untouched.
    ///
    /// Takes ownership of nothing: the returned String is built either way, so
    /// callers can format uniformly without branching on colour support.
    pub fn paint(&self, style: Style, text: &str) -> String {
        if !self.enabled {
            return text.to_string();
        }
        format!("\x1b[{}m{text}\x1b[0m", style.code())
    }

    /// Colour a number by how alarming it is, low to high.
    pub fn severity(&self, text: &str, level: f64) -> String {
        let style = match level {
            l if l >= 0.66 => Style::Red,
            l if l >= 0.33 => Style::Yellow,
            _ => Style::Green,
        };
        self.paint(style, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_palette_returns_text_unchanged() {
        let palette = Palette::plain();
        assert_eq!(palette.paint(Style::Red, "danger"), "danger");
        assert!(!palette.is_enabled());
    }

    #[test]
    fn an_enabled_palette_wraps_and_resets() {
        let palette = Palette { enabled: true };
        assert_eq!(palette.paint(Style::Red, "x"), "\x1b[31mx\x1b[0m");
        assert_eq!(palette.paint(Style::Header, "x"), "\x1b[1;4mx\x1b[0m");
    }

    #[test]
    fn no_color_flag_overrides_everything() {
        // The flag is checked before the environment, so this holds whatever
        // CLICOLOR_FORCE says on the machine running the tests.
        assert!(!Palette::detect(true).is_enabled());
    }

    #[test]
    fn severity_escalates_with_the_level() {
        let palette = Palette { enabled: true };
        assert!(palette.severity("x", 0.9).contains("31"), "high is red");
        assert!(
            palette.severity("x", 0.5).contains("33"),
            "middling is yellow"
        );
        assert!(palette.severity("x", 0.1).contains("32"), "low is green");
    }
}
