//! Terminal output in cargo's style.
//!
//! `anstyle` holds the palette (no I/O), `anstream` decides at write time
//! whether the stream gets ANSI codes, gets them stripped, or gets the
//! Windows console API, honouring `NO_COLOR`, `CLICOLOR_FORCE` and whether
//! the stream is a terminal. Both already ship with `clap`.
//!
//! Status lines follow cargo's shape: a bold coloured verb right-aligned in
//! a twelve-column field, then plain text. Everything here writes to stderr
//! so stdout carries only the JSON result.

#![allow(clippy::print_stderr)]

use std::fmt::Display;

use anstyle::{AnsiColor, Effects, Style};

/// Help headings and usage; successful status verbs.
pub const HEADER: Style = AnsiColor::Green.on_default().effects(Effects::BOLD);
/// Flags, values and subcommand names in help.
pub const LITERAL: Style = AnsiColor::Cyan.on_default().effects(Effects::BOLD);
/// Argument placeholders in help.
pub const PLACEHOLDER: Style = AnsiColor::Cyan.on_default();
/// Errors, and breaking changes in a diff.
pub const ERROR: Style = AnsiColor::Red.on_default().effects(Effects::BOLD);
/// Warnings, and invalid values in help.
pub const WARN: Style = AnsiColor::Yellow.on_default().effects(Effects::BOLD);
/// Notes and informational verbs.
pub const NOTE: Style = AnsiColor::Cyan.on_default().effects(Effects::BOLD);
/// Success, and compatible changes in a diff.
pub const GOOD: Style = HEADER;
/// Secondary detail, and cosmetic changes in a diff.
pub const MUTED: Style = Style::new().effects(Effects::DIMMED);

/// clap's help and error colours, matching cargo's.
pub const CLAP: clap::builder::Styles = clap::builder::Styles::styled()
    .header(HEADER)
    .usage(HEADER)
    .literal(LITERAL)
    .placeholder(PLACEHOLDER)
    .error(ERROR)
    .valid(LITERAL)
    .invalid(WARN);

/// Column the verb is right-aligned to, as in cargo.
const VERB_WIDTH: usize = 12;

/// `   Connecting some-server`, the verb bold green.
pub fn status(verb: &str, message: impl Display) {
    status_with(GOOD, verb, message);
}

/// A status line in another colour.
pub fn status_with(style: Style, verb: &str, message: impl Display) {
    let message = plain(message);
    anstream::eprintln!("{style}{verb:>VERB_WIDTH$}{style:#} {message}");
}

/// `error: …` in bold red.
pub fn error(message: impl Display) {
    let message = plain(message);
    anstream::eprintln!("{ERROR}error{ERROR:#}: {message}");
}

/// `warning: …` in bold yellow.
pub fn warn(message: impl Display) {
    let message = plain(message);
    anstream::eprintln!("{WARN}warning{WARN:#}: {message}");
}

/// `text` with every control character but the tab replaced, so a server's
/// stderr, a log message or an imported name cannot carry an escape
/// sequence into the terminal.
pub fn plain(text: impl Display) -> String {
    text.to_string()
        .chars()
        .map(|c| {
            if c.is_control() && c != '\t' {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Wrap `text` in `style` without disturbing an alignment width applied by
/// the caller (the width must be applied to the plain text, not the codes).
pub fn paint(style: Style, text: impl Display) -> String {
    format!("{style}{text}{style:#}")
}

/// Read `--color <when>` before clap parses, so clap's own help and errors
/// obey it too. Unknown values are left to clap's value parser to reject.
pub fn apply_color_from_args() {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = match arg.strip_prefix("--color") {
            Some("") => args.next(),
            Some(rest) => rest.strip_prefix('=').map(str::to_owned),
            None => continue,
        };
        let Some(value) = value else { continue };
        let choice = match value.as_str() {
            "always" => anstream::ColorChoice::Always,
            "never" => anstream::ColorChoice::Never,
            "auto" => anstream::ColorChoice::Auto,
            _ => continue,
        };
        choice.write_global();
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_verbs_align_like_cargo() {
        let line = format!("{:>VERB_WIDTH$} {}", "Connecting", "srv");
        assert_eq!(line, "  Connecting srv");
        assert_eq!(format!("{:>VERB_WIDTH$}", "Wrote").len(), VERB_WIDTH);
    }

    #[test]
    fn paint_keeps_the_plain_text() {
        let painted = paint(ERROR, format!("{:<11}", "breaking"));
        assert!(painted.contains("breaking   "));
        assert!(painted.starts_with('\x1b') && painted.ends_with('m'));
    }
}
