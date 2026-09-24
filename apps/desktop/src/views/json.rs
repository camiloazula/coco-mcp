//! Collapsible JSON tree in the design's colours: keys `fg`, strings
//! `str`, numbers `num`, punctuation `muted`, chevrons 10px muted in a 14px
//! box, 16px indent per level; collapsed containers show `… N items`.
//!
//! Every tree in the app is interactive. A node's key is its `prefix` plus
//! its JSON path (`log:{server}:{row id}$.tools[0]`), which is both the
//! element id and the entry in the workspace's fold set, so two trees on
//! screen at once never share folds.
//!
//! A tree is a list of its lines. The value is walked once into a plan of
//! lines for the folds in force (`lines.rs`), kept until the value or the
//! folds change, and a uniform list builds only the lines in view from it.
//! So a tree of a hundred thousand rows costs a frame what a tree of ten
//! does, and every container starts open.
//!
//! Every node is also copyable: right-clicking a line offers its value, its
//! path and its key. The menu addresses a node by the steps taken from the
//! root rather than by the printed path, because a key may itself contain
//! `.` or `[`; the path is only ever produced for a person to read.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    AnyElement, App, Div, Hsla, InteractiveElement, IntoElement, ListSizingBehavior, MouseButton,
    ParentElement, Pixels, SharedString, Stateful, StatefulInteractiveElement, Styled, StyledText,
    TestSupportExt as _, UniformListScrollHandle, Window, div, px, uniform_list,
};
use mcp_exchange::{Segment, copy_text, path_string, resolve};
use serde_json::Value;

use crate::clip;
use crate::theme::tokens;

mod lines;

use lines::*;

/// Height of one line: mono 12px at line height 1.6.
pub const LINE_HEIGHT: f32 = 19.2;

/// Rows a tree takes at most when it is not given a height: about half a
/// tall window. A longer tree scrolls inside.
pub const MAX_TREE_ROWS: usize = 30;

/// Called with the key of the chevron that was clicked and whether its node
/// was open at the time.
pub type Toggle = Rc<dyn Fn(String, bool, &mut Window, &mut App)>;

/// What every tree reads to decide which nodes are folded, the callback
/// that changes it, and where the trees keep their scroll positions.
pub struct Folds<'a> {
    /// Nodes folded by the user or by code.
    pub collapsed: &'a HashSet<String>,
    /// Bumped on every fold, so a plan made under other folds is not reused.
    pub rev: u64,
    /// Flips one node.
    pub toggle: &'a Toggle,
    /// The scroll position of every tree, by prefix.
    pub scrolls: &'a TreeScrolls,
}

/// The scroll handle of every tree drawn so far, by prefix, and where each
/// was last seen. A tree that scrolls inside a scrolling panel takes the
/// wheel while it can still move; the handle is what tells whether it did.
#[derive(Default)]
pub struct TreeScrolls(RefCell<HashMap<String, TreeScroll>>);

impl std::fmt::Debug for TreeScrolls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TreeScrolls")
            .field(&self.0.borrow().len())
            .finish()
    }
}

#[derive(Clone)]
struct TreeScroll {
    handle: UniformListScrollHandle,
    /// The position after the last wheel event, clamped to the rows.
    seen: Rc<Cell<Pixels>>,
}

impl TreeScrolls {
    /// The scroll of the tree drawn under `prefix`, made on first sight.
    fn of(&self, prefix: &str) -> TreeScroll {
        self.0
            .borrow_mut()
            .entry(prefix.to_owned())
            .or_insert_with(|| TreeScroll {
                handle: UniformListScrollHandle::new(),
                seen: Rc::new(Cell::new(Pixels::ZERO)),
            })
            .clone()
    }
}

impl TreeScroll {
    /// Whether the tree moved on the wheel event being handled. Its own
    /// scroll listener has run by now, so a position that changed, clamped
    /// to the rows it has, means the wheel was for it; at an end the raw
    /// offset overshoots, clamps back to where it was, and the wheel goes
    /// on to the panel around it.
    fn took_the_wheel(&self) -> bool {
        let state = self.handle.0.borrow();
        let Some(size) = state.last_item_size else {
            return false;
        };
        let lowest = (size.item.height - size.contents.height).min(Pixels::ZERO);
        let now = state.base_handle.offset().y.clamp(lowest, Pixels::ZERO);
        let moved = now != self.seen.get();
        self.seen.set(now);
        moved
    }
}

impl std::fmt::Debug for Folds<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Folds")
            .field("collapsed", &self.collapsed.len())
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}

/// How a tree takes its height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// As many rows as it has, up to [`MAX_TREE_ROWS`], scrolling inside
    /// past that: a tree beside other things.
    Rows,
    /// The height it is given, scrolling inside: the one block of a response.
    Fill,
}

