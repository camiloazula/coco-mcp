//! The body of an answered request, drawn by what it holds: structured
//! content and JSON text as trees, Markdown rendered, other text verbatim,
//! and base64 blobs through `blob.rs`. A text block is parsed and copied
//! once while it is drawn, in [`Decoded`], not on every frame; plain text
//! is a read-only text area (`plain.rs`), which lays out only the lines in
//! view.

use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, div, px};
use serde_json::Value;

use crate::clip;
use crate::theme::tokens;
use crate::views::blob::{blob_stem, render_blob};
use crate::views::json::{Fit, Folds, json_tree, json_tree_rc};
use crate::views::kept::Decoded;
use crate::views::plain::PlainText;
use crate::views::{labelled, muted, tree_section};

mod blocks;

use blocks::*;

/// What drawing a body needs besides the result: the folds of its trees and
/// the cache its blobs are decoded into.
pub struct Draw<'a> {
    /// Fold state of every tree in the window.
    pub folds: &'a Folds<'a>,
    /// Decoded blobs and parsed text, by element id.
    pub decoded: &'a mut Decoded,
}

/// The bytes of `raw`, a `method` result, as compact JSON, less what the
/// body draws at a flat cost however long it is: the base64 of a blob,
/// decoded once, and the text of a text area or a Markdown document, laid
/// out only where it is in view. Only what trees lay out counts.
pub fn laid_out_size(method: &str, raw: &Value) -> usize {
    let list = |name: &str| {
        raw.get(name)
            .and_then(Value::as_array)
            .map(|items| items.as_slice())
            .unwrap_or_default()
    };
    let flat: usize = match method {
        "resources/read" => list("contents")
            .iter()
            .filter_map(|c| {
                let mime = text_field(c, "mimeType").unwrap_or("");
                match text_field(c, "text") {
                    Some(text) => (!is_json_text(text, mime)).then_some(text),
                    None => text_field(c, "blob"),
                }
            })
            .map(str::len)
            .sum(),
        "prompts/get" => list("messages")
            .iter()
            .filter_map(|m| m.get("content"))
            .filter_map(block_flat)
            .map(str::len)
            .sum(),
        _ => list("content")
            .iter()
            .filter_map(block_flat)
            .map(str::len)
            .sum(),
    };
    mcp_exchange::json_size(raw).saturating_sub(flat)
}

/// The base64 [`content_block`] draws as a blob, or the text it draws as a
/// text area or a document, if either.
fn block_flat(block: &Value) -> Option<&str> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            let text = text_field(block, "text")?;
            (!is_json_text(text, "")).then_some(text)
        }
        Some("image" | "audio") => text_field(block, "data"),
        Some("resource") => {
            let inner = block.get("resource")?;
            let mime = text_field(inner, "mimeType").unwrap_or("");
            match text_field(inner, "text") {
                Some(text) => (!is_json_text(text, mime)).then_some(text),
                None => text_field(inner, "blob"),
            }
        }
        _ => None,
    }
}

fn text_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

/// A drawn body, and whether it takes the whole height it is given: a
/// response that is one plain text block fills the response panel with its
/// text area instead of scrolling the panel around it.
pub struct Body {
    pub element: AnyElement,
    pub fills: bool,
}

/// The body of a `method` result, every element id under `prefix`.
pub fn body(method: &str, raw: &Value, prefix: &str, draw: &mut Draw<'_>, cx: &App) -> Body {
    match method {
        "resources/read" => resource_body(raw, prefix, draw, cx),
        "prompts/get" => prompt_body(raw, prefix, draw, cx),
        _ => tool_body(raw, prefix, draw, cx),
    }
}

