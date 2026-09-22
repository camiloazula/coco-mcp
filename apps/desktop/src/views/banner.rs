//! "Changed since last snapshot" banner above the detail pane, with an
//! expandable list of classified changes. Uses the design's `sunk` row
//! style (28px, hairline) and the existing colours: breaking in `err`,
//! compatible in `accent`, cosmetic in `muted`.

use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};
use mcp_diff::{Change, ItemKind, Severity};

use serde_json::Value;

use crate::clip;
use crate::theme::tokens;
use crate::views::json::{Fit, json_tree_rc};
use crate::views::{Workspace, fold_icon, mono, tree_section};

/// The banner for the selected server, when its last connect found changes.
pub fn render(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let t = *tokens(cx);
    let state = ws.state.read(cx);
    let diff = state.visible_diff()?.clone();
    let expanded = state.server().is_some_and(|s| s.diff_expanded);
    let server_name = state
        .server()
        .map(|s| s.record.name.clone())
        .unwrap_or_default();
    let summary_color = if diff.has_breaking() { t.err } else { t.accent };
    let title = state
        .server()
        .and_then(|s| s.diff_baseline.clone())
        .map(|baseline| format!("Compared with {baseline}"))
        .unwrap_or_else(|| "Changed since last snapshot".to_owned());

    let row = h_flex()
        .h(px(28.))
        .flex_none()
        .px(px(24.))
        .gap(px(12.))
        .bg(t.sunk)
        .border_b_1()
        .border_color(t.hair)
        .child(div().text_size(px(12.)).text_color(t.fg).child(title))
        .child(mono(cx, 11., diff.summary()).text_color(summary_color))
        .child(div().flex_1())
        // The change list is what goes into a changelog or a pull request,
        // so it copies as Markdown rather than as the classifier's JSON.
        .child(clip::copy_button(
            cx,
            "diff-copy",
            "Copy the changes as Markdown",
            "changes",
            mcp_exchange::diff_markdown(&server_name, &diff),
        ))
        .child(
            div()
                .id("diff-details")
                .text_size(px(12.))
                .text_color(t.muted)
                .hover(|s| s.text_color(t.fg))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.toggle_diff_details(cx));
                }))
                .child(if expanded { "Hide" } else { "Details" })
                .test_support(),
        )
        .child(
            div()
                .id("diff-dismiss")
                .text_size(px(13.))
                .text_color(t.muted)
                .hover(|s| s.text_color(t.fg))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.dismiss_diff(cx));
                }))
                .child("×")
                .test_support(),
        );

    let list = expanded.then(|| {
        v_flex()
            .id("diff-list")
            .flex_none()
            .max_h(px(220.))
            .overflow_y_scroll()
            .bg(t.sunk)
            .border_b_1()
            .border_color(t.hair)
            .py(px(4.))
            .children(
                diff.changes()
                    .iter()
                    .enumerate()
                    .map(|(i, c)| change_row(ws, i, c, cx)),
            )
    });

    Some(
        v_flex()
            .flex_none()
            .child(row)
            .children(list)
            .into_any_element(),
    )
}

/// Identifies a change across renders and across servers, so the open row is
/// remembered by what it says rather than by its position.
fn change_key(change: &Change) -> String {
    format!(
        "{}:{}:{}",
        change.severity.label(),
        change.name,
        change.path
    )
}

pub(crate) fn change_row(
    ws: &Workspace,
    index: usize,
    change: &Change,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = *tokens(cx);
    let color = match change.severity {
        Severity::Breaking => t.err,
        Severity::Compatible => t.accent,
        Severity::Cosmetic => t.muted,
    };
    let kind = match change.kind {
        ItemKind::Server => "Server",
        ItemKind::Tool => "Tool",
        ItemKind::Resource => "Resource",
        ItemKind::ResourceTemplate => "Template",
        ItemKind::Prompt => "Prompt",
    };
    // Only a change that carries values expands; the others keep the column
    // alignment with an empty slot.
    let key = change_key(change);
    let expandable = change.before.is_some() || change.after.is_some();
    let open = expandable && ws.expanded_change.as_deref() == Some(key.as_str());
    let line = h_flex()
        .h(px(24.))
        .px(px(24.))
        .gap(px(12.))
        .child(if expandable {
            fold_icon(open, cx).into_any_element()
        } else {
            div().w(px(14.)).flex_none().into_any_element()
        })
        .child(
            mono(cx, 11., crate::views::capitalized(change.severity.label()))
                .w(px(76.))
                .flex_none()
                .text_color(color),
        )
        .child(
            mono(
                cx,
                12.,
                if change.kind == ItemKind::Server {
                    "Server".to_owned()
                } else {
                    format!("{kind} {}", change.name)
                },
            )
            .flex_none()
            .text_color(t.fg)
            .font_weight(FontWeight::MEDIUM),
        )
        .when(!change.path.is_empty(), |el| {
            el.child(mono(cx, 11., change.path.clone()).text_color(t.muted))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(12.))
                .text_color(t.muted)
                .child(crate::views::capitalized(&change.summary)),
        );

    // mcp-diff carries the old and new values for most changes; without this
    // the banner can only say that something changed, never what it became.
    if !expandable {
        return line.into_any_element();
    }
    let clicked = key.clone();
    let line = line
        .id(("diff-row", index))
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.expanded_change = (this.expanded_change.as_deref() != Some(clicked.as_str()))
                .then(|| clicked.clone());
            cx.notify();
        }))
        .test_support();
    if !open {
        return line.into_any_element();
    }
    let toggle = ws.collapse_toggle(cx);
    let folds = ws.folds(&toggle);
    let value = |label: &'static str, v: &Option<Value>, prefix: String| {
        v.as_ref().map(|v| {
            let v = Rc::new(v.clone());
            let body = json_tree_rc(v.clone(), &folds, &prefix, Fit::Rows, cx);
            tree_section(
                cx,
                label,
                SharedString::from(format!("{prefix}-copy")),
                v,
                body,
            )
        })
    };
    v_flex()
        .w_full()
        .child(line)
        .child(
            v_flex()
                .px(px(24.))
                .pl(px(112.))
                .py(px(6.))
                .gap(px(8.))
                .children(value("before", &change.before, format!("before:{key}")))
                .children(value("after", &change.after, format!("after:{key}"))),
        )
        .into_any_element()
}
