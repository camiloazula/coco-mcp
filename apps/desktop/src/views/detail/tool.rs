//! A tool's detail: its form, raw arguments or schema, with argument errors
//! above them.

use super::*;

pub(super) fn errors(cx: &Context<Workspace>, errors: &[String]) -> Option<Div> {
    (!errors.is_empty()).then(|| {
        v_flex()
            .px(px(24.))
            .pb(px(12.))
            .gap(px(2.))
            .text_size(px(12.))
            .text_color(tokens(cx).err)
            .children(errors.iter().cloned())
    })
}

pub(super) fn tool_detail(
    ws: &mut Workspace,
    tool: &mcp_core::Tool,
    meta: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Vec<AnyElement> {
    let title = tool.title.clone().unwrap_or_else(|| tool.name.clone());
    let mut parts: Vec<AnyElement> = vec![header(cx, title, meta).into_any_element()];
    parts.extend(description(cx, tool.description.as_deref()).map(IntoElement::into_any_element));
    parts.push(toolbar(ws, cx, Tabs::Tool, "Call").into_any_element());
    if ws.selection.schema_tab {
        // Everything the server declared about the tool, foldable. Nothing
        // else in the app shows the output schema or the annotations.
        let toggle = ws.collapse_toggle(cx);
        let folds = ws.folds(&toggle);
        let scope = ws.server_scope(cx);
        let key = |what: &str| format!("{what}:{scope}:{}", tool.name);
        let mut trees = vec![(
            "inputSchema".to_owned(),
            Rc::new(tool.input_schema.clone()),
            key("schema"),
        )];
        if let Some(output) = &tool.output_schema {
            trees.push((
                "outputSchema".to_owned(),
                Rc::new(output.clone()),
                key("outschema"),
            ));
        }
        if let Some(annotations) = &tool.annotations {
            trees.push((
                "annotations".to_owned(),
                Rc::new(annotations.clone()),
                key("annot"),
            ));
        }
        // Whatever else the server sent on the tool (`_meta`, icons, vendor
        // fields) is part of its declaration, so it is shown too.
        for (name, value) in &tool.extra {
            trees.push((name.clone(), Rc::new(value.clone()), key(name)));
        }
        parts.push(
            v_flex()
                .px(px(24.))
                .py(px(16.))
                .gap(px(12.))
                .children(trees.into_iter().map(|(label, value, prefix)| {
                    let body = json_tree_rc(value.clone(), &folds, &prefix, cx);
                    tree_section(
                        cx,
                        label,
                        SharedString::from(format!("{prefix}-copy")),
                        value,
                        body,
                    )
                }))
                .into_any_element(),
        );
    } else if let Some(form) = ws.selection.form.clone() {
        let (raw, errs) = {
            let f = form.read(cx);
            (f.raw_mode, f.errors.clone())
        };
        let body = form.update(cx, |f, cx| {
            if raw {
                f.render_raw(cx)
            } else {
                f.render_fields(window, cx)
            }
        });
        parts.push(div().px(px(24.)).py(px(16.)).child(body).into_any_element());
        parts.extend(errors(cx, &errs).map(IntoElement::into_any_element));
    } else {
        // No form for this schema: show it as a tree the user can fold.
        let toggle = ws.collapse_toggle(cx);
        let scope = ws.server_scope(cx);
        parts.push(
            div()
                .px(px(24.))
                .py(px(16.))
                .child(json_tree(
                    &tool.input_schema,
                    &ws.folds(&toggle),
                    &format!("schema:{scope}:{}", tool.name),
                    cx,
                ))
                .into_any_element(),
        );
    }
    parts
}
