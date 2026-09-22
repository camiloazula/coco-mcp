//! The line plan of a JSON tree, and one line of it drawn.
//!
//! A plan holds one entry per line the tree would show under its folds:
//! where the line sits and which node it is, not what it says. What it says
//! is read from the value when the line is in view, so a plan of a hundred
//! thousand lines is a few megabytes and is made in a fraction of a second.
//!
//! Hand-written: this is a walk over `serde_json::Value`, not something a
//! crate provides.

use super::*;

/// What a line stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// The opening line of a container: its bracket, or the whole of it
    /// folded.
    Open { folded: bool },
    /// The closing bracket of an open container, which belongs to the node
    /// its opening line stands for.
    Close,
    /// A leaf, or an empty container.
    Leaf,
}

/// One line of a tree.
#[derive(Debug)]
pub(super) struct Line {
    /// Levels under the root.
    pub indent: u16,
    /// The opening line of the container this line is under; the root's is
    /// itself, and a closing line's is its own opening line.
    pub parent: u32,
    /// The step from the container's value to this line's; `None` for the
    /// root and for a closing line.
    pub segment: Option<Segment>,
    pub kind: Kind,
    /// Whether the node is the last of its container, so its line ends
    /// without a comma.
    pub last: bool,
}

/// The lines of one tree under the folds it was planned with.
#[derive(Debug, Default)]
pub(super) struct Plan {
    /// The tree's root key, which every fold key starts from.
    pub key: String,
    pub lines: Vec<Line>,
}

impl Plan {
    /// The steps from the root to the node of line `ix`.
    pub fn path(&self, ix: usize) -> Vec<Segment> {
        let mut path = Vec::new();
        let mut at = ix;
        loop {
            let line = &self.lines[at];
            if let Some(segment) = &line.segment {
                path.push(segment.clone());
            }
            let parent = line.parent as usize;
            if parent == at {
                break;
            }
            at = parent;
        }
        path.reverse();
        path
    }

    /// The fold key of the node of line `ix`.
    pub fn key_of(&self, path: &[Segment]) -> String {
        path.iter()
            .fold(self.key.clone(), |key, segment| child_key(&key, segment))
    }
}

/// Plan the tree keyed `key` over `value`, with the containers in
/// `collapsed` folded.
pub(super) fn plan(value: &Value, key: &str, collapsed: &HashSet<String>) -> Plan {
    let mut plan = Plan {
        key: key.to_owned(),
        lines: Vec::new(),
    };
    push(&mut plan.lines, value, key, 0, None, 0, true, collapsed);
    plan
}

fn is_container(value: &Value) -> bool {
    match value {
        Value::Object(map) => !map.is_empty(),
        Value::Array(items) => !items.is_empty(),
        _ => false,
    }
}

// One node of a tree is planned from the tree's whole context; a struct for
// a single call site would only rename it.
#[allow(clippy::too_many_arguments)]
fn push(
    lines: &mut Vec<Line>,
    value: &Value,
    key: &str,
    parent: u32,
    segment: Option<Segment>,
    indent: u16,
    last: bool,
    collapsed: &HashSet<String>,
) {
    let ix = lines.len() as u32;
    // The root is its own parent, which ends a walk up.
    let parent = if segment.is_none() { ix } else { parent };
    if !is_container(value) {
        lines.push(Line {
            indent,
            parent,
            segment,
            kind: Kind::Leaf,
            last,
        });
        return;
    }
    let folded = collapsed.contains(key);
    lines.push(Line {
        indent,
        parent,
        segment,
        kind: Kind::Open { folded },
        last,
    });
    if folded {
        return;
    }
    // A key is spelled only for a child that can fold.
    let child = |lines: &mut Vec<Line>, child: &Value, segment: Segment, last: bool| {
        let key = if is_container(child) {
            child_key(key, &segment)
        } else {
            String::new()
        };
        push(
            lines,
            child,
            &key,
            ix,
            Some(segment),
            indent + 1,
            last,
            collapsed,
        );
    };
    match value {
        Value::Object(map) => {
            let n = map.len();
            for (i, (k, v)) in map.iter().enumerate() {
                child(lines, v, Segment::Key(k.clone()), i + 1 == n);
            }
        }
        Value::Array(items) => {
            let n = items.len();
            for (i, v) in items.iter().enumerate() {
                child(lines, v, Segment::Index(i), i + 1 == n);
            }
        }
        _ => {}
    }
    lines.push(Line {
        indent,
        parent: ix,
        segment: None,
        kind: Kind::Close,
        last,
    });
}

/// One line as a row: its lead (a chevron or its space) and its runs.
fn row(indent: u16, lead: Option<AnyElement>, runs: Vec<Run>) -> Div {
    h_flex()
        .h(px(LINE_HEIGHT))
        .pl(px(16. * f32::from(indent)))
        .whitespace_nowrap()
        .children(lead.or_else(|| Some(div().w(px(14.)).flex_none().into_any_element())))
        .child(styled(runs))
}

