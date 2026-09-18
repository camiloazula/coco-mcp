//! The server form's text and the values it stands for: the command line,
//! the working directory, and `KEY=value` lines for environment and headers.
//! Plain functions, so the form's round trip is tested without a window.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// A stored value and the text a form field was prefilled with to show it.
///
/// Text cannot show every value as it is: an input rewrites some characters,
/// and the quoting a line is shown with need not be the one it was typed
/// with. A field the user did not touch therefore gives back the stored
/// value, so saving an unedited server changes nothing.
#[derive(Debug, Clone)]
pub struct Kept<T> {
    text: String,
    value: T,
}

impl<T: Clone> Kept<T> {
    /// `value`, shown in its field as `text` (read back from the input, so
    /// any rewrite the input made is part of it).
    pub fn new(text: impl Into<String>, value: T) -> Self {
        Self {
            text: text.into(),
            value,
        }
    }
}

/// The kept value while `current` is still the prefilled text, otherwise
/// what `parse` makes of `current`.
pub fn resolve<T: Clone>(
    kept: Option<&Kept<T>>,
    current: &str,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    match kept {
        Some(kept) if kept.text == current => Ok(kept.value.clone()),
        _ => parse(current),
    }
}

/// The program and its arguments from a typed command line, split the way a
/// POSIX shell would split it.
pub fn parse_command(text: &str) -> Result<(String, Vec<String>), String> {
    let mut words = mcp_exchange::split_command_line(text)?.into_iter();
    let command = words.next().ok_or("Command is required")?;
    Ok((command, words.collect()))
}

/// A typed working directory; empty means the app's own.
pub fn parse_cwd(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// One `KEY<sep> value` line per entry, as a stored map is shown. Parsed
/// back by [`parse_pairs`] only while no value holds a line break or
/// surrounding spaces, so an untouched field keeps its map through [`Kept`].
pub fn join_pairs(map: &BTreeMap<String, String>, sep: char) -> String {
    map.iter()
        .map(|(k, v)| match sep {
            ':' => format!("{k}: {v}"),
            _ => format!("{k}{sep}{v}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Typed `KEY<sep>value` lines: the text is split by line and at each line's
/// first separator, blank lines are skipped, keys and values trimmed. A value
/// with a line break or surrounding spaces cannot be typed; a stored one
/// survives only through [`Kept`].
pub fn parse_pairs(text: &str, sep: char) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (k, v) = line
            .split_once(sep)
            .ok_or_else(|| format!("`{line}` must be `KEY{sep}value`"))?;
        map.insert(k.trim().to_owned(), v.trim().to_owned());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_round_trip() {
        let mut map = BTreeMap::new();
        map.insert("A".to_owned(), "1".to_owned());
        map.insert("X-B".to_owned(), "two".to_owned());
        assert_eq!(join_pairs(&map, '='), "A=1\nX-B=two");
        assert_eq!(join_pairs(&map, ':'), "A: 1\nX-B: two");
        assert_eq!(parse_pairs(&join_pairs(&map, ':'), ':').unwrap(), map);
    }

    #[test]
    fn pairs_parse_and_reject() {
        let env = parse_pairs("A=1\n\n B = two \n", '=').unwrap();
        assert_eq!(env["A"], "1");
        assert_eq!(env["B"], "two");
        assert!(parse_pairs("novalue", '=').is_err());
        let headers = parse_pairs("X-A: 1", ':').unwrap();
        assert_eq!(headers["X-A"], "1");
    }

    #[test]
    fn untouched_text_keeps_the_stored_value() {
        let dir = Some(PathBuf::from("/tmp/ends with a space "));
        let kept = Kept::new("/tmp/ends with a space ", dir.clone());
        let parsed = |text: &str| Ok(parse_cwd(text));
        assert_eq!(
            resolve(Some(&kept), "/tmp/ends with a space ", parsed),
            Ok(dir)
        );
        assert_eq!(
            resolve(Some(&kept), " /srv/other ", parsed),
            Ok(Some(PathBuf::from("/srv/other")))
        );
        assert_eq!(resolve(Some(&kept), "", parsed), Ok(None));
        assert_eq!(resolve(None, "/tmp/x", parsed), Ok(Some("/tmp/x".into())));
    }

    #[test]
    fn an_edited_command_line_is_parsed_and_errors_surface() {
        let stored = ("srv".to_owned(), vec!["a\nb".to_owned()]);
        let kept = Kept::new("srv 'a b'", stored.clone());
        assert_eq!(resolve(Some(&kept), "srv 'a b'", parse_command), Ok(stored));
        assert_eq!(
            resolve(Some(&kept), r#"srv --verbose "a b""#, parse_command),
            Ok((
                "srv".to_owned(),
                vec!["--verbose".to_owned(), "a b".to_owned()]
            ))
        );
        assert!(resolve(Some(&kept), r#"srv "a"#, parse_command).is_err());
        assert_eq!(parse_command("   "), Err("Command is required".to_owned()));
    }

    fn pairs(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn an_untouched_map_keeps_what_its_text_cannot_show() {
        let parse = |text: &str| parse_pairs(text, '=');
        let env = pairs(&[
            ("CERT", "-----BEGIN-----\nMIIBAB==\n-----END-----"),
            ("PADDED", "  spaced  "),
        ]);
        let text = join_pairs(&env, '=');
        assert!(parse(&text).is_err(), "`-----END-----` has no `=`");
        let kept = Kept::new(text.clone(), env.clone());
        assert_eq!(resolve(Some(&kept), &text, parse), Ok(env));
        assert_eq!(
            resolve(Some(&kept), "PADDED=  spaced  ", parse),
            Ok(pairs(&[("PADDED", "spaced")])),
            "edited text is parsed"
        );
        // Base64 padding on a line of its own reads as a key, silently.
        let padded = pairs(&[("KEY", "MIIB\nAB==")]);
        let text = join_pairs(&padded, '=');
        assert_eq!(parse(&text), Ok(pairs(&[("KEY", "MIIB"), ("AB", "=")])));
        assert_eq!(
            resolve(Some(&Kept::new(text.clone(), padded.clone())), &text, parse),
            Ok(padded)
        );

        let parse = |text: &str| parse_pairs(text, ':');
        let headers = pairs(&[("X-Note", "first\nSecond: line"), ("X-Pad", " v ")]);
        let text = join_pairs(&headers, ':');
        assert_eq!(
            parse(&text),
            Ok(pairs(&[
                ("X-Note", "first"),
                ("Second", "line"),
                ("X-Pad", "v")
            ]))
        );
        let kept = Kept::new(text.clone(), headers.clone());
        assert_eq!(resolve(Some(&kept), &text, parse), Ok(headers));
        assert_eq!(
            resolve(Some(&kept), "X-Pad: w", parse),
            Ok(pairs(&[("X-Pad", "w")]))
        );
    }

    #[test]
    fn a_blank_working_directory_is_the_apps_own() {
        assert_eq!(parse_cwd("  "), None);
        assert_eq!(parse_cwd(" /work dir "), Some(PathBuf::from("/work dir")));
    }
}
