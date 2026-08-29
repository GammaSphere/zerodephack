//! Column-aligned tables for a terminal.
//!
//! Replaces `comfy-table` and `tabled`. Widths are measured with
//! [`crate::util::width`], not by byte length, so a path containing CJK or an
//! author with an accented name does not push its column out of true.
//!
//! Cells may already carry ANSI escapes, so measurement strips them first -
//! which is `strip-ansi` in about fifteen lines.

use crate::render::ansi::{Palette, Style};
use crate::util::width::{pad_end, pad_start, str_width, truncate_start};

/// Width assumed when the terminal does not say. The standard library cannot
/// ask - there is no `ioctl` in `std` - so `COLUMNS` is the only hint
/// available, and most shells export it.
const FALLBACK_COLUMNS: usize = 100;
const COLUMN_GAP: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

pub struct Table {
    headers: Vec<String>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
    /// Index of the column that absorbs truncation when space runs short.
    flexible: Option<usize>,
}

impl Table {
    pub fn new(headers: &[&str], aligns: &[Align]) -> Table {
        assert_eq!(
            headers.len(),
            aligns.len(),
            "every column needs an alignment"
        );
        Table {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            aligns: aligns.to_vec(),
            rows: Vec::new(),
            flexible: None,
        }
    }

    /// Nominate the column to shrink first. Paths are the usual choice: they
    /// are the longest and the only ones that survive losing their left end.
    pub fn flexible_column(mut self, index: usize) -> Table {
        self.flexible = Some(index);
        self
    }

    pub fn push(&mut self, row: Vec<String>) {
        debug_assert_eq!(row.len(), self.headers.len(), "row shape must match header");
        self.rows.push(row);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn render(&self, palette: &Palette) -> String {
        if self.rows.is_empty() {
            return String::new();
        }

        let mut widths: Vec<usize> = self.headers.iter().map(|h| str_width(h)).collect();
        for row in &self.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(display_width(cell));
            }
        }

        // Give back what does not fit by squeezing the flexible column, never
        // below a width where the tail of a path is still readable.
        let available = terminal_columns();
        let total: usize = widths.iter().sum::<usize>() + COLUMN_GAP * (widths.len() - 1);
        if let Some(flex) = self.flexible
            && total > available
        {
            let overflow = total - available;
            widths[flex] = widths[flex].saturating_sub(overflow).max(12);
        }

        let mut out = String::new();

        let header: Vec<String> = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| align(h, widths[i], self.aligns[i]))
            .collect();
        out.push_str(&palette.paint(
            Style::Header,
            header.join(&" ".repeat(COLUMN_GAP)).trim_end(),
        ));
        out.push('\n');

        for row in &self.rows {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let fitted = if Some(i) == self.flexible {
                        truncate_start(cell, widths[i])
                    } else {
                        cell.clone()
                    };
                    align(&fitted, widths[i], self.aligns[i])
                })
                .collect();
            out.push_str(cells.join(&" ".repeat(COLUMN_GAP)).trim_end());
            out.push('\n');
        }

        out
    }
}

/// Pad to width, accounting for escape sequences that occupy no columns.
fn align(cell: &str, width: usize, alignment: Align) -> String {
    let visible = display_width(cell);
    let padding = width.saturating_sub(visible);

    match alignment {
        Align::Left => {
            if cell.contains('\x1b') {
                format!("{cell}{}", " ".repeat(padding))
            } else {
                pad_end(cell, width)
            }
        }
        Align::Right => {
            if cell.contains('\x1b') {
                format!("{}{cell}", " ".repeat(padding))
            } else {
                pad_start(cell, width)
            }
        }
    }
}

fn display_width(cell: &str) -> usize {
    if cell.contains('\x1b') {
        str_width(&strip_ansi(cell))
    } else {
        str_width(cell)
    }
}

/// Remove ANSI escape sequences. Replaces `strip-ansi`.
///
/// Handles CSI sequences (`ESC [` ... final byte in `@`..`~`), which is what
/// colour uses. Other escape forms are dropped up to the next alphabetic byte.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();

    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // Skip the introducer, then run to the sequence's final byte.
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(_) => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            None => break,
        }
    }

    out
}

/// Terminal width from `COLUMNS`, clamped to something sane.
fn terminal_columns() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&c| c >= 40)
        .unwrap_or(FALLBACK_COLUMNS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colour_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[1;4mbold\x1b[0m tail"), "bold tail");
        // A truncated escape at the end must not loop or panic.
        assert_eq!(strip_ansi("text\x1b"), "text");
    }

    #[test]
    fn coloured_cells_still_align() {
        let coloured = "\x1b[31m42\x1b[0m";
        assert_eq!(display_width(coloured), 2, "escapes occupy no columns");
        let aligned = align(coloured, 5, Align::Right);
        assert_eq!(display_width(&aligned), 5);
    }

    #[test]
    fn renders_a_header_and_rows() {
        let mut table = Table::new(&["file", "n"], &[Align::Left, Align::Right]);
        table.push(vec!["src/main.rs".into(), "7".into()]);
        table.push(vec!["README.md".into(), "12".into()]);

        let out = table.render(&Palette::plain());
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("file"));
        // The numeric column is right-aligned, so both values end at the same
        // column despite differing in length.
        assert!(lines[1].ends_with("7"));
        assert!(lines[2].ends_with("12"));
        assert_eq!(str_width(lines[1]), str_width(lines[2]));
    }

    #[test]
    fn an_empty_table_renders_nothing() {
        let table = Table::new(&["a"], &[Align::Left]);
        assert!(table.is_empty());
        assert_eq!(table.render(&Palette::plain()), "");
    }

    #[test]
    fn wide_characters_do_not_skew_a_column() {
        let mut table = Table::new(&["path", "n"], &[Align::Left, Align::Right]);
        table.push(vec!["docs/日本語.md".into(), "1".into()]);
        table.push(vec!["docs/readme.md".into(), "2".into()]);

        let out = table.render(&Palette::plain());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            str_width(lines[1]),
            str_width(lines[2]),
            "rows must occupy the same number of columns"
        );
    }
}
