//! Content blocks: text by its mime type, images and audio, and resource
//! links.

use super::*;

use gpui_kit::{InteractiveElement, TestSupportExt as _};

/// One MCP content block (`text`, `image`, `audio`, `resource`,
/// `resource_link`), and whether it was drawn as plain text. `lone` says it
/// is the only block, so plain text may fill the height given.
pub(super) fn content_block(
    block: &Value,
    prefix: &str,
    draw: &mut Draw<'_>,
    lone: bool,
    cx: &App,
) -> (AnyElement, bool) {
    let element = match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
            return render_text(text, "", prefix, draw, lone, cx);
        }
        Some("image") => {
            let data = block.get("data").and_then(Value::as_str).unwrap_or("");
            let mime = block
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            render_blob(data, mime, "image", prefix, draw.decoded, cx)
        }
        Some("audio") => {
            let data = block.get("data").and_then(Value::as_str).unwrap_or("");
            let mime = block
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("audio/wav");
            render_blob(data, mime, "audio", prefix, draw.decoded, cx)
        }
        Some("resource_link") => resource_link(block, prefix, cx),
        Some("resource") => {
            let inner = block.get("resource");
            let field = |name: &str| inner.and_then(|r| r.get(name)).and_then(Value::as_str);
            let mime = field("mimeType").unwrap_or("");
            if let Some(text) = field("text") {
                return render_text(text, mime, prefix, draw, lone, cx);
            } else if let (Some(blob), Some(inner)) = (field("blob"), inner) {
                render_blob(blob, mime, &blob_stem(inner), prefix, draw.decoded, cx)
            } else {
                json_tree(block, draw.folds, prefix, cx)
            }
        }
        _ => json_tree(block, draw.folds, prefix, cx),
    };
    (element, false)
}

/// A link to a resource the server did not embed: where it is and what it
/// is, with the URI to copy.
pub(super) fn resource_link(block: &Value, prefix: &str, cx: &App) -> AnyElement {
    let t = *tokens(cx);
    let uri = text_field(block, "uri").unwrap_or_default().to_owned();
    let facts: Vec<&str> = [text_field(block, "name"), text_field(block, "mimeType")]
        .into_iter()
        .flatten()
        .collect();
    let copy = clip::copy_button(
        cx,
        SharedString::from(format!("{prefix}-uri-copy")),
        "Copy the URI",
        "URI",
        SharedString::from(uri.clone()),
    );
    let body = v_flex()
        .gap(px(2.))
        .child(div().text_color(t.fg).child(uri))
        .children(text_field(block, "description").map(|d| muted(cx, 12., d.to_owned())))
        .when(!facts.is_empty(), |el| {
            el.child(muted(cx, 11., facts.join(" · ")))
        })
        .into_any_element();
    labelled(cx, "Resource link", copy, body).into_any_element()
}

/// Text by mime: JSON becomes a tree (under any type but Markdown, when the
/// text parses as JSON), Markdown is rendered, everything else is a
/// read-only text area. The flag says it was the last: `lone` lets that
/// text area fill the height given.
pub(super) fn render_text(
    text: &str,
    mime: &str,
    prefix: &str,
    draw: &mut Draw<'_>,
    lone: bool,
    cx: &App,
) -> (AnyElement, bool) {
    let t = *tokens(cx);
    let is_json = mime.contains("json")
        || (!mime.contains("markdown") && text.trim_start().starts_with(['{', '[']));
    if is_json && let Some(value) = draw.decoded.json(prefix, text) {
        // Parsed once while drawn, so the tree shares it without a clone.
        return (json_tree_rc(value, draw.folds, prefix, cx), false);
    }
    let shared = draw.decoded.shared(prefix, text);
    // Text is the answer itself in most tool results, so it copies as text
    // rather than only as part of the whole response.
    let copy = clip::copy_button(
        cx,
        SharedString::from(format!("{prefix}-text-copy")),
        "Copy the text",
        "text",
        shared.clone(),
    );
    let label = if mime.is_empty() { "text" } else { mime };
    if mime.contains("markdown") {
        let body = div()
            .font_family(cx.theme().font_family.clone())
            .text_size(px(13.))
            .line_height(px(crate::views::BODY_LINE_HEIGHT))
            .max_w(px(640.))
            .child(
                TextView::markdown(SharedString::from(format!("{prefix}md")), shared)
                    .selectable(true),
            )
            .into_any_element();
        return (
            labelled(cx, label.to_owned(), copy, body).into_any_element(),
            false,
        );
    }
    let stamp = draw.decoded.stamp(prefix, text);
    let area = PlainText::new(format!("{prefix}area"), shared, stamp).fill(lone);
    let body = div()
        .id(SharedString::from(format!("{prefix}txt")))
        .text_color(t.fg)
        .when(lone, |block| block.flex_1().min_h_0().flex().flex_col())
        .child(area)
        .test_support()
        .into_any_element();
    (
        labelled(cx, label.to_owned(), copy, body)
            .when(lone, |block| block.flex_1().min_h_0())
            .into_any_element(),
        true,
    )
}
