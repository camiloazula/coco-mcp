//! Views. Each file stays near 300 lines, tests aside; shared drawing helpers live here.

pub mod about;
mod add_server;
mod banner;
mod blob;
mod call;
mod confirm_dialog;
mod content;
pub mod copy;
mod copy_menu;
mod detail;
pub mod form;
pub mod history;
mod item_list;
pub mod json;
mod json_budget;
mod kept;
mod list_failures;
mod log_drawer;
mod log_list;
mod log_menu;
mod palette;
mod prune;
mod request_answers;
pub mod request_dialog;
mod response;
mod server_fields;
mod server_view;
mod sidebar;
mod status_bar;
mod workspace;

pub use about::About;
pub use add_server::AddServerForm;
pub use call::Selection;
pub use content::laid_out_size;
pub use log_list::{LogList, fold_prefix};
pub use request_answers::RequestUi;
pub use response::{LARGE_RESULT_BYTES, stored_size};
pub use workspace::Workspace;

use std::rc::Rc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, Stateful, StatefulInteractiveElement, Styled, div, px,
};
use serde_json::Value;

use crate::clip;
use crate::theme::tokens;

/// Line height for the 13px body text (1.4 in the design).
pub const BODY_LINE_HEIGHT: f32 = 18.2;

/// Monospace text at `size` px.
pub fn mono(cx: &App, size: f32, text: impl Into<SharedString>) -> Div {
    div()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(size))
        .child(text.into())
}

/// Muted helper text at `size` px.
pub fn muted(cx: &App, size: f32, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(size))
        .text_color(tokens(cx).muted)
        .child(text.into())
}

/// Keyboard hint, 11px muted (`⌘⏎`, `esc`). Drawn in the UI face: Inter
/// covers ⌘ ⇧ ⏎ ↑ ↓, the bundled mono face does not, and a missing glyph
/// would be filled from a proprietary system font.
pub fn kbd(cx: &App, text: impl Into<SharedString>) -> Div {
    muted(cx, 11., text)
}

/// The fold chevron shared by the JSON trees and the generated form: a 10px
/// Lucide chevron (bundled, ISC) in a 14px box, muted. Neither bundled font
/// carries ▾ / ▸.
pub fn fold_icon(open: bool, cx: &App) -> Div {
    let color = tokens(cx).muted;
    div().w(px(14.)).flex_none().text_color(color).child(
        Icon::new(if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .with_size(px(10.))
        .text_color(color),
    )
}

/// A 1px hairline row separator (top border).
pub fn hairline_top(cx: &App, el: Div) -> Div {
    el.border_t_1().border_color(tokens(cx).hair)
}

/// A text-only tab button (`Form` / `Raw`, `stdio` / `HTTP`).
pub fn text_tab(cx: &App, label: &'static str, selected: bool) -> Div {
    let t = tokens(cx);
    div()
        .text_size(px(12.))
        .text_color(if selected { t.fg } else { t.muted })
        .font_weight(if selected {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .cursor_pointer()
        .child(label)
}

/// A text control that cannot run now (see `crate::features`): drawn muted,
/// with `reason` as its hover caption. Pressing it puts the reason in the
/// status bar rather than doing nothing without a word.
pub fn disabled_control(
    cx: &App,
    id: impl Into<ElementId>,
    label: &'static str,
    reason: &'static str,
) -> Stateful<Div> {
    let t = tokens(cx);
    text_tab(cx, label, false)
        .id(id)
        .text_color(t.muted)
        .cursor_default()
        .tooltip(move |window, cx| Tooltip::new(reason).build(window, cx))
        .on_click(move |_, _, cx| crate::clip::announce_nothing(reason, cx))
}

/// The accent action button (`Call`, `Connect`, `Add server`).
pub fn accent_button(cx: &App, label: &'static str, height: f32) -> Div {
    let t = tokens(cx);
    h_flex()
        .h(px(height))
        .px(px(12.))
        .rounded(px(3.))
        .bg(t.accent)
        .text_color(t.on_accent)
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label)
}

/// Header row of the detail pane: title (15px) left, meta right, baseline aligned.
pub fn detail_header(title: impl IntoElement, meta: impl IntoElement) -> Div {
    h_flex()
        .items_baseline()
        .justify_between()
        .gap(px(12.))
        .pt(px(16.))
        .px(px(24.))
        .pb(px(12.))
        .child(title)
        .child(meta)
}

/// A named block of data: the label, its copy action on the right, the body.
///
/// Every block the app shows is drawn with this, so "what is this and how do
/// I get it out" is answered in the same place on every screen.
pub fn labelled(
    cx: &App,
    label: impl Into<SharedString>,
    copy: impl IntoElement,
    body: AnyElement,
) -> Div {
    v_flex()
        .gap(px(4.))
        .child(
            h_flex()
                .gap(px(8.))
                .items_center()
                .child(muted(cx, 11., label.into()))
                .child(copy),
        )
        .child(body)
}

/// A labelled block whose copy action yields `value` as indented JSON, built
/// when pressed. Pass the `Rc` the block's tree was drawn from
/// (`json::json_tree_rc`), so the two share one clone.
/// Single nodes are copied from the tree's own right-click menu.
pub fn tree_section(
    cx: &App,
    label: impl Into<SharedString>,
    id: impl Into<ElementId>,
    value: Rc<Value>,
    body: AnyElement,
) -> Div {
    let label = label.into();
    // The caption reads as a sentence even under a capitalised title.
    let mut chars = label.chars();
    let object: String = match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    };
    let copy = clip::copy_value_button(cx, id, format!("Copy {object}"), label.to_string(), value);
    labelled(cx, label, copy, body)
}

/// `text` with its first letter capitalised: a label drawn from a lowercase
/// identifier (a log filter, a log level, a change's severity).
pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
