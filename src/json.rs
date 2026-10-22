//! Writing JSON, for the commands that print for scripts.
//!
//! Written here rather than taken from `serde_json`: the viewer writes one
//! shape of document and reads none, and a serialisation framework for that
//! is a dependency bigger than the code it replaces.
//!
//! The output is ASCII whatever the data, with everything else escaped. That
//! is for Windows PowerShell 5.1, which decodes a program's output in the
//! console's code page rather than UTF-8: a file called `café.jpg` printed as
//! UTF-8 arrives in a script as `cafÃ©.jpg`, and `\u00e9` arrives intact.

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Written as an integer.
    Count(u64),
    /// Written as the shortest decimal that reads back as the same number;
    /// `null` when it is not finite, which JSON has no word for.
    Number(f64),
    Text(String),
    List(Vec<Value>),
    /// Fields in the order given, so a document reads the way it was laid
    /// out rather than alphabetically.
    Object(Vec<(&'static str, Value)>),
}

impl Value {
    /// `Text` from anything that is one, `Null` from `None`.
    pub fn text(value: Option<impl Into<String>>) -> Self {
        value.map_or(Value::Null, |value| Value::Text(value.into()))
    }

    pub fn number(value: Option<f64>) -> Self {
        value.map_or(Value::Null, Value::Number)
    }

    pub fn count(value: Option<u64>) -> Self {
        value.map_or(Value::Null, Value::Count)
    }

    /// The document, indented two spaces a level, ending in a newline.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        let indent = |out: &mut String, depth: usize| out.extend(std::iter::repeat_n("  ", depth));
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Value::Count(value) => out.push_str(&value.to_string()),
            Value::Number(value) if value.is_finite() => out.push_str(&value.to_string()),
            Value::Number(_) => out.push_str("null"),
            Value::Text(text) => quote(out, text),
            Value::List(items) if items.is_empty() => out.push_str("[]"),
            Value::List(items) => {
                out.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    indent(out, depth + 1);
                    item.write(out, depth + 1);
                    out.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
                }
                indent(out, depth);
                out.push(']');
            }
            Value::Object(fields) if fields.is_empty() => out.push_str("{}"),
            Value::Object(fields) => {
                out.push_str("{\n");
                for (index, (name, value)) in fields.iter().enumerate() {
                    indent(out, depth + 1);
                    quote(out, name);
                    out.push_str(": ");
                    value.write(out, depth + 1);
                    out.push_str(if index + 1 < fields.len() { ",\n" } else { "\n" });
                }
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

/// A string as JSON, in ASCII.
fn quote(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ' '..='~' => out.push(character),
            // Control characters and everything past ASCII, as UTF-16 code
            // units: a character outside the first plane takes two.
            _ => {
                let mut units = [0u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The escapes JSON requires, and the ASCII-only rule this adds: a name
    /// with an accent, a character outside the first plane, a quote, a
    /// backslash and a control character all come out as escapes.
    #[test]
    fn text_is_written_as_ascii_json() {
        let written = Value::Text("café \u{1F600} \"q\" C:\\x \u{1}".into()).render();
        assert_eq!(written, "\"caf\\u00e9 \\ud83d\\ude00 \\\"q\\\" C:\\\\x \\u0001\"\n");
        assert!(written.is_ascii());
    }

    #[test]
    fn numbers_are_written_as_json_numbers() {
        assert_eq!(Value::Number(0.008).render(), "0.008\n");
        assert_eq!(Value::Number(2.8).render(), "2.8\n");
        assert_eq!(Value::Count(4032).render(), "4032\n");
        assert_eq!(Value::Number(f64::NAN).render(), "null\n");
    }

    /// Nesting is indented, fields keep their order, and empty containers
    /// stay on one line.
    #[test]
    fn a_document_is_laid_out_in_the_order_given() {
        let document = Value::List(vec![Value::Object(vec![
            ("path", Value::Text("a.jpg".into())),
            ("width", Value::Count(3)),
            ("place", Value::Null),
            ("tags", Value::List(Vec::new())),
            ("hdr", Value::Bool(false)),
        ])]);
        assert_eq!(
            document.render(),
            "[\n  {\n    \"path\": \"a.jpg\",\n    \"width\": 3,\n    \"place\": null,\n    \"tags\": [],\n    \"hdr\": false\n  }\n]\n"
        );
    }
}
