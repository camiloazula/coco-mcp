//! Resource, template and prompt details: arguments with suggestions,
//! subscriptions and the declaration the server listed.

use super::*;

pub(super) fn resource_detail(
    ws: &mut Workspace,
    facts: ResourceFacts,
    meta: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Vec<AnyElement> {
    let t = *tokens(cx);
    let mut info = vec![facts.mime.unwrap_or_else(|| "unknown type".into())];
    if let Some(size) = facts.size {
        info.push(crate::state::size_label(size as usize));
    }
    let vars = Workspace::template_vars(&facts.uri);
    let watch = facts
        .concrete
        .then(|| subscription(ws, &facts.uri, cx))
        .flatten();
    // The title where the server gave one, its name beside the type.
    if facts.title.is_some() {
        info.insert(0, facts.name.clone());
    }
    let title = facts.title.clone().unwrap_or_else(|| facts.name.clone());
    let mut parts: Vec<AnyElement> = vec![header(cx, title, meta).into_any_element()];
    parts.extend(description(cx, facts.description.as_deref()).map(IntoElement::into_any_element));
    parts.push(
        v_flex()
            .px(px(24.))
            .pb(px(16.))
            .gap(px(4.))
            .child(mono(cx, 12., facts.uri.clone()).text_color(t.fg))
            .child(muted(cx, 12., info.join(" · ")))
            .children(watch)
            .into_any_element(),
    );
    parts.push(toolbar(ws, cx, Tabs::Declared, "Read").into_any_element());
    if ws.selection.schema_tab {
        let what = if facts.concrete {
            "resource"
        } else {
            "template"
        };
        parts.push(declaration(ws, facts.declared, what, &facts.uri, cx));
        return parts;
    }
    if !vars.is_empty() {
        let rows: Vec<AnyElement> = vars
            .iter()
            .map(|var| {
                let input = ws.arg_input(var, "", window, cx);
                let control = with_suggestions(
                    ws,
                    var,
                    Input::new(&input)
                        .id(SharedString::from(format!("var-{var}")))
                        .h(px(26.))
                        .into_any_element(),
                    cx,
                );
                arg_row(cx, var, true, control)
            })
            .collect();
        let expanded = ws.expand_template(&facts.uri, cx);
        parts.push(
            v_flex()
                .px(px(24.))
                .py(px(16.))
                .gap(px(10.))
                .max_w(px(640.))
                .children(rows)
                .child(mono(cx, 11., expanded).text_color(t.muted))
                .into_any_element(),
        );
    }
    parts
}

pub(super) fn prompt_detail(
    ws: &mut Workspace,
    prompt: &mcp_core::Prompt,
    meta: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Vec<AnyElement> {
    let title = prompt.title.clone().unwrap_or_else(|| prompt.name.clone());
    let mut parts: Vec<AnyElement> = vec![header(cx, title, meta).into_any_element()];
    parts.extend(description(cx, prompt.description.as_deref()).map(IntoElement::into_any_element));
    parts.push(toolbar(ws, cx, Tabs::Declared, "Get").into_any_element());
    if ws.selection.schema_tab {
        let declared = serde_json::to_value(prompt).unwrap_or_default();
        parts.push(declaration(ws, declared, "prompt", &prompt.name, cx));
        return parts;
    }
    let rows: Vec<AnyElement> = prompt
        .arguments
        .iter()
        .map(|a| {
            let input = ws.arg_input(&a.name, a.description.as_deref().unwrap_or(""), window, cx);
            let control = with_suggestions(
                ws,
                &a.name,
                Input::new(&input)
                    .id(SharedString::from(format!("arg-{}", a.name)))
                    .h(px(26.))
                    .into_any_element(),
                cx,
            );
            let label = a.title.as_deref().unwrap_or(&a.name);
            arg_row(cx, label, a.required == Some(true), control)
        })
        .collect();
    parts.push(
        v_flex()
            .px(px(24.))
            .py(px(16.))
            .gap(px(10.))
            .max_w(px(640.))
            .when(rows.is_empty(), |el| {
                el.child(muted(cx, 12., "No arguments"))
            })
            .children(rows)
            .into_any_element(),
    );
    parts
}

/// Everything the server declared about a resource, template or prompt, as
/// it was listed and foldable, vendor fields and `_meta` included, the way a
/// tool's Schema tab shows a tool.
pub(super) fn declaration(
    ws: &Workspace,
    value: serde_json::Value,
    what: &str,
    name: &str,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let toggle = ws.collapse_toggle(cx);
    let prefix = format!("decl:{}:{what}:{name}", ws.server_scope(cx));
    let value = Rc::new(value);
    let body = json_tree_rc(value.clone(), &ws.folds(&toggle), &prefix, cx);
    v_flex()
        .px(px(24.))
        .py(px(16.))
        .child(tree_section(
            cx,
            what.to_owned(),
            SharedString::from(format!("{prefix}-copy")),
            value,
            body,
        ))
        .into_any_element()
}

/// Subscribe or Unsubscribe for the concrete resource `uri`, with when it
/// last changed and why a subscription was refused. Where subscriptions
/// cannot run, the control stays, disabled, and says why.
pub(super) fn subscription(
    ws: &Workspace,
    uri: &str,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let t = *tokens(cx);
    let (subscribed, updated, refused, cannot) = {
        let state = ws.state.read(cx);
        let server = state.server()?;
        (
            server.is_subscribed(uri),
            server.resource_updated_at(uri).map(str::to_owned),
            server
                .subscription_error
                .clone()
                .filter(|(failed, _)| failed == uri)
                .map(|(_, e)| e),
            server
                .features()
                .get(crate::features::Feature::Subscriptions)
                .reason(),
        )
    };
    let target = uri.to_owned();
    let control = match cannot {
        Some(reason) => crate::views::disabled_control(cx, "subscribe", "Subscribe", reason)
            .test_support()
            .into_any_element(),
        None => text_tab(
            cx,
            if subscribed {
                "Unsubscribe"
            } else {
                "Subscribe"
            },
            false,
        )
        .id("subscribe")
        .text_color(t.accent)
        .on_click(cx.listener(move |ws, _, _, cx| {
            let uri = target.clone();
            ws.state.update(cx, |s, cx| s.toggle_subscription(uri, cx));
        }))
        .test_support()
        .into_any_element(),
    };
    Some(
        h_flex()
            .gap(px(12.))
            .items_center()
            .child(control)
            .children(updated.map(|at| {
                muted(cx, 12., format!("Changed at {at}"))
                    .id("resource-updated")
                    .test_support()
            }))
            .children(refused.map(|e| div().text_size(px(12.)).text_color(t.err).child(e)))
            .into_any_element(),
    )
}

/// An argument input with the server's suggestions for it underneath; a
/// suggestion clicked fills the input.
pub(super) fn with_suggestions(
    ws: &Workspace,
    arg: &str,
    control: AnyElement,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = *tokens(cx);
    let values = ws
        .selection
        .suggestions
        .get(arg)
        .map(|values| {
            values
                .iter()
                .take(MAX_SUGGESTIONS)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let chips: Vec<AnyElement> = values
        .into_iter()
        .enumerate()
        .map(|(i, value)| {
            let arg = arg.to_owned();
            mono(cx, 12., value.clone())
                .id(SharedString::from(format!("suggest-{arg}-{i}")))
                .text_color(t.muted)
                .hover(|s| s.text_color(t.fg))
                .cursor_pointer()
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.choose_suggestion(&arg, value.clone(), window, cx);
                }))
                .test_support()
                .into_any_element()
        })
        .collect();
    v_flex()
        .gap(px(4.))
        .child(control)
        .when(!chips.is_empty(), |el| {
            el.child(h_flex().gap(px(12.)).flex_wrap().children(chips))
        })
        .into_any_element()
}

pub(super) fn arg_row(
    cx: &Context<Workspace>,
    name: &str,
    required: bool,
    control: AnyElement,
) -> AnyElement {
    let t = *tokens(cx);
    h_flex()
        .gap(px(16.))
        .child(
            h_flex()
                .w(px(140.))
                .flex_none()
                .gap(px(4.))
                .child(mono(cx, 12., name.to_owned()).text_color(t.muted))
                .when(required, |el| {
                    el.child(div().text_size(px(12.)).text_color(t.accent).child("*"))
                }),
        )
        .child(div().flex_1().min_w_0().child(control))
        .into_any_element()
}
