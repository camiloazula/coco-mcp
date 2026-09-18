//! Laying a JSON tree out line by line, within the node budget.

use super::*;

/// One line. `at` is `None` for a closing bracket, which belongs to the node
/// its opening line already represents.
pub(super) fn line(
    indent: usize,
    lead: Option<AnyElement>,
    runs: Vec<Run>,
    at: Option<(&At, &Ctx<'_>)>,
) -> AnyElement {
    let row = h_flex()
        .pl(px(16. * indent as f32))
        .whitespace_nowrap()
        .children(lead.or_else(|| Some(div().w(px(14.)).flex_none().into_any_element())))
        .child(styled(runs));
    match at {
        Some((at, ctx)) => copy_menu(
            row.id(SharedString::from(format!("row{}", at.key))),
            ctx.root.clone(),
            at.path.clone(),
        ),
        None => row.into_any_element(),
    }
}

// One line of a tree is drawn from the tree's whole context (folds, menu,
// budget, scope); a struct for a single call site would only rename it.
#[allow(clippy::too_many_arguments)]
pub(super) fn push(
    out: &mut Vec<AnyElement>,
    value: &Value,
    at: At,
    indent: usize,
    prefix: Vec<Run>,
    trailing: &str,
    ctx: &Ctx<'_>,
    cx: &App,
) {
    let t = *tokens(cx);
    let p = |s: &str| Run(SharedString::from(s.to_owned()), t.muted);
    match value {
        Value::Object(map) if !map.is_empty() => {
            let mut head = prefix;
            if ctx.folded(&at) {
                head.push(p("{ "));
                head.push(p(&format!("… {} keys", map.len())));
                head.push(p(&format!(" }}{trailing}")));
                let lead = chevron(false, at.key.clone(), ctx, cx);
                out.push(line(indent, Some(lead), head, Some((&at, ctx))));
                return;
            }
            head.push(p("{"));
            let lead = chevron(true, at.key.clone(), ctx, cx);
            out.push(line(indent, Some(lead), head, Some((&at, ctx))));
            let n = map.len();
            let shown = window(&at.key, n, ctx.folds.unfolded);
            for (i, (k, v)) in map.iter().take(shown).enumerate() {
                let key = vec![Run(SharedString::from(format!("\"{k}\"")), t.fg), p(": ")];
                let trailing = if i + 1 < n { "," } else { "" };
                push(
                    out,
                    v,
                    at.child(Segment::Key(k.clone())),
                    indent + 1,
                    key,
                    trailing,
                    ctx,
                    cx,
                );
            }
            if shown < n {
                let tail = || map.values().skip(shown).map(nodes).sum();
                ctx.hide(ctx.budget.tail(&at.key, shown, tail));
                out.push(more(indent + 1, &at.key, shown, n - shown, "keys", ctx, cx));
            }
            out.push(line(indent, None, vec![p(&format!("}}{trailing}"))], None));
        }
        Value::Array(items) if !items.is_empty() => {
            let mut head = prefix;
            if ctx.folded(&at) {
                head.push(p("[ "));
                head.push(p(&format!("… {} items", items.len())));
                head.push(p(&format!(" ]{trailing}")));
                let lead = chevron(false, at.key.clone(), ctx, cx);
                out.push(line(indent, Some(lead), head, Some((&at, ctx))));
                return;
            }
            head.push(p("["));
            let lead = chevron(true, at.key.clone(), ctx, cx);
            out.push(line(indent, Some(lead), head, Some((&at, ctx))));
            let n = items.len();
            let shown = window(&at.key, n, ctx.folds.unfolded);
            for (i, v) in items.iter().take(shown).enumerate() {
                let trailing = if i + 1 < n { "," } else { "" };
                push(
                    out,
                    v,
                    at.child(Segment::Index(i)),
                    indent + 1,
                    Vec::new(),
                    trailing,
                    ctx,
                    cx,
                );
            }
            if shown < n {
                let tail = || items[shown..].iter().map(nodes).sum();
                ctx.hide(ctx.budget.tail(&at.key, shown, tail));
                out.push(more(
                    indent + 1,
                    &at.key,
                    shown,
                    n - shown,
                    "items",
                    ctx,
                    cx,
                ));
            }
            out.push(line(indent, None, vec![p(&format!("]{trailing}"))], None));
        }
        _ => {
            let mut runs = prefix;
            let (text, color) = match value {
                Value::String(s) => (format!("\"{}\"", s.replace('"', "\\\"")), t.str),
                Value::Number(n) => (n.to_string(), t.num),
                Value::Bool(b) => (b.to_string(), t.num),
                Value::Null => ("null".to_string(), t.muted),
                Value::Object(_) => ("{}".to_string(), t.muted),
                Value::Array(_) => ("[]".to_string(), t.muted),
            };
            runs.push(Run(SharedString::from(text), color));
            if !trailing.is_empty() {
                runs.push(p(trailing));
            }
            out.push(line(indent, None, runs, Some((&at, ctx))));
        }
    }
}
