//! How much of a JSON tree is painted before its containers start folded.
//!
//! A tree draws one element per line, which is nothing for a tool schema and
//! a frozen window for a result with twenty thousand rows. So before drawing,
//! a tree plans its folds from the value alone: walking in document order,
//! a container whose lines would carry the tree past the budget starts
//! folded. The plan never reads the fold sets, so a node keeps its meaning
//! when the user opens or folds an earlier one.
//!
//! Opening a fold cannot bring the frozen window back either: an open
//! container paints its children a budget at a time, and a `… N more` line
//! under them paints the next budget when pressed (see [`window`]).
//!
//! Hand-written: this is a walk over `serde_json::Value`, not something a
//! crate provides.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use mcp_exchange::Segment;
use serde_json::Value;

/// Lines a JSON tree paints before later containers start folded: about
/// 2_000 mono lines is about 38_000 px, many screens of content, and still
/// lays out inside a frame.
pub const TREE_NODE_BUDGET: usize = 2_000;

/// Fold key of a child: `{parent}.{key}` for a member, `{parent}[{i}]` for an
/// element. The tree and the plan both spell keys through this, so a fold the
/// plan decides is the fold the tree draws.
pub fn child_key(parent: &str, segment: &Segment) -> String {
    match segment {
        Segment::Key(k) => format!("{parent}.{k}"),
        Segment::Index(i) => format!("{parent}[{i}]"),
    }
}

/// How one container is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// Its children are painted.
    Open,
    /// Folded by the user, or by code such as the request dialog.
    Collapsed,
    /// Folded by the budget; the number is how many nodes it hides.
    Budget(usize),
}

/// The containers one tree starts folded.
#[derive(Debug, Default)]
pub struct Budget {
    /// Key of each container the budget folds, with the nodes under it.
    folded: HashMap<String, usize>,
    /// Nodes under the children an open container leaves unpainted, by its
    /// key and how many it paints, counted once per plan.
    tails: RefCell<HashMap<(String, usize), usize>>,
}

impl Budget {
    /// How the container keyed `key` is drawn. A fold in `collapsed` always
    /// wins; `unfolded` only opens what the budget folded.
    pub fn fold(&self, key: &str, collapsed: &HashSet<String>, unfolded: &HashSet<String>) -> Fold {
        if collapsed.contains(key) {
            return Fold::Collapsed;
        }
        match self.folded.get(key) {
            Some(&nodes) if !unfolded.contains(key) => Fold::Budget(nodes),
            _ => Fold::Open,
        }
    }

    /// The nodes under the children of the open container `key` from `shown`
    /// on, counted by `count` the first time they are asked for.
    pub fn tail(&self, key: &str, shown: usize, count: impl FnOnce() -> usize) -> usize {
        *self
            .tails
            .borrow_mut()
            .entry((key.to_owned(), shown))
            .or_insert_with(count)
    }
}

/// Plan the folds of `value`, whose root is keyed `root`, so that no more than
/// about `budget` lines are painted.
///
/// A leaf or an empty container is one line. A container with `n` children
/// takes at least `n + 2`, so it starts folded when that would pass the
/// budget, and costs its one summary line instead. The children of a folded
/// container are planned as if it were a tree of its own, so the containers
/// among them fold again. Its leaves cannot fold, which is why an opened
/// container also paints no more than [`window`] of its children: opening a
/// fold paints at most about twice the budget, however wide it is.
pub fn plan(value: &Value, root: &str, budget: usize) -> Budget {
    let mut planned = Budget::default();
    let mut lines = 0;
    node(
        value,
        &|| root.to_owned(),
        budget,
        &mut lines,
        &mut planned.folded,
    );
    planned
}

/// Plan one node, adding its lines to `lines`. The key is built only for a
/// container, the one kind of node that can fold. Returns the node itself
/// plus the nodes under it.
///
/// The key is a trait object rather than a generic closure: each level would
/// otherwise instantiate `node` for the closure type of the level above.
fn node(
    value: &Value,
    key: &dyn Fn() -> String,
    budget: usize,
    lines: &mut usize,
    folded: &mut HashMap<String, usize>,
) -> usize {
    let children = match value {
        Value::Object(map) => map.len(),
        Value::Array(items) => items.len(),
        _ => 0,
    };
    if children == 0 {
        *lines += 1;
        return 1;
    }
    let key = key();
    let open = *lines + children + 2 <= budget;
    let mut own = 0;
    let counted = if open { &mut *lines } else { &mut own };
    *counted += 1;
    let mut under = 0;
    match value {
        Value::Object(map) => {
            for (k, child) in map {
                let spell = || child_key(&key, &Segment::Key(k.clone()));
                under += node(child, &spell, budget, counted, folded);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                let spell = || child_key(&key, &Segment::Index(i));
                under += node(child, &spell, budget, counted, folded);
            }
        }
        _ => {}
    }
    *counted += 1;
    if !open {
        *lines += 1;
        folded.insert(key, under);
    }
    1 + under
}