/// The leaf `value` as text, in its colour.
fn leaf(value: &Value, cx: &App) -> Run {
    let t = *tokens(cx);
    let (text, color) = match value {
        Value::String(s) => (format!("\"{}\"", s.replace('"', "\\\"")), t.str),
        Value::Number(n) => (n.to_string(), t.num),
        Value::Bool(b) => (b.to_string(), t.num),
        Value::Null => ("null".to_string(), t.muted),
        Value::Object(_) => ("{}".to_string(), t.muted),
        Value::Array(_) => ("[]".to_string(), t.muted),
    };
    Run(SharedString::from(text), color)
}

/// Line `ix` of `plan`, drawn from `root`.
pub(super) fn line_element(
    plan: &Plan,
    ix: usize,
    root: &Rc<Value>,
    toggle: &Toggle,
    cx: &App,
) -> AnyElement {
    let t = *tokens(cx);
    let p = |s: &str| Run(SharedString::from(s.to_owned()), t.muted);
    let line = &plan.lines[ix];
    let path = plan.path(ix);
    let trailing = if line.last { "" } else { "," };
    let value = resolve(root, &path);
    if line.kind == Kind::Close {
        let bracket = match value {
            Some(Value::Array(_)) => "]",
            _ => "}",
        };
        return row(line.indent, None, vec![p(&format!("{bracket}{trailing}"))]).into_any_element();
    }
    let mut runs = Vec::new();
    if let Some(Segment::Key(k)) = &line.segment {
        runs.push(Run(SharedString::from(format!("\"{k}\"")), t.fg));
        runs.push(p(": "));
    }
    let key = plan.key_of(&path);
    let lead = match (line.kind, value) {
        (Kind::Open { folded }, Some(container)) => {
            let (open, close, count, noun) = match container {
                Value::Object(map) => ("{", "}", map.len(), "keys"),
                Value::Array(items) => ("[", "]", items.len(), "items"),
                other => {
                    runs.push(leaf(other, cx));
                    runs.push(p(trailing));
                    return copy_menu(
                        row(line.indent, None, runs).id(SharedString::from(format!("row{key}"))),
                        root.clone(),
                        path,
                    );
                }
            };
            if folded {
                runs.push(p(&format!("{open} ")));
                runs.push(p(&format!("… {count} {noun}")));
                runs.push(p(&format!(" {close}{trailing}")));
            } else {
                runs.push(p(open));
            }
            Some(chevron(!folded, key.clone(), toggle, cx))
        }
        (_, Some(value)) => {
            runs.push(leaf(value, cx));
            if !trailing.is_empty() {
                runs.push(p(trailing));
            }
            None
        }
        // A node the value no longer has: the plan is of another value and
        // is about to be replaced.
        (_, None) => None,
    };
    copy_menu(
        row(line.indent, lead, runs).id(SharedString::from(format!("row{key}"))),
        root.clone(),
        path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kinds(plan: &Plan) -> Vec<(u16, Kind, bool)> {
        plan.lines
            .iter()
            .map(|l| (l.indent, l.kind, l.last))
            .collect()
    }

    #[test]
    fn a_plan_has_a_line_per_bracket_and_leaf() {
        let value = json!({"a": [1, 2], "b": {}, "c": "text"});
        let plan = plan(&value, "t$", &HashSet::new());
        let open = |folded| Kind::Open { folded };
        assert_eq!(
            kinds(&plan),
            vec![
                (0, open(false), true),
                (1, open(false), false),
                (2, Kind::Leaf, false),
                (2, Kind::Leaf, true),
                (1, Kind::Close, false),
                (1, Kind::Leaf, false),
                (1, Kind::Leaf, true),
                (0, Kind::Close, true),
            ]
        );
        assert_eq!(
            plan.path(3),
            vec![Segment::Key("a".into()), Segment::Index(1)]
        );
        assert_eq!(plan.key_of(&plan.path(3)), "t$.a[1]");
        assert_eq!(plan.path(4), plan.path(1), "a closing line is its node's");
        assert_eq!(plan.path(0), Vec::<Segment>::new());
        assert_eq!(plan.key_of(&plan.path(0)), "t$");
    }

    #[test]
    fn a_folded_container_is_one_line() {
        let value = json!({"a": [1, 2], "b": [3]});
        let folded = HashSet::from(["t$.a".to_owned()]);
        let plan = plan(&value, "t$", &folded);
        assert_eq!(
            kinds(&plan),
            vec![
                (0, Kind::Open { folded: false }, true),
                (1, Kind::Open { folded: true }, false),
                (1, Kind::Open { folded: false }, true),
                (2, Kind::Leaf, true),
                (1, Kind::Close, true),
                (0, Kind::Close, true),
            ]
        );
        let root = HashSet::from(["t$".to_owned()]);
        assert_eq!(plan_len(&value, &root), 1, "a folded root is its one line");
    }

    fn plan_len(value: &Value, collapsed: &HashSet<String>) -> usize {
        plan(value, "t$", collapsed).lines.len()
    }

    #[test]
    fn a_wide_array_plans_in_a_moment() {
        let value = Value::Array((0..100_000).map(|i| json!({"id": i, "ok": true})).collect());
        let started = std::time::Instant::now();
        let n = plan_len(&value, &HashSet::new());
        assert_eq!(n, 2 + 100_000 * 4);
        assert!(started.elapsed().as_secs() < 2, "{:?}", started.elapsed());
    }
}
