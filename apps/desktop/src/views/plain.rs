//! A text block of a response as a read-only text area. The text lives in
//! a rope and only the lines in view are laid out, so a block of any length
//! costs a frame the same; the text can be selected, searched with the
//! keyboard and copied. Nothing is parsed or converted: the text is shown as
//! the server sent it.
//!
//! The text area is created the first time its block is drawn and kept for
//! as long as the block stays on screen, keyed by the block's element id;
//! the text is set again only when the block shows another text (the
//! `stamp` from `kept::Decoded`).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Entity, IntoElement, RenderOnce, SharedString, Styled, Window, px,
};

/// Rows a text block takes at most when drawn beside other blocks: about
/// half a tall window. The block scrolls inside for the rest.
pub const MAX_ROWS: usize = 30;

/// A response's text, as the text area under `id`.
#[derive(IntoElement)]
pub struct PlainText {
    id: SharedString,
    text: SharedString,
    stamp: u64,
    fill: bool,
}

/// What is kept between frames: the text area and the stamp of the text it
/// holds.
struct Kept {
    editor: Entity<TextareaState>,
    stamp: u64,
    rows: usize,
}

impl PlainText {
    /// `text`, drawn as `id`; `stamp` tells one text from another under the
    /// same id without comparing them.
    pub fn new(id: impl Into<SharedString>, text: SharedString, stamp: u64) -> Self {
        Self {
            id: id.into(),
            text,
            stamp,
            fill: false,
        }
    }

    /// Fill the height given rather than take the rows the text needs: for
    /// the one block of a response, which has the whole response panel.
    pub fn fill(mut self, fill: bool) -> Self {
        self.fill = fill;
        self
    }
}

/// Lines in `text`, one at least.
fn rows_of(text: &str) -> usize {
    text.lines().count().max(1)
}

impl RenderOnce for PlainText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            text,
            stamp,
            fill,
        } = self;
        let first = text.clone();
        let kept = window.use_keyed_state(id, cx, move |window, cx| {
            let rows = rows_of(&first);
            let editor = cx.new(|cx| {
                let mut state = TextareaState::new(window, cx);
                state.set_value(first.clone(), window, cx);
                state.set_readonly(true, cx);
                state
            });
            Kept {
                editor,
                stamp,
                rows,
            }
        });
        let (editor, rows) = kept.update(cx, |kept, cx| {
            if kept.stamp != stamp {
                kept.stamp = stamp;
                kept.rows = rows_of(&text);
                kept.editor
                    .update(cx, |editor, cx| editor.set_value(text.clone(), window, cx));
            }
            (kept.editor.clone(), kept.rows)
        });
        // Its own height when it is not the only block: as many rows as the
        // text has, up to a cap, and the rest by scrolling inside.
        if !fill {
            let rows = rows.min(MAX_ROWS);
            editor.update(cx, |editor, cx| editor.set_rows(rows, cx));
        }
        Textarea::new(&editor)
            .readonly(true)
            .appearance(false)
            .bordered(false)
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(12.))
            .when(fill, |area| area.flex_1().min_h_0())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_takes_a_row_per_line() {
        assert_eq!(rows_of(""), 1);
        assert_eq!(rows_of("one"), 1);
        assert_eq!(rows_of("one\ntwo\n"), 2);
        assert_eq!(rows_of("one\ntwo\nthree"), 3);
    }
}
