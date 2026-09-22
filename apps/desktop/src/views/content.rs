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
    let mut fills = false;
    let description = raw.get("description").and_then(Value::as_str);
    if let Some(desc) = description {
        parts.push(muted(cx, 12., desc.to_owned()).into_any_element());
    }
    let messages = raw.get("messages").and_then(Value::as_array);
    let rest = rest_fields(raw, &["description", "messages"]);
    // The one message of a prompt with nothing else in it.
    let lone = description.is_none()
        && messages.is_some_and(|messages| messages.len() == 1)
        && rest.is_none();
    let no_content = Value::Null;
    for (i, message) in messages.into_iter().flatten().enumerate() {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("?");
        let content = message.get("content").unwrap_or(&no_content);
        let (block, full) = content_block(content, &format!("{prefix}m{i}"), draw, lone, cx);
        fills |= full;
        parts.push(
            v_flex()
                .gap(px(4.))
                .when(full, |message| message.flex_1().min_h_0())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(t.muted)
                        .child(role.to_owned()),
                )
                .child(block)
                .into_any_element(),
        );
    }
    if parts.is_empty() {
        let value = draw.decoded.value(prefix, raw);
        let tree = json_tree_rc(value, draw.folds, prefix, Fit::Fill, cx);
        return blocks_column(vec![tree], true);
    }
    if let Some(rest) = rest {
        parts.push(other_fields(rest, prefix, draw, cx));
    }
    blocks_column(parts, fills)
}
