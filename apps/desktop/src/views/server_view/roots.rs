//! The Roots section of the Server view: the roots offered when the server
//! asks, editable, and the parsing of the text they are typed in.

use super::*;

/// The roots offered when the server asks, editable. Saving tells a
/// connected legacy server they changed; a 2026-07-28 server has no such
/// notice and gets them when it asks during a request.
pub(super) fn roots_section(
    ws: &mut Workspace,
    server_id: &str,
    roots: Option<Vec<Root>>,
    features: &Features,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let modern = features.get(Feature::Roots).is_deprecated();
    let note = if modern {
        "Offered when the server asks for them during a request, one file:// URI per line. \
         Saving keeps them: 2026-07-28 sends no change notice."
    } else {
        "Offered when the server asks for roots, one file:// URI per line. \
         Saving tells a connected server they changed."
    };
    let editor = roots_editor(ws, server_id, roots.as_deref(), window, cx);
    let target = server_id.to_owned();
    let source = editor.clone();
    let save = accent_button(cx, "Save roots", 24.)
        .id("save-roots")
        .on_click(cx.listener(move |ws, _, _, cx| {
            let roots = parse_roots(&source.read(cx).value());
            ws.state
                .update(cx, |s, cx| s.keep_roots(&target, roots, true, cx));
        }))
        .test_support();
    let body = v_flex()
        .gap(px(8.))
        .max_w(px(640.))
        .child(muted(cx, 12., note))
        .children(modern.then(|| {
            muted(cx, 11., DEPRECATED)
                .id("roots-deprecated")
                .test_support()
        }))
        .child(
            div()
                .id("server-roots")
                .h(px(96.))
                .child(Textarea::new(&editor).h_full())
                .test_support(),
        )
        .child(h_flex().child(save))
        .into_any_element();
    div()
        .px(px(24.))
        .py(px(8.))
        .child(labelled(cx, "Roots", div(), body))
        .into_any_element()
}

/// The roots field for `server_id`, filled from its roots. Built again when
/// the server changes, or when its stored roots arrive after it was built.
fn roots_editor(
    ws: &mut Workspace,
    server_id: &str,
    roots: Option<&[Root]>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<TextareaState> {
    let read = roots.is_some();
    if let Some((id, was_read, editor)) = &ws.roots_editor
        && id == server_id
        && *was_read == read
    {
        return editor.clone();
    }
    let text = roots
        .unwrap_or_default()
        .iter()
        .map(|root| root.uri.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let editor = cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder("file:///path/to/project")
            .default_value(text)
    });
    ws.roots_editor = Some((server_id.to_owned(), read, editor.clone()));
    editor
}

/// Roots from text holding one URI per line.
pub(crate) fn parse_roots(text: &str) -> Vec<Root> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|uri| Root {
            uri: uri.to_owned(),
            name: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_are_read_one_per_line() {
        let roots = parse_roots("file:///a\n\n  file:///b  \n");
        let uris: Vec<&str> = roots.iter().map(|r| r.uri.as_str()).collect();
        assert_eq!(uris, ["file:///a", "file:///b"]);
        assert!(parse_roots("  \n").is_empty());
    }
}
