//! The body of each kind of server request: elicitation, sampling and roots.

use super::*;

pub(super) fn kind_body(
    ws: &mut Workspace,
    kind: &ServerRequestKind,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = *tokens(cx);
    let Some(ui) = &ws.request_ui else {
        return div().into_any_element();
    };
    match kind {
        ServerRequestKind::Elicitation { message, mode } => {
            let mut parts: Vec<AnyElement> = vec![
                div()
                    .text_size(px(13.))
                    .text_color(t.fg)
                    .child(message.clone())
                    .into_any_element(),
            ];
            match mode {
                ElicitationMode::Form { .. } => {
                    if let Some(form) = ui.form.clone() {
                        let errs = form.read(cx).errors.clone();
                        let fields = form.update(cx, |f, cx| f.render_fields(window, cx));
                        parts.push(fields);
                        if !errs.is_empty() {
                            parts.push(
                                v_flex()
                                    .gap(px(2.))
                                    .text_size(px(12.))
                                    .text_color(t.err)
                                    .children(errs)
                                    .into_any_element(),
                            );
                        }
                    }
                }
                ElicitationMode::Url { url, .. } => {
                    parts.push(
                        mono(cx, 12., url.clone())
                            .text_color(t.fg)
                            .into_any_element(),
                    );
                    parts.push(
                        muted(
                            cx,
                            12.,
                            "The server wants you to finish this in the browser.",
                        )
                        .into_any_element(),
                    );
                }
            }
            v_flex().gap(px(10.)).children(parts).into_any_element()
        }
        ServerRequestKind::Sampling(params) => {
            let messages: Vec<AnyElement> = params["messages"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .enumerate()
                        .map(|(i, msg)| message_row(ws, msg, i, cx))
                        .collect()
                })
                .unwrap_or_default();
            let system = params
                .get("systemPrompt")
                .and_then(Value::as_str)
                .map(|s| s.to_owned());
            let max_tokens = params.get("maxTokens").and_then(Value::as_u64);
            v_flex()
                .gap(px(10.))
                .children(system.map(|s| {
                    v_flex()
                        .gap(px(2.))
                        .child(muted(cx, 11., "System"))
                        .child(div().text_size(px(12.)).child(s))
                }))
                .children(messages)
                .when_some(max_tokens, |el, n| {
                    el.child(muted(cx, 11., format!("maxTokens {n}")))
                })
                .child(field(
                    cx,
                    "model",
                    Input::new(&ui.model).id("req-model").h(px(26.)),
                ))
                .child(field(
                    cx,
                    "reply",
                    Input::new(&ui.text).id("req-text").h(px(26.)),
                ))
                .into_any_element()
        }
        ServerRequestKind::ListRoots => v_flex()
            .gap(px(10.))
            .child(
                div()
                    .text_size(px(13.))
                    .child("The server asks which roots it may access. One file:// URI per line."),
            )
            .child(
                div()
                    .id("req-roots")
                    .h(px(96.))
                    .child(Textarea::new(&ui.roots).h_full())
                    .test_support(),
            )
            .into_any_element(),
    }
}

pub(super) fn message_row(
    ws: &Workspace,
    msg: &Value,
    index: usize,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = *tokens(cx);
    let role = msg["role"].as_str().unwrap_or("user").to_owned();
    let content = &msg["content"];
    let text = match content {
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other["text"].as_str().unwrap_or_default().to_owned(),
    };
    // Anything that is not plain text (images, audio, resource links) keeps
    // its structure instead of being stringified into one line.
    let body = if text.is_empty() {
        let toggle = ws.collapse_toggle(cx);
        json_tree(content, &ws.folds(&toggle), &format!("req:msg{index}"), cx)
    } else {
        div()
            .text_size(px(12.))
            .text_color(t.fg)
            .child(text)
            .into_any_element()
    };
    v_flex()
        .gap(px(2.))
        .child(muted(cx, 11., role))
        .child(body)
        .into_any_element()
}

pub(super) fn field(
    cx: &Context<Workspace>,
    label: &'static str,
    control: impl IntoElement,
) -> AnyElement {
    let t = *tokens(cx);
    h_flex()
        .gap(px(16.))
        .child(
            mono(cx, 12., label)
                .w(px(80.))
                .flex_none()
                .text_color(t.muted),
        )
        .child(div().flex_1().min_w_0().child(control))
        .into_any_element()
}
