//! Writing JSON.
//!
//! Replaces `serde_json`, the most-installed crate on crates.io. Only the
//! writing half is here - strata reads no JSON - which is the smaller and more
//! forgiving direction, and the honest thing to say about the substitution.
//!
//! The escaping is the part worth getting right. Rust strings are already valid
//! UTF-8 and JSON accepts UTF-8 directly, so multi-byte characters pass through
//! untouched; what must be escaped is the quote, the backslash, and every
//! control character below U+0020. Anything less and a commit message with a
//! newline or a tab in it produces output no parser will accept.

use std::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    /// Insertion-ordered, because a report reads better with its fields in a
    /// deliberate order than sorted alphabetically.
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn string(text: impl Into<String>) -> Value {
        Value::Str(text.into())
    }

    pub fn object(fields: Vec<(&str, Value)>) -> Value {
        Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }
}

impl From<usize> for Value {
    fn from(n: usize) -> Value {
        Value::Int(n as i64)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Value {
        Value::Int(n)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Value {
        Value::Float(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::Str(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::Str(s)
    }
}

/// Serialise with two-space indentation.
pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out.push('\n');
    out
}

fn write_value(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(n) => write_float(out, *n),
        Value::Str(s) => write_string(out, s),

        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                indent(out, depth + 1);
                write_value(out, item, depth + 1);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push(']');
        }

        Value::Object(fields) if fields.is_empty() => out.push_str("{}"),
        Value::Object(fields) => {
            out.push_str("{\n");
            for (index, (key, item)) in fields.iter().enumerate() {
                indent(out, depth + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, item, depth + 1);
                if index + 1 < fields.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

/// JSON has no NaN and no infinity, so those become null rather than output
/// that no parser will read back.
fn write_float(out: &mut String, value: f64) {
    if value.is_finite() {
        let _ = write!(out, "{value:.4}");
    } else {
        out.push_str("null");
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            // Everything else below a space needs the numeric form.
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn indent(out: &mut String, depth: usize) {
    out.push_str(&"  ".repeat(depth));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_scalars() {
        assert_eq!(to_string(&Value::Null), "null\n");
        assert_eq!(to_string(&Value::Bool(true)), "true\n");
        assert_eq!(to_string(&Value::Int(-42)), "-42\n");
        assert_eq!(to_string(&Value::Float(0.5)), "0.5000\n");
    }

    #[test]
    fn escapes_the_characters_that_break_parsers() {
        let value = Value::string("a \"quote\", a \\slash\\ and a\nnewline\there");
        let out = to_string(&value);
        assert!(out.contains(r#"\"quote\""#));
        assert!(out.contains(r"\\slash\\"));
        assert!(out.contains(r"\n"));
        assert!(out.contains(r"\t"));
        assert!(
            !out.contains('\n') || out.ends_with('\n'),
            "no raw newline inside the string"
        );
    }

    #[test]
    fn escapes_other_control_characters_numerically() {
        let out = to_string(&Value::string("bell\u{7}null\u{1f}"));
        assert!(out.contains(r"\u0007"), "{out}");
        assert!(out.contains(r"\u001f"), "{out}");
        // The raw control bytes must not survive into the output.
        assert!(!out.contains('\u{7}'), "raw BEL leaked through");
        assert!(!out.contains('\u{1f}'), "raw unit separator leaked through");
    }

    #[test]
    fn passes_multibyte_text_through_unescaped() {
        // JSON accepts UTF-8, and Rust strings are already valid UTF-8, so
        // there is nothing to do here except not mangle it.
        let out = to_string(&Value::string("日本語 café 🔥"));
        assert!(out.contains("日本語 café 🔥"), "{out}");
    }

    #[test]
    fn writes_nested_structures() {
        let value = Value::object(vec![
            ("name", Value::string("strata")),
            (
                "rows",
                Value::Array(vec![
                    Value::object(vec![("path", "a.rs".into()), ("n", 3usize.into())]),
                    Value::object(vec![("path", "b.rs".into()), ("n", 1usize.into())]),
                ]),
            ),
        ]);

        let out = to_string(&value);
        assert!(out.starts_with("{\n"));
        assert!(out.contains("  \"name\": \"strata\","));
        assert!(out.contains("      \"path\": \"a.rs\","));
        // Trailing commas are the classic hand-rolled-serialiser bug.
        assert!(!out.contains(",\n  }"), "trailing comma in object");
        assert!(!out.contains(",\n  ]"), "trailing comma in array");
    }

    #[test]
    fn empty_containers_stay_on_one_line() {
        assert_eq!(to_string(&Value::Array(vec![])), "[]\n");
        assert_eq!(to_string(&Value::Object(vec![])), "{}\n");
    }

    #[test]
    fn non_finite_floats_become_null() {
        assert_eq!(to_string(&Value::Float(f64::NAN)), "null\n");
        assert_eq!(to_string(&Value::Float(f64::INFINITY)), "null\n");
    }
}
