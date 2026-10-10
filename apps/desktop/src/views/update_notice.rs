//! The update notice: a note in the status bar when a newer release is out,
//! and the popover it opens with the command that upgrades this install.
//!
//! Drawn by hand like the copy menu (see [`crate::clip::CopyMenu`]), with
//! the same hairline card and 3px radius, anchored over the status bar's
//! right end where the note sits.

use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};
use time::OffsetDateTime;
use time::macros::format_description;

use crate::theme::tokens;
use crate::update::{self, Update};
use crate::views::{mono, muted};

/// Width of the popover.
const WIDTH: f32 = 300.;

/// The status bar's note, when there is a release to announce.
pub fn note(cx: &App) -> Option<AnyElement> {
    let notice = cx.try_global::<Update>()?.notice()?;
    let t = *tokens(cx);
    let label = format!("{} available", notice.latest.version);
    Some(
        div()
            .id("update-note")
            .flex_none()
            .text_color(t.accent)
            .cursor_pointer()
            .hover(|s| s.text_color(t.fg))
            .aria_label(label.clone())
            .on_click(|_, _, cx| update::toggle(cx))
            .child(label)
            .test_support()
            .into_any_element(),
    )
}

/// The popover, when open.
pub fn render(cx: &App) -> Option<AnyElement> {
    let update = cx.try_global::<Update>()?;
    if !update.open {
        return None;
    }
    let notice = update.notice()?;
    let t = *tokens(cx);
    let released = OffsetDateTime::from_unix_timestamp(notice.latest.published_at)
        .ok()
        .and_then(|at| {
            at.format(format_description!("[month repr:short] [day padding:none]"))
                .ok()
        });
    let detail = match released {
        Some(day) => format!("You have {} · released {day}", notice.current),
        None => format!("You have {}", notice.current),
    };
    let url = notice.latest.url.clone();
    let link = |id: &'static str, label: &'static str, color| {
        div()
            .id(id)
            .text_size(px(12.))
            .text_color(color)
            .cursor_pointer()
            .hover(move |s| s.text_color(t.fg))
            .child(label)
    };
    let command = notice.command.map(|command| {
        h_flex()
            .mt(px(10.))
            .px(px(8.))
            .h(px(28.))
            .items_center()
            .justify_between()
            .bg(t.sunk)
            .border_1()
            .border_color(t.hair)
            .rounded(px(3.))
            .child(mono(cx, 12., command))
            .child(crate::clip::copy_button(
                cx,
                "update-copy",
                "Copy the command",
                "the upgrade command",
                command,
            ))
    });
    // An install with no command gets the release itself, where the
    // archives are, rather than the notes.
    let open_label = if notice.command.is_some() {
        "What's new"
    } else {
        "Download"
    };
    let card = v_flex()
        .id("update-popover")
        .occlude()
        .absolute()
        .right(px(12.))
        .bottom(px(28.))
        .w(px(WIDTH))
        .p(px(12.))
        .bg(t.bg)
        .border_1()
        .border_color(t.hair)
        .rounded(px(3.))
        // A click inside is not a click outside.
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(format!(
                            "{} {} is available",
                            crate::APP_NAME,
                            notice.latest.version
                        )),
                )
                .child(
                    crate::clip::icon_button(
                        cx,
                        "update-dismiss",
                        IconName::Close,
                        "Hide until the next launch",
                    )
                    .on_click(|_, _, cx| update::dismiss(cx))
                    .test_support(),
                ),
        )
        .child(div().mt(px(2.)).child(muted(cx, 12., detail)))
        .children(command)
        .child(
            h_flex()
                .mt(px(10.))
                .gap(px(12.))
                .child(
                    link("update-open", open_label, t.accent)
                        .on_click(move |_, _, cx| {
                            if let Err(e) = open::that_detached(&url) {
                                tracing::warn!("cannot open {url}: {e}");
                            }
                            update::close(cx);
                        })
                        .test_support(),
                )
                .child(
                    link("update-skip", "Skip this version", t.muted)
                        .on_click(|_, _, cx| update::skip(cx))
                        .test_support(),
                ),
        )
        .test_support();
    Some(
        div()
            .id("update-overlay")
            .occlude()
            .absolute()
            .inset_0()
            .on_click(|_, _, cx| update::close(cx))
            .child(card)
            .into_any_element(),
    )
}