/// Key under which an open container `key` keeps that its children from
/// `from` on are painted too. `[+` cannot start an index, so the key never
/// names a child.
pub fn more_key(key: &str, from: usize) -> String {
    format!("{key}[+{from}]")
}

/// How many of the `len` children of the open container `key` are painted:
/// [`TREE_NODE_BUDGET`], and a budget more for each `… more` line pressed,
/// which lands its [`more_key`] in `unfolded`.
pub fn window(key: &str, len: usize, unfolded: &HashSet<String>) -> usize {
    let mut shown = len.min(TREE_NODE_BUDGET);
    while shown < len && unfolded.contains(&more_key(key, shown)) {
        shown = len.min(shown + TREE_NODE_BUDGET);
    }
    shown
}

/// Nodes in `value`: itself and everything beneath it.
pub fn nodes(value: &Value) -> usize {
    1 + match value {
        Value::Object(map) => map.values().map(nodes).sum(),
        Value::Array(items) => items.iter().map(nodes).sum(),
        _ => 0,
    }
}

/// Record a click on the chevron of `key`, which was `open` when clicked.
///
/// Closing always lands in `collapsed`, so it wins over anything the budget
/// decides; opening lands in `unfolded`, which is what lets a budget fold open.
pub fn apply_toggle(
    key: String,
    open: bool,
    collapsed: &mut HashSet<String>,
    unfolded: &mut HashSet<String>,
) {
    if open {
        unfolded.remove(&key);
        collapsed.insert(key);
    } else {
        collapsed.remove(&key);
        unfolded.insert(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    fn fold(budget: &Budget, key: &str) -> Fold {
        budget.fold(key, &none(), &none())
    }

    #[test]
    fn a_small_value_folds_nothing() {
        let value = json!({"a": [1, 2, {"b": null}], "c": {}, "d": "text"});
        let budget = plan(&value, "t$", TREE_NODE_BUDGET);
        for key in ["t$", "t$.a", "t$.a[2]", "t$.c"] {
            assert_eq!(fold(&budget, key), Fold::Open, "{key}");
        }
        assert!(budget.folded.is_empty());
    }

    #[test]
    fn a_long_root_array_starts_folded() {
        let value = Value::Array((0..5_000).map(|i| json!(i)).collect());
        let budget = plan(&value, "t$", 2_000);
        assert_eq!(fold(&budget, "t$"), Fold::Budget(5_000));
    }

    #[test]
    fn containers_past_the_budget_fold_in_document_order() {
        let long = || Value::Array((0..1_500).map(|i| json!(i)).collect());
        let value = json!({"a": long(), "b": long(), "c": long()});
        let budget = plan(&value, "t$", 2_000);
        assert_eq!(fold(&budget, "t$"), Fold::Open);
        assert_eq!(fold(&budget, "t$.a"), Fold::Open);
        assert_eq!(fold(&budget, "t$.b"), Fold::Budget(1_500));
        assert_eq!(fold(&budget, "t$.c"), Fold::Budget(1_500));
        let hidden: usize = ["t$.b", "t$.c"]
            .iter()
            .map(|key| match fold(&budget, key) {
                Fold::Budget(nodes) => nodes,
                _ => 0,
            })
            .sum();
        assert_eq!(hidden, 3_000);
    }

    #[test]
    fn opening_a_budget_fold_paints_another_budget() {
        let value = Value::Array((0..5_000).map(|i| json!([i, i])).collect());
        let budget = plan(&value, "t$", 2_000);
        // Every element and both of its numbers.
        assert_eq!(fold(&budget, "t$"), Fold::Budget(15_000));
        assert_eq!(fold(&budget, "t$[0]"), Fold::Open);
        assert_eq!(fold(&budget, "t$[4999]"), Fold::Budget(2));
    }

    #[test]
    fn an_opened_wide_container_paints_a_budget_of_children_at_a_time() {
        let value = Value::Array((0..30_000).map(|i| json!(i)).collect());
        let budget = plan(&value, "t$", TREE_NODE_BUDGET);
        let mut unfolded = HashSet::from(["t$".to_owned()]);
        assert_eq!(budget.fold("t$", &none(), &unfolded), Fold::Open);
        // Thirty thousand leaves, none of which can fold: only a budget of
        // them is painted, and the rest are counted as hidden.
        assert_eq!(window("t$", 30_000, &unfolded), TREE_NODE_BUDGET);
        let rest: usize = value.as_array().unwrap()[TREE_NODE_BUDGET..]
            .iter()
            .map(nodes)
            .sum();
        assert_eq!(rest, 28_000);
        // Each `… more` line pressed paints another budget, in order.
        unfolded.insert(more_key("t$", 2 * TREE_NODE_BUDGET));
        assert_eq!(window("t$", 30_000, &unfolded), TREE_NODE_BUDGET);
        unfolded.insert(more_key("t$", TREE_NODE_BUDGET));
        assert_eq!(window("t$", 30_000, &unfolded), 3 * TREE_NODE_BUDGET);
        // A narrow container is painted whole, and the last window stops at
        // its end.
        assert_eq!(window("t$", 10, &none()), 10);
        let all: HashSet<String> = (1..15)
            .map(|i| more_key("t$", i * TREE_NODE_BUDGET))
            .collect();
        assert_eq!(window("t$", 30_000, &all), 30_000);
        assert_eq!(nodes(&json!({"a": [1, {"b": null}], "c": {}})), 6);
    }

    #[test]
    fn a_tail_is_counted_once_per_plan() {
        let budget = plan(&json!([1, 2, 3]), "t$", TREE_NODE_BUDGET);
        assert_eq!(budget.tail("t$", 1, || 2), 2);
        assert_eq!(budget.tail("t$", 1, || unreachable!("counted again")), 2);
        assert_eq!(budget.tail("t$", 2, || 1), 1, "another window counts anew");
    }

    #[test]
    fn keys_are_spelled_the_way_the_tree_spells_them() {
        assert_eq!(child_key("t$", &Segment::Key("a.b".into())), "t$.a.b");
        assert_eq!(child_key("t$.a.b", &Segment::Index(0)), "t$.a.b[0]");
        assert_eq!(more_key("t$.a.b", 2_000), "t$.a.b[+2000]");
        // A key containing `.` and an index, folded by a tiny budget.
        let value = json!({"a.b": [[1, 2], 3]});
        let budget = plan(&value, "t$", 3);
        assert_eq!(fold(&budget, "t$"), Fold::Open);
        assert_eq!(fold(&budget, "t$.a.b"), Fold::Budget(4));
        assert_eq!(fold(&budget, "t$.a.b[0]"), Fold::Budget(2));
    }

    #[test]
    fn collapsed_wins_and_unfolded_only_opens_budget_folds() {
        let value = Value::Array((0..10).map(|i| json!([i])).collect());
        let budget = plan(&value, "t$", 5);
        let root = HashSet::from(["t$".to_owned()]);
        assert_eq!(budget.fold("t$", &none(), &none()), Fold::Budget(20));
        assert_eq!(budget.fold("t$", &none(), &root), Fold::Open);
        assert_eq!(budget.fold("t$", &root, &root), Fold::Collapsed);
        // A node the budget left open is not folded by `unfolded`; only
        // `collapsed` folds it.
        let open = plan(&json!([1]), "t$", 5);
        assert_eq!(open.fold("t$", &none(), &root), Fold::Open);
        assert_eq!(open.fold("t$", &root, &none()), Fold::Collapsed);
    }

    #[test]
    fn a_click_flips_either_kind_of_fold() {
        let long = plan(&Value::Array((0..10).map(|i| json!(i)).collect()), "t$", 5);
        let short = plan(&json!([1]), "t$", 5);
        for budget in [&long, &short] {
            let (mut collapsed, mut unfolded) = (none(), none());
            let drawn = |c: &HashSet<String>, u: &HashSet<String>| budget.fold("t$", c, u);
            let first = drawn(&collapsed, &unfolded);
            // The chevron passes whether the node was open when clicked.
            apply_toggle(
                "t$".into(),
                first == Fold::Open,
                &mut collapsed,
                &mut unfolded,
            );
            let second = drawn(&collapsed, &unfolded);
            assert_ne!(first == Fold::Open, second == Fold::Open);
            apply_toggle(
                "t$".into(),
                second == Fold::Open,
                &mut collapsed,
                &mut unfolded,
            );
            let third = drawn(&collapsed, &unfolded);
            assert_eq!(first == Fold::Open, third == Fold::Open);
        }
    }
}