fn tool_body(raw: &Value, prefix: &str, draw: &mut Draw<'_>, cx: &App) -> Body {
    let mut fills = false;
    let structured = raw.get("structuredContent");
    let blocks = raw.get("content").and_then(Value::as_array);
    let rest = rest_fields(raw, &["content", "structuredContent"]);
    // The one content block of a result with nothing else in it.
    let lone =
        structured.is_none() && blocks.is_some_and(|blocks| blocks.len() == 1) && rest.is_none();
    let mut shown: Vec<AnyElement> = Vec::new();
    if let Some(blocks) = blocks {
        for (i, block) in blocks.iter().enumerate() {
            let id = format!("{prefix}c{i}");
            // Servers commonly mirror structuredContent as a JSON text block; show it once.
            let duplicate = structured.is_some_and(|s| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| draw.decoded.mirrors(&id, text, s))
            });
            if duplicate {
                continue;
            }
            let (block, full) = content_block(block, &id, draw, lone, cx);
            fills |= full;
            shown.push(block);
        }
    }
    let mut parts: Vec<AnyElement> = Vec::new();
    if let Some(value) = structured {
        // The tree of a result that is nothing but its structuredContent
        // has the whole panel.
        let fit = if shown.is_empty() && rest.is_none() {
            fills = true;
            Fit::Fill
        } else {
            Fit::Rows
        };
        let value = draw.decoded.value(&format!("{prefix}s"), value);
        let tree = json_tree_rc(value.clone(), draw.folds, &format!("{prefix}s"), fit, cx);
        parts.push(
            tree_section(
                cx,
                "structuredContent",
                SharedString::from(format!("{prefix}s-copy")),
                value,
                tree,
            )
            .when(fit == Fit::Fill, |section| section.flex_1().min_h_0())
            .into_any_element(),
        );
    }
    parts.extend(shown);
    if parts.is_empty() {
        let value = draw.decoded.value(prefix, raw);
        parts.push(json_tree_rc(value, draw.folds, prefix, Fit::Fill, cx));
        fills = true;
    } else if let Some(rest) = rest {
        parts.push(other_fields(rest, prefix, draw, cx));
    }
    blocks_column(parts, fills)
}

/// `parts` in a column; one that fills gives its height to its one block.
fn blocks_column(parts: Vec<AnyElement>, fills: bool) -> Body {
    let element = v_flex()
        .gap(px(12.))
        .when(fills, |column| column.size_full().min_h_0())
        .children(parts)
        .into_any_element();
    Body { element, fills }
}

