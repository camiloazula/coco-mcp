//! Where the log drawer's text filter matched, for showing it: the range of
//! a match in a text as it is drawn, and the excerpt of a long text around
//! one.
//!
//! Hand-written because `std` finds a substring only as it is spelled. The
//! filter decides on lowercased copies (`LogRow::search`), but what is drawn
//! keeps its case, and a range found in a lowercased copy is not a range of
//! the original wherever lowering a character changes its length. `regex`
//! would find it and is in the graph, though only through other crates, and
//! a pattern compiled per keystroke is more than a plain substring needs.
//!
//! Everything the filter compares is lowered by [`lower`], one character at
//! a time, which is how [`find`] reads a text: `str::to_lowercase` lowers a
//! word's last sigma to another letter than the same sigma elsewhere, so a
//! copy made with it would disagree with a match looked for here.

use std::ops::Range;

/// Characters of an excerpt before its match, so the match reads in context
/// and still sits at the start of the cell.
const LEAD: usize = 24;
/// Characters of an excerpt, the length of a row's preview.
const LENGTH: usize = 200;
/// A match that starts within this many characters of a row's preview is
/// shown there; a later one may be past the end of a narrow cell.
pub const NEAR: usize = 40;

/// `text` in lowercase, each character lowered on its own.
pub fn lower(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    text.chars().flat_map(char::to_lowercase).collect()
}

/// The first match of `needle` in `text`, whatever the case of `text`.
/// `needle` is lowercase already ([`lower`]), as the filter keeps it; an
/// empty one matches nothing.
pub fn find(text: &str, needle: &str) -> Option<Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    text.char_indices()
        .find_map(|(start, _)| Some(start..start + match_at(&text[start..], needle)?))
}

/// Bytes of the match of `needle` at the start of `text`.
fn match_at(text: &str, needle: &str) -> Option<usize> {
    let mut wanted = needle.chars().peekable();
    for (at, c) in text.char_indices() {
        if wanted.peek().is_none() {
            return Some(at);
        }
        // A character that lowers to several must match all of them: a
        // match cannot end inside one.
        for lowered in c.to_lowercase() {
            if wanted.next() != Some(lowered) {
                return None;
            }
        }
    }
    wanted.peek().is_none().then_some(text.len())
}

/// Whether `hit` starts close enough to the start of `text` to be read in a
/// narrow cell.
pub fn near_start(text: &str, hit: &Range<usize>) -> bool {
    text[..hit.start].chars().nth(NEAR).is_none()
}

/// The part of `text` around `hit`, a range of it, and the range of the
/// match in that part: a few characters before the match, an ellipsis where
/// text was left out on either side.
pub fn excerpt(text: &str, hit: Range<usize>) -> (String, Range<usize>) {
    let start = text[..hit.start]
        .char_indices()
        .rev()
        .nth(LEAD - 1)
        .map_or(0, |(at, _)| at);
    let end = text[start..]
        .char_indices()
        .nth(LENGTH)
        .map_or(text.len(), |(at, _)| start + at);
    let mut out = String::with_capacity(end - start + 6);
    if start > 0 {
        out.push('…');
    }
    let shift = out.len();
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push('…');
    }
    // A match longer than the excerpt is marked as far as the excerpt goes.
    let hit = shift + hit.start - start..shift + hit.end.min(end) - start;
    (out, hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_match_is_found_whatever_the_case_of_the_text() {
        assert_eq!(find("Tools/List", "tools/l"), Some(0..7));
        assert_eq!(find("{\"URI\":\"mock://Hello\"}", "hello"), Some(15..20));
        assert_eq!(find("abc", "abcd"), None);
        assert_eq!(find("abc", "c"), Some(2..3));
        assert_eq!(find("abc", ""), None, "an empty filter marks nothing");
        assert_eq!(find("", "a"), None);
    }

    #[test]
    fn a_text_and_its_lowered_copy_agree_on_a_match() {
        // `str::to_lowercase` ends a word with `ς` and writes `σ` elsewhere.
        assert_ne!("ΟΔΟΣ".to_lowercase(), lower("ΟΔΟΣ"));
        for (text, typed) in [("ΟΔΟΣ", "ΟΔΟΣ"), ("ΟΣΑ", "ΟΣ"), ("Hello", "ELL")] {
            let needle = lower(typed);
            assert!(lower(text).contains(&needle), "{text} has {typed}");
            assert!(find(text, &needle).is_some(), "{text} has {typed}");
        }
        assert_eq!(lower("Tools/LIST"), "tools/list");
        assert_eq!(lower("İ"), "i\u{307}");
    }

    #[test]
    fn a_range_is_of_the_text_drawn_not_of_its_lowercase() {
        // `İ` is two bytes and lowers to three (`i` and a combining dot):
        // an offset taken in the lowercased copy would be one byte late.
        let text = "İstanbul hello";
        let lowered = text.to_lowercase();
        assert_ne!(lowered.find("hello"), text.find("hello"));
        assert_eq!(find(text, "hello"), text.find("hello").map(|at| at..at + 5));
        // The whole of what `İ` lowers to matches it; half of it does not.
        assert_eq!(find(text, "i\u{307}s"), Some(0..3));
        assert_eq!(find("İ", "i"), None);
        assert_eq!(find("ÉTÉ été", "été"), Some(0..5));
    }

    #[test]
    fn a_match_near_the_start_is_read_where_it_is() {
        let text = format!("{}needle", "x".repeat(NEAR));
        assert!(near_start(&text, &(NEAR..NEAR + 6)));
        let text = format!("{}needle", "x".repeat(NEAR + 1));
        assert!(!near_start(&text, &(NEAR + 1..NEAR + 7)));
        // Counted in characters, not bytes.
        let text = format!("{}needle", "é".repeat(NEAR));
        assert!(near_start(&text, &(NEAR * 2..NEAR * 2 + 6)));
    }

    #[test]
    fn an_excerpt_starts_a_little_before_its_match() {
        let text = format!("{}needle{}", "a".repeat(100), "b".repeat(400));
        let (shown, hit) = excerpt(&text, 100..106);
        assert_eq!(&shown[hit.clone()], "needle");
        assert!(shown.starts_with(&format!("…{}needle", "a".repeat(LEAD))));
        assert!(shown.ends_with('…'));
        assert_eq!(shown.chars().count(), LENGTH + 2);

        // Nothing is left out of a short text, so nothing says so.
        let (shown, hit) = excerpt("a needle here", 2..8);
        assert_eq!((shown.as_str(), hit), ("a needle here", 2..8));
    }

    #[test]
    fn an_excerpt_cuts_between_characters() {
        let text = format!("{}NEEDLE{}", "é".repeat(60), "ü".repeat(300));
        let hit = find(&text, "needle").unwrap();
        let (shown, hit) = excerpt(&text, hit);
        assert_eq!(&shown[hit], "NEEDLE");
        assert_eq!(shown.chars().count(), LENGTH + 2);

        // A match longer than the excerpt is marked to the excerpt's end.
        let long = "x".repeat(LENGTH * 2);
        let (shown, hit) = excerpt(&long, 0..long.len());
        assert_eq!(&shown[hit], "x".repeat(LENGTH));
    }
}
