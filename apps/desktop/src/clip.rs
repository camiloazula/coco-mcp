//! Getting data out: the clipboard, and the native save panel.
//!
//! A debugger exists to be read from, so every surface that shows
//! structured data offers the same two affordances, built here. The formats
//! themselves live in `mcp-exchange`, which the CLI shares, and neither this
//! module nor that crate ever reads a secret on its own.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Div, ElementId, Global, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, SharedString, Stateful, StatefulInteractiveElement, Styled,
    TestSupportExt as _, div, px,
};
use serde_json::Value;

use crate::state::size_label;
use crate::theme::tokens;

/// How long a confirmation stays in the status bar.
const SHOWN_FOR: Duration = Duration::from_secs(3);

/// The last copy or save, until it expires.
///
/// A global rather than workspace state: a copy button only needs `&mut App`,
/// which keeps every one of them a plain element that any view can build,
/// including the ones that render without a `Context<Workspace>`.
#[derive(Debug, Default)]
pub struct Copied {
    note: Option<SharedString>,
    /// Bumped on every announcement so a stale timer cannot clear a newer one.
    generation: u64,
}

impl Global for Copied {}

/// The confirmation the status bar should show, if any.
pub fn note(cx: &App) -> Option<SharedString> {
    cx.try_global::<Copied>().and_then(|c| c.note.clone())
}

/// Put `text` on the clipboard and confirm it in the status bar.
pub fn copy(text: impl Into<String>, what: impl std::fmt::Display, cx: &mut App) {
    let text = text.into();
    let note = format!("copied {what} · {}", size_label(text.len()));
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    announce(note, cx);
}

/// Ask for a path, then write `contents` there off the UI thread.
pub fn save(suggested_name: impl Into<String>, contents: Vec<u8>, cx: &mut App) {
    let suggested_name = suggested_name.into();
    let directory = directories::UserDirs::new()
        .and_then(|d| d.document_dir().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let path = cx.prompt_for_new_path(&directory, Some(&suggested_name));
    cx.spawn(async move |cx| {
        // Cancelled panel, closed channel or platform error: nothing to say.
        let Ok(Ok(Some(path))) = path.await else {
            return;
        };
        let bytes = contents.len();
        let written = cx
            .background_spawn(async move { std::fs::write(&path, contents).map(|()| path) })
            .await;
        cx.update(|cx| match written {
            Ok(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                announce(format!("Saved {name} · {}", size_label(bytes)), cx);
            }
            Err(e) => announce(format!("Save failed: {e}"), cx),
        });
    })
    .detach();
}

/// A muted icon button with a hover caption, the shape the sidebar's actions
/// already use. Public so a view can attach its own handler; the two
/// buttons below cover everything that needs no view state.
pub fn icon_button(
    cx: &App,
    id: impl Into<ElementId>,
    icon: IconName,
    caption: impl Into<SharedString>,
) -> Stateful<Div> {
    let t = *tokens(cx);
    let caption = caption.into();
    div()
        .id(id)
        .flex_none()
        .p(px(2.))
        .text_color(t.muted)
        .hover(|s| s.text_color(t.fg))
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(caption.clone()).build(window, cx))
        .child(Icon::new(icon).with_size(px(12.)).text_color(t.muted))
}

/// A button that copies `text`. `what` names it in the confirmation. The
/// text is shared with whatever else draws it and copied only when pressed.
pub fn copy_button(
    cx: &App,
    id: impl Into<ElementId>,
    caption: impl Into<SharedString>,
    what: impl Into<String>,
    text: impl Into<SharedString>,
) -> impl IntoElement {
    let what = what.into();
    let text = text.into();
    icon_button(cx, id, IconName::Copy, caption)
        .on_click(move |_, _, cx| copy(text.to_string(), &what, cx))
        .test_support()
}

/// A button that copies `value` as indented JSON. The text is built when the
/// button is pressed, not on every frame that draws it.
pub fn copy_value_button(
    cx: &App,
    id: impl Into<ElementId>,
    caption: impl Into<SharedString>,
    what: impl Into<String>,
    value: Rc<Value>,
) -> impl IntoElement {
    let what = what.into();
    icon_button(cx, id, IconName::Copy, caption)
        .on_click(move |_, _, cx| copy(mcp_exchange::pretty(&value), &what, cx))
        .test_support()
}

/// A button that saves `contents` under a suggested name. The bytes are
/// shared with whatever else draws them and copied only when pressed.
pub fn save_button(
    cx: &App,
    id: impl Into<ElementId>,
    caption: impl Into<SharedString>,
    suggested_name: String,
    contents: Arc<[u8]>,
) -> impl IntoElement {
    icon_button(cx, id, IconName::Download, caption)
        .on_click(move |_, _, cx| save(suggested_name.clone(), contents.to_vec(), cx))
        .test_support()
}

/// One entry of the copy menu: what it says, what it copies, what the
/// confirmation calls it.
#[derive(Debug, Clone)]
pub struct MenuEntry {
    /// Menu label.
    pub label: SharedString,
    /// Text put on the clipboard.
    pub text: String,
    /// Noun for the status bar.
    pub what: SharedString,
}

impl MenuEntry {
    /// An entry labelled `label` that copies `text`, named `what` afterwards.
    pub fn new(label: &'static str, what: &'static str, text: String) -> Self {
        Self {
            label: label.into(),
            text,
            what: what.into(),
        }
    }
}

/// The copy menu, while one is open.
///
/// Hand-written rather than gpui-component's `ContextMenu`: that component
/// keeps its popup entity in a shared cell that a dismiss subscription also
/// captures, so every open leaks one entity, and the headless tests refuse to
/// exit with a leaked handle. The overlay below is a few dozen lines, and the
/// app already draws its dialogs and its palette the same way.
#[derive(Debug, Default)]
pub struct CopyMenu {
    entries: Vec<MenuEntry>,
    position: Point<Pixels>,
}

impl Global for CopyMenu {}

/// Open the copy menu at `position` with `entries`.
pub fn open_menu(entries: Vec<MenuEntry>, position: Point<Pixels>, cx: &mut App) {
    cx.set_global(CopyMenu { entries, position });
    cx.refresh_windows();
}

/// Close it, if it is open.
pub fn close_menu(cx: &mut App) {
    if cx
        .try_global::<CopyMenu>()
        .is_some_and(|m| !m.entries.is_empty())
    {
        cx.set_global(CopyMenu::default());
        cx.refresh_windows();
    }
}

/// The open menu's entries and where to draw them.
pub fn menu(cx: &App) -> Option<(Vec<MenuEntry>, Point<Pixels>)> {
    let menu = cx.try_global::<CopyMenu>()?;
    (!menu.entries.is_empty()).then(|| (menu.entries.clone(), menu.position))
}

/// Say in the status bar why an action produced nothing. Silence would read
/// as a copy that worked.
pub fn announce_nothing(reason: impl Into<SharedString>, cx: &mut App) {
    announce(reason, cx);
}

fn announce(note: impl Into<SharedString>, cx: &mut App) {
    if cx.try_global::<Copied>().is_none() {
        cx.set_global(Copied::default());
    }
    let generation = {
        let state = cx.global_mut::<Copied>();
        state.generation = state.generation.wrapping_add(1);
        state.note = Some(note.into());
        state.generation
    };
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SHOWN_FOR).await;
        cx.update(|cx| {
            if cx
                .try_global::<Copied>()
                .is_some_and(|c| c.generation == generation)
            {
                cx.global_mut::<Copied>().note = None;
                cx.refresh_windows();
            }
        });
    })
    .detach();
}
