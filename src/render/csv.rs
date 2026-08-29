//! Writing CSV, to RFC 4180.
//!
//! Replaces the `csv` crate, writing only. The rule is short: a field needs
//! quoting when it contains a comma, a quote, or a line break, and a quote
//! inside a quoted field is written twice.
//!
//! Getting that wrong is how a commit summary containing a comma silently
//! shifts every column after it - the kind of corruption that is invisible
//! until someone opens the file in a spreadsheet and the numbers are in the
//! wrong place.

pub struct Writer {
    out: String,
}

impl Writer {
    pub fn new(headers: &[&str]) -> Writer {
        let mut writer = Writer { out: String::new() };
        writer.row(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>());
        writer
    }

    pub fn row(&mut self, fields: &[String]) {
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                self.out.push(',');
            }
            self.out.push_str(&escape(field));
        }
        // RFC 4180 specifies CRLF. Every tool that reads CSV also accepts LF,
        // and LF is what the rest of this program emits, so LF it is.
        self.out.push('\n');
    }

    pub fn finish(self) -> String {
        self.out
    }
}

fn escape(field: &str) -> String {
    let needs_quotes = field.contains([',', '"', '\n', '\r']);
    if !needs_quotes {
        return field.to_string();
    }
    format!("\"{}\"", field.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render one row and return it without the header.
    ///
    /// Splitting on lines would be wrong here: a correctly quoted field may
    /// itself contain a newline, which is the whole point of two of the tests
    /// below.
    fn row(fields: &[&str]) -> String {
        let mut writer = Writer::new(&["a"]);
        writer.row(&fields.iter().map(|f| f.to_string()).collect::<Vec<_>>());
        let out = writer.finish();
        out.strip_prefix("a\n")
            .expect("header is written first")
            .trim_end_matches('\n')
            .to_string()
    }

    #[test]
    fn plain_fields_are_written_bare() {
        assert_eq!(row(&["one", "two"]), "one,two");
    }

    #[test]
    fn commas_force_quoting() {
        assert_eq!(row(&["a,b", "c"]), "\"a,b\",c");
    }

    #[test]
    fn quotes_are_doubled_inside_a_quoted_field() {
        assert_eq!(row(&[r#"say "hi""#]), r#""say ""hi""""#);
    }

    #[test]
    fn newlines_force_quoting() {
        assert_eq!(row(&["line\nbreak"]), "\"line\nbreak\"");
        assert_eq!(row(&["carriage\rreturn"]), "\"carriage\rreturn\"");
    }

    #[test]
    fn writes_a_header_first() {
        let writer = Writer::new(&["path", "count"]);
        assert_eq!(writer.finish(), "path,count\n");
    }

    #[test]
    fn empty_fields_survive() {
        assert_eq!(row(&["", "x", ""]), ",x,");
    }
}
