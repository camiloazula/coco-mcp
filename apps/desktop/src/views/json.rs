//! Collapsible JSON tree in the design's colours: keys `fg`, strings
//! `str`, numbers `num`, punctuation `muted`, chevrons 10px muted in a 14px
//! box, 16px indent per level; collapsed containers show `… N items`.
//!
//! Every tree in the app is interactive. A node's key is its `prefix` plus
//! its JSON path (`log:{server}:{row id}$.tools[0]`), which is both the
//! element id and the entry in the workspace's fold sets, so two trees on
//! screen at once never share folds.
//!
//! A tree paints a bounded number of lines: containers past
//! `TREE_NODE_BUDGET` start folded (see `json_budget.rs`), an open
//! container paints its children a budget at a time under a `… N more` line,
//! and a line under the tree says how many nodes both leave out.
//!
//! Every node is also copyable: right-clicking a line offers its value, its
//! path and its key. The menu addresses a node by the steps taken from the
//! root rather than by the printed path, because a key may itself contain
//! `.` or `[`; the path is only ever produced for a person to read.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Div, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Stateful, StatefulInteractiveElement, Styled, StyledText, TestSupportExt as _,
    Window, div, px,
};
use mcp_exchange::{Segment, copy_text, path_string, resolve};
use serde_json::Value;

use crate::clip;
use crate::theme::tokens;
use crate::views::json_budget::{
    Budget, Fold, TREE_NODE_BUDGET, child_key, more_key, nodes, plan, window,
};

mod lines;

use lines::*;

/// Called with the key of the chevron that was clicked and whether its node
/// was open at the time.
pub type Toggle = Rc<dyn Fn(String, bool, &mut Window, &mut App)>;

/// What every tree reads to decide which nodes are folded, and the callback
/// that changes it.
pub struct Folds<'a> {
    /// Nodes folded by the user or by code; a fold here always wins.
    pub collapsed: &'a HashSet<String>,
    /// Nodes the budget folded that the user opened.
    pub unfolded: &'a HashSet<String>,
    /// Flips one node.
    pub toggle: &'a Toggle,
}

impl std::fmt::Debug for Folds<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Folds")
            .field("collapsed", &self.collapsed.len())
            .field("unfolded", &self.unfolded.len())
            .finish_non_exhaustive()
    }
}

struct Run(SharedString, Hsla);

fn styled(runs: Vec<Run>) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for Run(s, color) in runs {
        let start = text.len();
        text.push_str(&s);
        highlights.push((start..text.len(), gpui_kit::HighlightStyle::color(color)));
    }
    StyledText::new(text).with_highlights(highlights)
}

/// Tree root keys, each with the value its plan was made from.
type Plans = HashMap<String, (Weak<Value>, Rc<Budget>)>;

thread_local! {
    /// The line plan of each tree, by its root key, with the value it was
    /// planned from. A plan walks the whole value, so a tree drawn on every
    /// frame from one value, such as a response's text parsed once in
    /// `Decoded`, plans it once. A value built afresh for each render (the
    /// one clone `json_tree` makes) plans again, costing what that clone
    /// already costs. Plans of values since dropped are let go.
    static PLANS: RefCell<Plans> =
        RefCell::new(HashMap::new());
}

/// The plan of the tree keyed `key` over `root`, planned only when `root` is
/// not the value it was last planned from.
fn planned(root: &Rc<Value>, key: &str) -> Rc<Budget> {
    PLANS.with(|plans| {
        let mut plans = plans.borrow_mut();
        if let Some((value, budget)) = plans.get(key)
            && value
                .upgrade()
                .is_some_and(|value| Rc::ptr_eq(&value, root))
        {
            return budget.clone();
        }
        let budget = Rc::new(plan(root, key, TREE_NODE_BUDGET));
        plans.retain(|_, (value, _)| value.strong_count() > 0);
        plans.insert(key.to_owned(), (Rc::downgrade(root), budget.clone()));
        budget
    })
}

struct Ctx<'a> {
    folds: &'a Folds<'a>,
    budget: Rc<Budget>,
    /// Nodes hidden by the budget's folds and by the children an open
    /// container leaves unpainted, among the lines drawn so far.
    hidden: Cell<usize>,
    root: Rc<Value>,
}

impl Ctx<'_> {
    /// Whether the container at `at` is drawn folded.
    fn folded(&self, at: &At) -> bool {
        match self
            .budget
            .fold(&at.key, self.folds.collapsed, self.folds.unfolded)
        {
            Fold::Open => false,
            Fold::Collapsed => true,
            Fold::Budget(nodes) => {
                self.hide(nodes);
                true
            }
        }
    }

    fn hide(&self, nodes: usize) {
        self.hidden.set(self.hidden.get() + nodes);
    }
}

/// Where a line sits: its fold key, and the steps from the root to its value.
#[derive(Clone)]
struct At {
    key: String,
    path: Vec<Segment>,
}

