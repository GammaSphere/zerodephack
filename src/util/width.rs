//! Display width of text in a terminal cell grid.
//!
//! Replaces `unicode-width`. `str::len` counts bytes and `chars().count()`
//! counts code points; a terminal cares about neither. A CJK ideograph occupies
//! two columns, a combining accent occupies none, and a table aligned on either
//! of the other two measures comes out visibly crooked.
//!
//! **This is a subset of UAX#11, not an implementation of it.** The real
//! standard is a large generated table that changes with each Unicode release.
//! What is here covers the ranges that appear in author names and file paths:
//! CJK, Hangul, fullwidth forms, the common emoji blocks, and the combining
//! marks. Outside those it assumes one column, which is right for Latin,
//! Greek, Cyrillic, Hebrew and Arabic.
//!
//! Emoji sequences joined by U+200D - a family, a flag, a profession - are
//! measured as the sum of their parts, so they over-count. Getting that right
//! needs grapheme cluster breaking, which is a bigger table still.

/// Columns a single character occupies.
pub fn char_width(c: char) -> usize {
    let code = c as u32;

    // Control characters have no width of their own; a terminal does something
    // else entirely with them.
    if code < 0x20 || (0x7f..0xa0).contains(&code) {
        return 0;
    }

    if is_zero_width(code) {
        return 0;
    }

    if is_wide(code) { 2 } else { 1 }
}

/// Columns a string occupies.
pub fn str_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Truncate to at most `columns`, appending an ellipsis when anything is cut.
///
/// Paths are the reason this exists: they are long, and the informative end is
/// the right-hand one, so the *start* is what gets dropped.
pub fn truncate_start(text: &str, columns: usize) -> String {
    if str_width(text) <= columns || columns == 0 {
        return text.to_string();
    }
    if columns == 1 {
        return "…".to_string();
    }

    // Keep taking characters from the right until the budget is spent.
    let budget = columns - 1;
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = char_width(c);
        if used + w > budget {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept.reverse();

    let mut out = String::from("…");
    out.extend(kept);
    out
}

/// Pad on the right to exactly `columns`, or return as-is when already wider.
pub fn pad_end(text: &str, columns: usize) -> String {
    let width = str_width(text);
    if width >= columns {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(columns - width))
}

/// Pad on the left to exactly `columns`, for numeric columns.
pub fn pad_start(text: &str, columns: usize) -> String {
    let width = str_width(text);
    if width >= columns {
        return text.to_string();
    }
    format!("{}{text}", " ".repeat(columns - width))
}

/// Combining marks and other characters that attach to the previous cell.
fn is_zero_width(code: u32) -> bool {
    matches!(code,
        0x0300..=0x036f   // combining diacritical marks
        | 0x0483..=0x0489
        | 0x0591..=0x05bd // Hebrew points
        | 0x0610..=0x061a // Arabic marks
        | 0x064b..=0x065f
        | 0x0670
        | 0x06d6..=0x06dc
        | 0x0e31 | 0x0e34..=0x0e3a | 0x0e47..=0x0e4e  // Thai
        | 0x1ab0..=0x1aff // combining diacriticals extended
        | 0x1dc0..=0x1dff // combining diacriticals supplement
        | 0x200b..=0x200f // zero width space through RTL mark
        | 0x2028..=0x202e // line/paragraph separators, bidi overrides
        | 0x20d0..=0x20f0 // combining marks for symbols
        | 0xfe00..=0xfe0f // variation selectors
        | 0xfe20..=0xfe2f // combining half marks
        | 0xfeff          // zero width no-break space
        | 0xe0100..=0xe01ef // variation selectors supplement
    )
}

/// East Asian Wide and Fullwidth, plus the emoji blocks that render double.
fn is_wide(code: u32) -> bool {
    matches!(code,
        0x1100..=0x115f   // Hangul Jamo initial consonants
        | 0x2e80..=0x303e // CJK radicals through CJK symbols
        | 0x3041..=0x33ff // Hiragana, Katakana, CJK compatibility
        | 0x3400..=0x4dbf // CJK extension A
        | 0x4e00..=0x9fff // CJK unified ideographs
        | 0xa000..=0xa4cf // Yi
        | 0xa960..=0xa97f // Hangul Jamo extended A
        | 0xac00..=0xd7a3 // Hangul syllables
        | 0xf900..=0xfaff // CJK compatibility ideographs
        | 0xfe10..=0xfe19 // vertical forms
        | 0xfe30..=0xfe6f // CJK compatibility forms
        | 0xff00..=0xff60 // fullwidth forms
        | 0xffe0..=0xffe6 // fullwidth signs
        | 0x1f300..=0x1f64f // symbols, pictographs, emoticons
        | 0x1f680..=0x1f6ff // transport and map symbols
        | 0x1f900..=0x1f9ff // supplemental symbols and pictographs
        | 0x1fa70..=0x1faff // symbols extended A
        | 0x20000..=0x2fffd // CJK extension B and beyond
        | 0x30000..=0x3fffd
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_column_each() {
        assert_eq!(str_width("hello"), 5);
        assert_eq!(str_width(""), 0);
        assert_eq!(str_width("src/git/pack.rs"), 15);
    }

    #[test]
    fn latin_accents_are_one_column() {
        // Precomposed: one code point, one column.
        assert_eq!(str_width("café"), 4);
        // Decomposed: two code points, still one column, because the combining
        // accent occupies no cell of its own. This is the case that makes
        // chars().count() wrong.
        assert_eq!(str_width("cafe\u{0301}"), 4);
    }

    #[test]
    fn cjk_is_two_columns_each() {
        assert_eq!(str_width("日本語"), 6);
        assert_eq!(str_width("中文"), 4);
        assert_eq!(str_width("한국어"), 6);
        // Mixed, which is what a real path looks like.
        assert_eq!(str_width("docs/日本語.md"), 14);
    }

    #[test]
    fn emoji_are_two_columns() {
        assert_eq!(str_width("🔥"), 2);
        assert_eq!(str_width("🚀 launch"), 9);
    }

    #[test]
    fn control_characters_have_no_width() {
        assert_eq!(str_width("a\u{0}b"), 2);
        assert_eq!(str_width("\u{200b}"), 0);
    }

    #[test]
    fn padding_reaches_the_requested_width() {
        assert_eq!(str_width(&pad_end("日本", 8)), 8);
        assert_eq!(str_width(&pad_start("日本", 8)), 8);
        assert_eq!(pad_end("abc", 2), "abc", "never truncates");
    }

    #[test]
    fn truncation_keeps_the_informative_end() {
        assert_eq!(truncate_start("src/git/pack.rs", 100), "src/git/pack.rs");
        let cut = truncate_start("src/analysis/history.rs", 12);
        assert!(cut.starts_with('…'));
        assert!(cut.ends_with("history.rs"), "kept the filename: {cut}");
        assert!(str_width(&cut) <= 12);
    }

    #[test]
    fn truncation_never_splits_a_wide_character() {
        // An odd budget cannot fit half an ideograph, so it must come in under.
        let cut = truncate_start("日本語テスト", 5);
        assert!(str_width(&cut) <= 5, "{cut} is {} wide", str_width(&cut));
    }
}
