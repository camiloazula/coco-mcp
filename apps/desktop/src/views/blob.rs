//! Base64 blobs in a response: images are drawn, anything else shows its
//! size, and both can be saved to a file. Each is decoded once and kept in
//! [`Decoded`] while it is drawn.

use std::sync::Arc;

use base64::Engine as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Image, ImageFormat, IntoElement, ParentElement, SharedString, Styled, div,
    img, px,
};
use serde_json::Value;

use crate::theme::tokens;
use crate::views::kept::{Contents, Decoded};
use crate::views::muted;
use crate::{clip, state};

/// Decode a blob's base64, and draw it as an image when it has a `format`.
pub(super) fn decode(text: &str, format: Option<ImageFormat>) -> Contents {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| format!("invalid base64: {e}"))?;
    let image = format.map(|format| Arc::new(Image::from_bytes(format, bytes.clone())));
    Ok((image, Arc::from(bytes)))
}

/// File stem for a saved blob: the last meaningful part of its URI, so
/// `weather://city/paris.png` saves as `paris.png`.
pub fn blob_stem(content: &Value) -> String {
    let uri = content.get("uri").and_then(Value::as_str).unwrap_or("");
    let tail = uri
        .rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or("contents");
    let stem: String = tail
        .chars()
        .take_while(|c| *c != '.')
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let stem = stem.trim_matches('-').to_owned();
    if stem.is_empty() {
        "contents".to_owned()
    } else {
        stem
    }
}

/// File extension for a mime type, for the name the save panel suggests.
fn extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/csv" => "csv",
        "application/json" => "json",
        _ => "bin",
    }
}

/// Base64 blob by mime: images are decoded and drawn; anything else shows its
/// size. Either way the bytes can be saved, which is the only route out of
/// the app for a binary resource.
pub fn render_blob(
    blob: &str,
    mime: &str,
    stem: &str,
    id: &str,
    decoded: &mut Decoded,
    cx: &App,
) -> AnyElement {
    let format = match mime {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/webp" => Some(ImageFormat::Webp),
        "image/gif" => Some(ImageFormat::Gif),
        "image/svg+xml" => Some(ImageFormat::Svg),
        "image/bmp" => Some(ImageFormat::Bmp),
        "image/tiff" => Some(ImageFormat::Tiff),
        _ => None,
    };
    let save = |bytes: &Arc<[u8]>, cx: &App| -> AnyElement {
        clip::save_button(
            cx,
            SharedString::from(format!("save-{id}")),
            "Save to a file",
            format!("{stem}.{}", extension(mime)),
            bytes.clone(),
        )
        .into_any_element()
    };
    match decoded.blob(id, blob, format) {
        Ok((Some(image), bytes)) => v_flex()
            .gap(px(6.))
            .child(img(image.clone()).max_w(px(640.)).max_h(px(480.)))
            .child(
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .child(muted(
                        cx,
                        11.,
                        format!("{mime} · {}", state::size_label(bytes.len())),
                    ))
                    .child(save(bytes, cx)),
            )
            .into_any_element(),
        Ok((None, bytes)) => h_flex()
            .gap(px(8.))
            .items_center()
            .child(muted(
                cx,
                12.,
                format!(
                    "{} · {}",
                    if mime.is_empty() { "binary" } else { mime },
                    state::size_label(bytes.len())
                ),
            ))
            .child(save(bytes, cx))
            .into_any_element(),
        Err(e) => div()
            .text_size(px(12.))
            .text_color(tokens(cx).err)
            .child(e.clone())
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_blob_is_named_after_its_uri() {
        let content = serde_json::json!({"uri": "weather://city/paris.png"});
        assert_eq!(blob_stem(&content), "paris");
        assert_eq!(extension("image/png"), "png");
        assert_eq!(blob_stem(&serde_json::json!({})), "contents");
    }
}