/// Fold key of a child: `{parent}.{key}` for a member, `{parent}[{i}]` for an
/// element. The plan and the chevrons both spell keys through this.
pub fn child_key(parent: &str, segment: &Segment) -> String {
    match segment {
        Segment::Key(k) => format!("{parent}.{k}"),
        Segment::Index(i) => format!("{parent}[{i}]"),
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

/// Tree root keys, each with the value and the fold revision its plan was
/// made under.
type Plans = HashMap<String, (Weak<Value>, u64, Rc<Plan>)>;

thread_local! {
    /// The line plan of each tree, by its root key. A plan walks the whole
    /// value, so a tree drawn on every frame from one value, such as a
    /// response's text parsed once in `Decoded`, plans it once per fold. A
    /// value built afresh for each render (the one clone `json_tree` makes)
    /// plans again, costing what that clone already costs. Plans of values
    /// since dropped are let go.
    static PLANS: RefCell<Plans> = RefCell::new(HashMap::new());
}

/// The plan of the tree keyed `key` over `root` under `folds`, planned only
/// when `root` is not the value it was last planned from or the folds have
/// changed since.
fn planned(root: &Rc<Value>, key: &str, folds: &Folds<'_>) -> Rc<Plan> {
    PLANS.with(|plans| {
        let mut plans = plans.borrow_mut();
        if let Some((value, rev, plan)) = plans.get(key)
            && *rev == folds.rev
            && value
                .upgrade()
                .is_some_and(|value| Rc::ptr_eq(&value, root))
        {
            return plan.clone();
        }
        let plan = Rc::new(plan(root, key, folds.collapsed));
        plans.retain(|_, (value, _, _)| value.strong_count() > 0);
        plans.insert(
            key.to_owned(),
            (Rc::downgrade(root), folds.rev, plan.clone()),
        );
        plan
    })
}

fn chevron(open: bool, key: String, toggle: &Toggle, cx: &App) -> AnyElement {
    let toggle = toggle.clone();
    super::fold_icon(open, cx)
        .id(SharedString::from(key.clone()))
        .cursor_pointer()
        .on_click(move |_, window, cx| toggle(key.clone(), open, window, cx))
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
pub fn json_tree(value: &Value, folds: &Folds<'_>, prefix: &str, fit: Fit, cx: &App) -> AnyElement {
    json_tree_rc(Rc::new(value.clone()), folds, prefix, fit, cx)
}

/// [`json_tree`] over a value the caller already holds in an `Rc`, so a
/// block that also copies the value (`views::tree_section`) shares the one
/// clone instead of making a second.
pub fn json_tree_rc(
    root: Rc<Value>,
    folds: &Folds<'_>,
    prefix: &str,
    fit: Fit,
    cx: &App,
) -> AnyElement {
    let key = format!("{prefix}$");
    let plan = planned(&root, &key, folds);
    let count = plan.lines.len();
    let toggle = folds.toggle.clone();
    let scroll = folds.scrolls.of(prefix);
    let list = uniform_list(SharedString::from(format!("{prefix}-tree")), count, {
        let root = root.clone();
        move |range, _window, cx| {
            range
                .map(|ix| line_element(&plan, ix, &root, &toggle, cx))
                .collect()
        }
    })
    // Its rows' height, up to what it is given.
    .with_sizing_behavior(ListSizingBehavior::Infer)
    .track_scroll(&scroll.handle)
    // A wheel the tree moved on is its own: the panel around it, which
    // would otherwise scroll on the same event, stays put until the tree
    // reaches an end.
    .on_scroll_wheel(move |_, _, cx| {
        if scroll.took_the_wheel() {
            cx.stop_propagation();
        }
    })
    .w_full()
    .font_family(cx.theme().mono_font_family.clone())
    .text_size(px(12.))
    .line_height(px(LINE_HEIGHT))
    .text_color(tokens(cx).fg);
    match fit {
        // Its rows' height, given outright: a list left to infer its height
        // inside a flex column is handed the column's, and draws only the
        // rows that fit the cap while taking far more room than they need.
        Fit::Rows => list
            .flex_none()
            .h(px(LINE_HEIGHT * count.min(MAX_TREE_ROWS) as f32))
            .into_any_element(),
        Fit::Fill => list.flex_1().min_h_0().into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn folds<'a>(
        collapsed: &'a HashSet<String>,
        rev: u64,
        toggle: &'a Toggle,
        scrolls: &'a TreeScrolls,
    ) -> Folds<'a> {
        Folds {
            collapsed,
            rev,
            toggle,
            scrolls,
        }
    }

    #[test]
    fn a_tree_is_planned_once_per_value_and_fold() {
        let toggle: Toggle = Rc::new(|_, _, _, _| {});
        let none = HashSet::new();
        let scrolls = TreeScrolls::default();
        let value = Rc::new(json!([1, 2, {"a": [3]}]));
        let first = planned(&value, "t$", &folds(&none, 0, &toggle, &scrolls));
        assert!(
            Rc::ptr_eq(
                &first,
                &planned(&value, "t$", &folds(&none, 0, &toggle, &scrolls))
            ),
            "kept"
        );
        let other = Rc::new(json!([1, 2, {"a": [3]}]));
        assert!(
            !Rc::ptr_eq(
                &first,
                &planned(&other, "t$", &folds(&none, 0, &toggle, &scrolls))
            ),
            "another value under the same key is planned afresh"
        );
        let refolded = planned(&value, "t$", &folds(&none, 1, &toggle, &scrolls));
        assert!(!Rc::ptr_eq(&first, &refolded), "and so is a fold");
    }
}