impl At {
    fn child(&self, segment: Segment) -> Self {
        let key = child_key(&self.key, &segment);
        let mut path = self.path.clone();
        path.push(segment);
        Self { key, path }
    }
}

fn chevron(open: bool, key: String, ctx: &Ctx<'_>, cx: &App) -> AnyElement {
    let toggle = ctx.folds.toggle.clone();
    super::fold_icon(open, cx)
        .id(SharedString::from(key.clone()))
        .cursor_pointer()
        .on_click(move |_, window, cx| toggle(key.clone(), open, window, cx))
        .test_support()
        .into_any_element()
}

/// The line under the painted children of the open container `key` when it
/// has more: how many are left, and a press paints the next budget of them.
fn more(
    indent: usize,
    key: &str,
    shown: usize,
    left: usize,
    noun: &str,
    ctx: &Ctx<'_>,
    cx: &App,
) -> AnyElement {
    let t = *tokens(cx);
    let key = more_key(key, shown);
    let toggle = ctx.folds.toggle.clone();
    let next = left.min(TREE_NODE_BUDGET);
    h_flex()
        .id(SharedString::from(key.clone()))
        .pl(px(16. * indent as f32))
        .whitespace_nowrap()
        .text_color(t.muted)
        .hover(|s| s.text_color(t.fg))
        .cursor_pointer()
        .child(div().w(px(14.)).flex_none())
        .child(format!("… {left} more {noun} · show {next}"))
        .on_click(move |_, window, cx| toggle(key.clone(), false, window, cx))
        .test_support()
        .into_any_element()
}

/// What right-clicking one node offers.
fn entries(root: &Value, path: &[Segment]) -> Vec<clip::MenuEntry> {
    let value = resolve(root, path);
    let mut entries = vec![
        clip::MenuEntry::new(
            "Copy value",
            "value",
            value.map(copy_text).unwrap_or_default(),
        ),
        clip::MenuEntry::new(
            "Copy as one line",
            "value",
            value.map(Value::to_string).unwrap_or_default(),
        ),
        clip::MenuEntry::new("Copy path", "path", path_string(path)),
    ];
    if let Some(Segment::Key(key)) = path.last() {
        entries.push(clip::MenuEntry::new("Copy key", "key", key.clone()));
    }
    entries
}

/// Attach the copy menu to one node's line.
///
/// Observed first: a leaf line has no chevron, so without this the line is
/// the only thing a test could aim at and it would not be registered.
fn copy_menu(row: Stateful<Div>, root: Rc<Value>, path: Vec<Segment>) -> AnyElement {
    row.test_support()
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            clip::open_menu(entries(&root, &path), event.position, cx);
        })
        .into_any_element()
}

/// Render `value` as a tree of mono 12px lines (line height 1.6).
/// `folds` holds the folded node keys and the callback the chevrons call.
/// `prefix` namespaces this tree's keys and element ids, so it must be
/// unique among the trees that can be on screen together and stable across
/// renders for the folds to survive.
///
/// The value is cloned once per tree so the copy menus can resolve a node
/// after the render that built them has ended.
pub fn json_tree(value: &Value, folds: &Folds<'_>, prefix: &str, cx: &App) -> AnyElement {
    json_tree_rc(Rc::new(value.clone()), folds, prefix, cx)
}

/// [`json_tree`] over a value the caller already holds in an `Rc`, so a
/// block that also copies the value (`views::tree_section`) shares the one
/// clone instead of making a second.
pub fn json_tree_rc(root: Rc<Value>, folds: &Folds<'_>, prefix: &str, cx: &App) -> AnyElement {
    let key = format!("{prefix}$");
    let budget = planned(&root, &key);
    let ctx = Ctx {
        folds,
        budget,
        hidden: Cell::new(0),
        root: root.clone(),
    };
    let mut lines = Vec::new();
    push(
        &mut lines,
        &root,
        At {
            key,
            path: Vec::new(),
        },
        0,
        Vec::new(),
        "",
        &ctx,
        cx,
    );
    let hidden = ctx.hidden.get();
    let note = (hidden > 0).then(|| {
        let noun = if hidden == 1 { "node" } else { "nodes" };
        div()
            .id(SharedString::from(format!("{prefix}-folded")))
            .pl(px(14.))
            .text_color(tokens(cx).muted)
            .child(format!(
                "{hidden} {noun} folded to keep this tree responsive"
            ))
            .test_support()
    });
    v_flex()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(12.))
        .line_height(px(19.2))
        .text_color(tokens(cx).fg)
        .children(lines)
        .children(note)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tree_is_planned_once_per_value() {
        let value = Rc::new(json!([1, 2, {"a": [3]}]));
        let first = planned(&value, "t$");
        assert!(Rc::ptr_eq(&first, &planned(&value, "t$")), "kept");
        let other = Rc::new(json!([1, 2, {"a": [3]}]));
        assert!(
            !Rc::ptr_eq(&first, &planned(&other, "t$")),
            "another value under the same key is planned afresh"
        );
    }
}