/// Whatever else a result carries (`isError`, `_meta`, vendor fields),
/// which the reading of it above leaves out. The two defaults every result
/// repeats, `isError: false` and `resultType: complete`, say nothing.
fn rest_fields(raw: &Value, shown: &[&str]) -> Option<serde_json::Map<String, Value>> {
    let rest: serde_json::Map<String, Value> = raw
        .as_object()?
        .iter()
        .filter(|(key, value)| {
            !shown.contains(&key.as_str())
                && !(key.as_str() == "isError" && **value == Value::Bool(false))
                && !(key.as_str() == "resultType" && value.as_str() == Some("complete"))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    (!rest.is_empty()).then_some(rest)
}

/// The tree of the [`rest_fields`] `rest`.
fn other_fields(
    rest: serde_json::Map<String, Value>,
    prefix: &str,
    draw: &mut Draw<'_>,
    cx: &App,
) -> AnyElement {
    let id = format!("{prefix}o");
    let value = draw.decoded.owned(&id, Value::Object(rest));
    let tree = json_tree_rc(value.clone(), draw.folds, &id, Fit::Rows, cx);
    tree_section(
        cx,
        "Other fields",
        SharedString::from(format!("{id}-copy")),
        value,
        tree,
    )
    .into_any_element()
}

fn resource_body(raw: &Value, prefix: &str, draw: &mut Draw<'_>, cx: &App) -> Body {
    let contents = raw
        .get("contents")
        .and_then(Value::as_array)
        .filter(|contents| !contents.is_empty());
    let Some(contents) = contents else {
        let value = draw.decoded.value(prefix, raw);
        let tree = json_tree_rc(value, draw.folds, prefix, Fit::Fill, cx);
        return blocks_column(vec![tree], true);
    };
    let mut parts: Vec<AnyElement> = Vec::new();
    let mut fills = false;
    let rest = rest_fields(raw, &["contents"]);
    let lone = contents.len() == 1 && rest.is_none();
    for (i, c) in contents.iter().enumerate() {
        let mime = c.get("mimeType").and_then(Value::as_str).unwrap_or("");
        let p = format!("{prefix}x{i}");
        parts.push(if let Some(text) = c.get("text").and_then(Value::as_str) {
            let (text, full) = render_text(text, mime, &p, draw, lone, cx);
            fills |= full;
            text
        } else if let Some(blob) = c.get("blob").and_then(Value::as_str) {
            render_blob(blob, mime, &blob_stem(c), &p, draw.decoded, cx)
        } else {
            json_tree(c, draw.folds, &p, Fit::Rows, cx)
        });
    }
    if let Some(rest) = rest {
        parts.push(other_fields(rest, prefix, draw, cx));
    }
    blocks_column(parts, fills)
}

fn prompt_body(raw: &Value, prefix: &str, draw: &mut Draw<'_>, cx: &App) -> Body {
    let t = *tokens(cx);
    let mut parts: Vec<AnyElement> = Vec::new();
    if let Some(desc) = raw.get("description").and_then(Value::as_str) {
        parts.push(muted(cx, 12., desc.to_owned()).into_any_element());
    }
    let no_content = Value::Null;
    for (i, message) in raw
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("?");
        let content = message.get("content").unwrap_or(&no_content);
        parts.push(
            v_flex()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(t.muted)
                        .child(role.to_owned()),
                )
                .child(content_block(content, &format!("{prefix}m{i}"), draw, false, cx).0)
                .into_any_element(),
        );
    }
    if parts.is_empty() {
        let value = draw.decoded.value(prefix, raw);
        let tree = json_tree_rc(value, draw.folds, prefix, Fit::Fill, cx);
        return blocks_column(vec![tree], true);
    }
    if let Some(rest) = rest_fields(raw, &["description", "messages"]) {
        parts.push(other_fields(rest, prefix, draw, cx));
    }
    blocks_column(parts, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64_drawn_as_a_blob_is_not_counted_as_laid_out() {
        let data = "iVBORw0KGgo".repeat(40_000);
        let text = json!({"content": [{"type": "text", "text": "a picture"}]});
        let image = json!({"content": [
            {"type": "text", "text": "a picture"},
            {"type": "image", "data": data, "mimeType": "image/png"},
        ]});
        let size = laid_out_size("tools/call", &image);
        let caption = "a picture".len();
        assert_eq!(size, mcp_exchange::json_size(&image) - data.len() - caption);
        assert!(size < mcp_exchange::json_size(&text) + 64, "{size}");
        // A resource read and an embedded resource likewise; a blob beside
        // text is not drawn as one, so it counts, and the text does not.
        let read = json!({"contents": [{"uri": "a", "blob": data}]});
        assert_eq!(
            laid_out_size("resources/read", &read),
            mcp_exchange::json_size(&read) - data.len()
        );
        let both = json!({"contents": [{"uri": "a", "text": "t", "blob": data}]});
        assert_eq!(
            laid_out_size("resources/read", &both),
            mcp_exchange::json_size(&both) - 1
        );
        let embedded = json!({"messages": [{"role": "user", "content":
            {"type": "resource", "resource": {"uri": "a", "blob": data}}}]});
        assert_eq!(
            laid_out_size("prompts/get", &embedded),
            mcp_exchange::json_size(&embedded) - data.len()
        );
        // Audio is a blob with a save button; everything under another key is
        // drawn as a tree.
        let audio = json!({"content": [{"type": "audio", "data": data}]});
        assert_eq!(
            laid_out_size("tools/call", &audio),
            mcp_exchange::json_size(&audio) - data.len()
        );
        let rows = json!({"structuredContent": {"data": data}});
        assert_eq!(
            laid_out_size("tools/call", &rows),
            mcp_exchange::json_size(&rows)
        );
    }

    #[test]
    fn text_drawn_as_text_or_a_document_is_not_counted_either() {
        let prose = "line\n".repeat(60_000);
        let plain = json!({"content": [{"type": "text", "text": prose}]});
        assert_eq!(
            laid_out_size("tools/call", &plain),
            mcp_exchange::json_size(&plain) - prose.len(),
            "a text area lays out only what is in view"
        );
        let md = json!({"content": [{"type": "resource", "resource":
            {"uri": "a", "mimeType": "text/markdown", "text": prose}}]});
        assert_eq!(
            laid_out_size("tools/call", &md),
            mcp_exchange::json_size(&md) - prose.len()
        );
        let read = json!({"contents": [{"uri": "a", "mimeType": "text/plain", "text": prose}]});
        assert_eq!(
            laid_out_size("resources/read", &read),
            mcp_exchange::json_size(&read) - prose.len()
        );
        // JSON in a text block becomes a tree, which lays out its lines.
        let rows = format!("[{}]", "{\"id\":1},".repeat(20_000).trim_end_matches(','));
        let tree = json!({"content": [{"type": "text", "text": rows}]});
        assert_eq!(
            laid_out_size("tools/call", &tree),
            mcp_exchange::json_size(&tree)
        );
        let said =
            json!({"contents": [{"uri": "a", "mimeType": "application/json", "text": "plain"}]});
        assert_eq!(
            laid_out_size("resources/read", &said),
            mcp_exchange::json_size(&said),
            "a JSON type is parsed whatever the text"
        );
    }
}
