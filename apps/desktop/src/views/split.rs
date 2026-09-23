//! The detail pane below its header and toolbar: the input (form, arguments
//! or declaration) above and the response below, each a scroll view of its
//! own, with a draggable separator between them. Neither moves the other,
//! so a long form is read beside a long result.
//!
//! The separator starts where the input ends: a short form leaves the
//! response the rest of the pane, and a long one takes half. Dragged, it
//! stays where it was put. The split is remembered per selection, like the
//! folds of the trees: the split that suits one tool's form is not the one
//! that suits another's. Before the first call there is no response and
//! the input takes the whole pane.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::resizable::{
    ResizablePanelEvent, ResizableState, resizable_panel, v_resizable,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Pixels, StatefulInteractiveElement, Styled, Subscription, TestSupportExt as _, Window, canvas,
    div, px, relative,
};

use crate::calls::ResponseKey;
use crate::state::{Gone, Mode};
use crate::views::Workspace;

/// The least either panel can be dragged to: enough for a row of a form or
/// a response header with a line under it.
const PANEL_MIN: Pixels = px(80.);

/// One selection's split, and what fits it.
struct Split {
    state: Entity<ResizableState>,
    /// The height the input body last laid out at, when it scrolls; `None`
    /// while it fills or has not been drawn.
    input: Rc<Cell<Option<Pixels>>>,
    /// The input panel's height the fit last asked for, so the resize it
    /// emits is told from a drag.
    fitted: Rc<Cell<Option<Pixels>>>,
    /// Whether the user dragged the separator, after which it is theirs.
    dragged: Rc<Cell<bool>>,
    _resized: Subscription,
}

/// What rendering a split needs of it, cloned out so nothing of the
/// workspace stays borrowed.
#[derive(Clone)]
struct Handles {
    state: Entity<ResizableState>,
    input: Rc<Cell<Option<Pixels>>>,
    fitted: Rc<Cell<Option<Pixels>>>,
    dragged: Rc<Cell<bool>>,
}

/// Split state of every selection dragged or shown so far, by its key.
#[derive(Default)]
pub struct Splits(HashMap<ResponseKey, Split>);

impl std::fmt::Debug for Splits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Splits").field(&self.0.len()).finish()
    }
}

impl Splits {
    /// The split of `key`, created on first use.
    fn of(&mut self, key: ResponseKey, cx: &mut Context<Workspace>) -> Handles {
        let split = self.0.entry(key).or_insert_with(|| {
            let state = cx.new(|_| ResizableState::default());
            let fitted = Rc::new(Cell::new(None));
            let dragged = Rc::new(Cell::new(false));
            // A resize the fit did not ask for is the user's.
            let resized = cx.subscribe(&state, {
                let fitted = fitted.clone();
                let dragged = dragged.clone();
                move |_, state, _: &ResizablePanelEvent, cx| {
                    let input = state.read(cx).sizes().first().copied();
                    let ours = match (input, fitted.get()) {
                        (Some(input), Some(fitted)) => (input - fitted).abs() <= px(1.),
                        _ => false,
                    };
                    if !ours {
                        dragged.set(true);
                    }
                }
            });
            Split {
                state,
                input: Rc::new(Cell::new(None)),
                fitted,
                dragged,
                _resized: resized,
            }
        });
        Handles {
            state: split.state.clone(),
            input: split.input.clone(),
            fitted: split.fitted.clone(),
            dragged: split.dragged.clone(),
        }
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
/// that gives it the height. A scrolling body reports the height it lays
/// out at into `measured`, for the split to fit.
fn input_view(
    body: Vec<AnyElement>,
    fills: bool,
    measured: Option<Rc<Cell<Option<Pixels>>>>,
) -> AnyElement {
    let body: AnyElement = match measured.filter(|_| !fills) {
        Some(measured) => div()
            .relative()
            .w_full()
            .child(
                canvas(
                    move |bounds, _, _| measured.set(Some(bounds.size.height)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(body)
            .into_any_element(),
        // A body that fills is a column that hands the height down; a plain
        // block would stop it here and the body would shrink to its content.
        None => div()
            .w_full()
            .when(fills, |wrap| wrap.flex_1().min_h_0().flex().flex_col())
            .children(body)
            .into_any_element(),
    };
    div()
        .id("detail-input")
        .size_full()
        .min_h_0()
        .when(!fills, |view| view.overflow_y_scroll())
        .when(fills, |view| view.flex().flex_col())
        .child(body)
        .test_support()
        .into_any_element()
}

/// Move the separator of `split` to where the input body ends, when that is
/// known, above half the pane at most, unless the user has dragged it.
fn fit(split: &Handles, window: &mut Window, cx: &mut Context<Workspace>) {
    if split.dragged.get() {
        return;
    }
    let Some(height) = split.input.get() else {
        return;
    };
    let (container, current) = {
        let state = split.state.read(cx);
        (state.container_size(), state.sizes().first().copied())
    };
    let Some(current) = current.filter(|_| container > px(0.)) else {
        return;
    };
    let wanted = (height + px(1.)).min(container / 2.).max(PANEL_MIN);
    if (current - wanted).abs() <= px(1.) {
        return;
    }
    split.fitted.set(Some(wanted));
    split
        .state
        .update(cx, |state, cx| state.resize_panel(0, wanted, window, cx));
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
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (Some(response), Some(key)) = (response, key) else {
        let input = input_view(body, fills, None);
        return div().flex_1().min_h_0().child(input).into_any_element();
    };
    let split = ws.splits.of(key, cx);
    fit(&split, window, cx);
    let input = input_view(body, fills, Some(split.input.clone()));
    let state = split.state;
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
