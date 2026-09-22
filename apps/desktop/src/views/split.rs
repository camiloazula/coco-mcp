//! The detail pane below its header and toolbar: the input (form, arguments
//! or declaration) above and the response below, each a scroll view of its
//! own, with a draggable separator between them. Neither moves the other,
//! so a long form is read beside a long result.
//!
//! The separator starts halfway and is remembered per selection, like the
//! folds of the trees: the split that suits one tool's form is not the one
//! that suits another's. Before the first call there is no response and
//! the input takes the whole pane.

use std::collections::HashMap;

use gpui_kit::component::resizable::{ResizableState, resizable_panel, v_resizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Pixels, StatefulInteractiveElement, Styled, TestSupportExt as _, div, px, relative,
};

use crate::calls::ResponseKey;
use crate::state::{Gone, Mode};
use crate::views::Workspace;

/// The least either panel can be dragged to: enough for a row of a form or
/// a response header with a line under it.
const PANEL_MIN: Pixels = px(80.);

/// Split state of every selection dragged or shown so far, by its key.
#[derive(Default)]
pub struct Splits(HashMap<ResponseKey, Entity<ResizableState>>);

impl std::fmt::Debug for Splits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Splits").field(&self.0.len()).finish()
    }
}

impl Splits {
    /// The split of `key`, created halfway on first use.
    fn of(&mut self, key: ResponseKey, cx: &mut Context<Workspace>) -> Entity<ResizableState> {
        self.0
            .entry(key)
            .or_insert_with(|| cx.new(|_| ResizableState::default()))
            .clone()
    }

    /// Drop the splits of what `gone` names.
    pub(crate) fn forget(&mut self, gone: &Gone) {
        self.0.retain(|key, _| keeps(key, gone));
    }
}

/// Whether the split of `key` outlives `gone`: a server takes every split
/// under it, a cleared call its history split, and log rows own none.
fn keeps((server, mode, name): &ResponseKey, gone: &Gone) -> bool {
    match gone {
        Gone::LogRows { .. } => true,
        Gone::Server { id, .. } => server != id,
        Gone::Calls { calls } => *mode != Mode::History || !calls.contains(name),
    }
}

/// The view of the input, the tab body under the toolbar: a scroll view,
/// or, for a body that fills (one tree, which scrolls inside), a column
/// that gives it the height.
fn input_view(body: Vec<AnyElement>, fills: bool) -> AnyElement {
    div()
        .id("detail-input")
        .size_full()
        .min_h_0()
        .when(!fills, |view| view.overflow_y_scroll())
        .when(fills, |view| view.flex().flex_col())
        .children(body)
        .test_support()
        .into_any_element()
}

/// `body` above `response`, split by the separator of `key`; `body` alone,
/// filling the pane, while there is no response. `fills` says the body
/// takes the height it is given rather than scrolling in it.
pub(crate) fn render(
    ws: &mut Workspace,
    key: Option<ResponseKey>,
    body: Vec<AnyElement>,
    fills: bool,
    response: Option<AnyElement>,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let input = input_view(body, fills);
    let (Some(response), Some(key)) = (response, key) else {
        return div().flex_1().min_h_0().child(input).into_any_element();
    };
    let state = ws.splits.of(key, cx);
    v_resizable("detail-split")
        .with_state(&state)
        .child(
            resizable_panel()
                .size_range(PANEL_MIN..Pixels::MAX)
                // Halfway on the first frame; measured pixels, and then the
                // drag, from the next.
                .flex_basis(relative(0.5))
                .child(input),
        )
        .child(
            resizable_panel()
                .size_range(PANEL_MIN..Pixels::MAX)
                .flex_basis(relative(0.5))
                .child(response),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_leave_with_their_server_or_call() {
        let key = |server: &str, mode: Mode, name: &str| -> ResponseKey {
            (server.into(), mode, name.into())
        };
        let tool = key("a", Mode::Tools, "echo");
        let call = key("a", Mode::History, "call-1");
        let other = key("b", Mode::History, "call-2");
        let same_name = key("b", Mode::Tools, "call-2");
        let rows = Gone::LogRows {
            server_id: "a".into(),
            before: 9,
        };
        for k in [&tool, &call, &other, &same_name] {
            assert!(keeps(k, &rows), "log rows own no split");
        }
        let cleared = Gone::Calls {
            calls: vec!["call-2".into()],
        };
        assert!(keeps(&tool, &cleared) && keeps(&call, &cleared));
        assert!(!keeps(&other, &cleared), "a cleared call takes its split");
        assert!(
            keeps(&same_name, &cleared),
            "not a tool that happens to share the name"
        );
        let deleted = Gone::Server {
            id: "a".into(),
            calls: vec![],
        };
        assert!(!keeps(&tool, &deleted) && !keeps(&call, &deleted));
        assert!(keeps(&other, &deleted) && keeps(&same_name, &deleted));
    }
}
