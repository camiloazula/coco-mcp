//! A stdio server's command and arguments as one line of text.
//!
//! The line is what a POSIX shell would split back into the same words:
//! single and double quotes group, a backslash escapes, and an unquoted word
//! that starts with `#` begins a comment, as in a shell. Nothing is expanded;
//! `$HOME` and `~` stay as written, because the process is spawned directly
//! and never sees a shell.

/// Join `words` into one line, quoting each word that needs it, so that
/// [`split_command_line`] gives the same words back.
pub fn command_line<'a>(words: impl IntoIterator<Item = &'a str>) -> String {
    let words: Vec<&str> = words.into_iter().collect();
    // The only quoting error is a NUL byte, which no process argument can
    // carry anyway; show such a word as it is rather than nothing.
    shlex::try_join(words.iter().copied()).unwrap_or_else(|_| words.join(" "))
}

/// Split a typed command line into the program and its arguments.
pub fn split_command_line(text: &str) -> Result<Vec<String>, String> {
    shlex::split(text)
        .ok_or_else(|| "the command line has an unclosed quote or a trailing backslash".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_round_trips_every_argument() {
        let words = [
            "srv",
            "--verbose",
            "server name",
            "it's",
            "#tag",
            "~/dir",
            "a\"b",
            "back\\slash",
            "",
            "\"\"",
            "--port=3000",
            "$HOME",
            "üñí",
            "two\nlines",
        ];
        let line = command_line(words);
        assert_eq!(split_command_line(&line).unwrap(), words);
    }

    #[test]
    fn plain_words_are_not_quoted() {
        assert_eq!(
            command_line(["srv", "--verbose", "server", "/tmp/x.json"]),
            "srv --verbose server /tmp/x.json"
        );
    }

    #[test]
    fn typed_quotes_and_escapes_are_honoured() {
        assert_eq!(
            split_command_line(r#"srv --verbose "a b" c\ d 'e f' 'it'\''s'"#).unwrap(),
            ["srv", "--verbose", "a b", "c d", "e f", "it's"]
        );
    }

    #[test]
    fn an_unclosed_quote_is_an_error() {
        assert!(split_command_line(r#"srv "unclosed"#).is_err());
        assert!(split_command_line("srv 'unclosed").is_err());
        assert!(split_command_line("srv trailing\\").is_err());
    }

    #[test]
    fn an_empty_line_has_no_words() {
        assert!(split_command_line("").unwrap().is_empty());
        assert!(split_command_line("   ").unwrap().is_empty());
    }

    #[test]
    fn a_nul_cannot_be_quoted_but_is_still_shown() {
        assert_eq!(command_line(["a\0b", "c"]), "a\0b c");
    }
}
