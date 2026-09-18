//! Addressing and rendering one node of a JSON tree.
//!
//! The tree views identify a node by the steps taken from the root rather
//! than by a printed path, because a key may itself contain `.` or `[`; a
//! path string is produced only when someone asks to copy one.

use serde_json::Value;

/// One step from a JSON value to a child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Object member.
    Key(String),
    /// Array element.
    Index(usize),
}

/// Follow `path` from `root`.
pub fn resolve<'a>(root: &'a Value, path: &[Segment]) -> Option<&'a Value> {
    let mut current = root;
    for segment in path {
        current = match segment {
            Segment::Key(key) => current.get(key)?,
            Segment::Index(ix) => current.get(ix)?,
        };
    }
    Some(current)
}

/// `path` in the JSONPath-ish form the trees display: `$.tools[0].name`.
/// A key that is not a bare identifier is bracketed and quoted.
pub fn path_string(path: &[Segment]) -> String {
    let mut out = String::from("$");
    for segment in path {
        match segment {
            Segment::Key(key) if is_identifier(key) => {
                out.push('.');
                out.push_str(key);
            }
            Segment::Key(key) => {
                out.push_str("[\"");
                out.push_str(&key.replace('\\', "\\\\").replace('"', "\\\""));
                out.push_str("\"]");
            }
            Segment::Index(ix) => out.push_str(&format!("[{ix}]")),
        }
    }
    out
}

fn is_identifier(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Indented JSON, the shape a person reads.
pub fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Bytes `value` takes as compact JSON, the length `to_string` would have,
/// counted without building the string.
///
/// A debugger measures a result before deciding whether to draw it, so the
/// measurement must not cost what drawing would.
pub fn json_size(value: &Value) -> usize {
    json_head(value, 0).1
}

/// The first `limit` bytes of `value` as compact JSON, cut back to a
/// character boundary, and the bytes the whole of it takes.
///
/// One serialization answers both how large a payload is and what to keep of
/// it, so a log can bound what it holds without printing a message twice.
pub fn json_head(value: &Value, limit: usize) -> (String, usize) {
    let mut head = Head {
        kept: Vec::new(),
        limit,
        total: 0,
    };
    // A head never refuses a write, and a `Value` always serializes.
    let _ = serde_json::to_writer(&mut head, value);
    let mut kept = head.kept;
    // The cut may land inside a character; keep only the whole ones.
    if let Err(e) = std::str::from_utf8(&kept) {
        kept.truncate(e.valid_up_to());
    }
    (String::from_utf8(kept).unwrap_or_default(), head.total)
}

/// A writer that keeps the first `limit` bytes it is given and counts all.
struct Head {
    kept: Vec<u8>,
    limit: usize,
    total: usize,
}

impl std::io::Write for Head {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = self.limit.saturating_sub(self.kept.len());
        self.kept.extend_from_slice(&buf[..buf.len().min(room)]);
        self.total += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What copying a node should put on the clipboard.
///
/// A string copies as its text. Quoting it would mean deleting two
/// characters every time someone lifts an error message, a URI or a base64
/// blob out of a response, which is most of what copying from a debugger
/// is for. Everything else copies as indented JSON.
pub fn copy_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => pretty(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(k: &str) -> Segment {
        Segment::Key(k.to_owned())
    }

    #[test]
    fn resolves_keys_and_indices() {
        let root = json!({"tools": [{"name": "get_weather"}]});
        let path = vec![key("tools"), Segment::Index(0), key("name")];
        assert_eq!(resolve(&root, &path), Some(&json!("get_weather")));
        assert_eq!(resolve(&root, &[key("missing")]), None);
        assert_eq!(resolve(&root, &[key("tools"), Segment::Index(9)]), None);
        assert_eq!(resolve(&root, &[]), Some(&root));
    }

    #[test]
    fn a_key_containing_punctuation_still_resolves() {
        // The printed path is ambiguous; the segments are not.
        let root = json!({"a.b": {"c[0]": 1}});
        let path = vec![key("a.b"), key("c[0]")];
        assert_eq!(resolve(&root, &path), Some(&json!(1)));
        assert_eq!(path_string(&path), r#"$["a.b"]["c[0]"]"#);
    }

    #[test]
    fn paths_read_like_jsonpath() {
        assert_eq!(path_string(&[]), "$");
        assert_eq!(
            path_string(&[key("content"), Segment::Index(0), key("text")]),
            "$.content[0].text"
        );
        assert_eq!(path_string(&[key("mime-type")]), "$.mime-type");
        assert_eq!(path_string(&[key("2fa")]), r#"$["2fa"]"#);
        assert_eq!(path_string(&[key(r#"say "hi""#)]), r#"$["say \"hi\""]"#);
    }

    #[test]
    fn size_is_the_compact_length() {
        let values = [
            json!(null),
            json!({}),
            json!("18 °C, clear"),
            json!({"say": "\"hi\"\n\ttab \\ slash", "日本": ["東京", 1.5, -2, true]}),
            json!([{"a": [{"b": {"c": "\u{0001}"}}]}, "emoji 🌤", 12345678901234u64]),
        ];
        for value in values {
            assert_eq!(json_size(&value), value.to_string().len(), "{value}");
        }
    }

    #[test]
    fn a_head_keeps_whole_characters_within_its_limit() {
        let value = json!({"city": "東京", "sky": "🌤 clear", "n": [1, 2, 3]});
        let text = value.to_string();
        for limit in 0..=text.len() + 4 {
            let (head, total) = json_head(&value, limit);
            assert_eq!(total, text.len(), "the whole payload is counted");
            assert!(head.len() <= limit, "{limit}: {head}");
            assert!(text.starts_with(&head), "{limit}: {head}");
            // Only a character that would not fit whole is left out.
            let next = text[head.len()..].chars().next().map_or(0, char::len_utf8);
            assert!(head.len() == text.len() || head.len() + next > limit);
        }
        assert_eq!(json_head(&value, usize::MAX).0, text);
    }

    #[test]
    fn strings_copy_without_their_quotes() {
        assert_eq!(copy_text(&json!("18 °C, clear")), "18 °C, clear");
        assert_eq!(copy_text(&json!(42)), "42");
        assert_eq!(copy_text(&json!({"a": 1})), "{\n  \"a\": 1\n}");
    }
}
