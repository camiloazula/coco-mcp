//! What a selected response keeps between frames. A response is drawn on
//! every frame for as long as it is selected, so a blob is decoded once and a
//! text block parsed and copied once, each kept by the id it is drawn under
//! until a frame no longer draws it.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{Image, ImageFormat, SharedString};
use serde_json::Value;

use crate::views::blob::decode;

/// Bytes read from each end of a blob's base64 text to tell it from another
/// of the same length at the same address, without hashing all of it.
const EDGE_BYTES: usize = 64;

/// What a blob decoded to: the image, when its type is one GPUI draws, and
/// the bytes a save writes; or why the base64 would not decode.
pub type Contents = Result<(Option<Arc<Image>>, Arc<[u8]>), String>;

/// Tells whether a text is still the one it was decoded from: the answer it
/// arrived in, where the text lives, its length, its two ends, and the format
/// it was drawn as. The answer settles it for a response run again, whose
/// new text may land where the old one lived with the same length and ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    answer: u64,
    at: usize,
    len: usize,
    edges: u64,
    format: Option<ImageFormat>,
}

impl Fingerprint {
    fn of(answer: u64, text: &str, format: Option<ImageFormat>) -> Self {
        let bytes = text.as_bytes();
        let mut hasher = DefaultHasher::new();
        bytes[..bytes.len().min(EDGE_BYTES)].hash(&mut hasher);
        bytes[bytes.len().saturating_sub(EDGE_BYTES)..].hash(&mut hasher);
        Self {
            answer,
            at: bytes.as_ptr() as usize,
            len: bytes.len(),
            edges: hasher.finish(),
            format,
        }
    }
}

struct Blob {
    fingerprint: Fingerprint,
    contents: Contents,
}

/// A text block, and what drawing it has needed so far.
struct Text {
    fingerprint: Fingerprint,
    /// The text shared with the elements and copy buttons that show it.
    shared: Option<SharedString>,
    /// The text parsed as JSON, `None` inside when it is not JSON.
    json: Option<Option<Rc<Value>>>,
    /// Whether the text is the JSON of the result's structuredContent.
    mirrors: Option<bool>,
}

/// The decoded blobs and parsed text blocks of the responses on screen, by
/// element id.
///
/// The workspace calls [`Decoded::begin_frame`] before drawing its columns
/// and [`Decoded::end_frame`] after, which drops everything the frame did not
/// draw, so a selection left behind does not keep its images.
#[derive(Default)]
pub struct Decoded {
    blobs: HashMap<String, Blob>,
    texts: HashMap<String, Text>,
    drawn: HashSet<String>,
    /// The answer the texts drawn next belong to (`Response::answer`).
    answer: u64,
}

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The blobs themselves are megabytes of bytes; their count is enough.
        f.debug_struct("Decoded")
            .field("blobs", &self.blobs.len())
            .field("texts", &self.texts.len())
            .finish_non_exhaustive()
    }
}

impl Decoded {
    /// Start marking what a frame draws.
    pub fn begin_frame(&mut self) {
        self.drawn.clear();
    }

    /// Drop what the frame did not draw.
    pub fn end_frame(&mut self) {
        let drawn = &self.drawn;
        self.blobs.retain(|id, _| drawn.contains(id));
        self.texts.retain(|id, _| drawn.contains(id));
    }

    /// Name the answer whose texts are drawn next: a text under an id is only
    /// kept for the answer it was decoded from.
    pub fn answer(&mut self, answer: u64) {
        self.answer = answer;
    }

    fn mark(&mut self, id: &str) {
        if !self.drawn.contains(id) {
            self.drawn.insert(id.to_owned());
        }
    }

    /// The contents of the blob drawn as `id` from `text`, decoded again only
    /// when `id` last showed a different text.
    pub fn blob(&mut self, id: &str, text: &str, format: Option<ImageFormat>) -> &Contents {
        self.mark(id);
        let fingerprint = Fingerprint::of(self.answer, text, format);
        let blob = match self.blobs.entry(id.to_owned()) {
            Entry::Occupied(mut entry) => {
                if entry.get().fingerprint != fingerprint {
                    entry.insert(Blob {
                        fingerprint,
                        contents: decode(text, format),
                    });
                }
                entry.into_mut()
            }
            Entry::Vacant(entry) => entry.insert(Blob {
                fingerprint,
                contents: decode(text, format),
            }),
        };
        &blob.contents
    }

    /// The entry of the text drawn as `id`, emptied when `id` last showed a
    /// different text.
    fn text(&mut self, id: &str, text: &str) -> &mut Text {
        self.mark(id);
        let fingerprint = Fingerprint::of(self.answer, text, None);
        let fresh = Text {
            fingerprint,
            shared: None,
            json: None,
            mirrors: None,
        };
        match self.texts.entry(id.to_owned()) {
            Entry::Occupied(mut entry) => {
                if entry.get().fingerprint != fingerprint {
                    entry.insert(fresh);
                }
                entry.into_mut()
            }
            Entry::Vacant(entry) => entry.insert(fresh),
        }
    }

    /// `text`, drawn as `id`, copied once into a string elements can share.
    pub fn shared(&mut self, id: &str, text: &str) -> SharedString {
        self.text(id, text)
            .shared
            .get_or_insert_with(|| SharedString::from(text.to_owned()))
            .clone()
    }

    /// A number that changes when `id` shows another text than it did, so
    /// an element keeping its own copy of the text (a text area) can tell
    /// without comparing them.
    pub fn stamp(&mut self, id: &str, text: &str) -> u64 {
        let fingerprint = self.text(id, text).fingerprint;
        let mut hasher = DefaultHasher::new();
        (
            fingerprint.answer,
            fingerprint.at,
            fingerprint.len,
            fingerprint.edges,
        )
            .hash(&mut hasher);
        hasher.finish()
    }

    /// `text`, drawn as `id`, parsed once as JSON; `None` when it is not.
    pub fn json(&mut self, id: &str, text: &str) -> Option<Rc<Value>> {
        let entry = self.text(id, text);
        if entry.json.is_none() {
            entry.json = Some(serde_json::from_str(text).ok().map(Rc::new));
        }
        entry.json.clone().flatten()
    }

    /// Whether `text`, drawn as `id`, is the JSON of `structured`, compared
    /// once. The text lives in the same result as `structured`, so while the
    /// text is unchanged so is the answer.
    pub fn mirrors(&mut self, id: &str, text: &str, structured: &Value) -> bool {
        if let Some(mirrors) = self.text(id, text).mirrors {
            return mirrors;
        }
        let mirrors = self
            .json(id, text)
            .is_some_and(|value| *value == *structured);
        self.text(id, text).mirrors = Some(mirrors);
        mirrors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn encoded(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn parts(contents: &Contents) -> (Option<Arc<Image>>, Arc<[u8]>) {
        contents.clone().unwrap()
    }

    #[test]
    fn a_blob_is_decoded_once_while_it_is_drawn() {
        let mut decoded = Decoded::default();
        let text = encoded(b"not really a png, but bytes all the same");
        decoded.begin_frame();
        let (image, bytes) = parts(decoded.blob("respx0", &text, Some(ImageFormat::Png)));
        decoded.end_frame();
        decoded.begin_frame();
        let (again, same) = parts(decoded.blob("respx0", &text, Some(ImageFormat::Png)));
        decoded.end_frame();
        assert!(
            Arc::ptr_eq(&bytes, &same),
            "the bytes were not decoded again"
        );
        assert!(Arc::ptr_eq(&image.unwrap(), &again.unwrap()));
        assert_eq!(&*bytes, b"not really a png, but bytes all the same");
    }

    #[test]
    fn another_text_under_the_same_id_is_decoded_afresh() {
        let mut decoded = Decoded::default();
        let first = encoded(b"first blob");
        let second = encoded(b"other blob");
        assert_eq!(first.len(), second.len());
        decoded.begin_frame();
        let (_, before) = parts(decoded.blob("respc0", &first, None));
        let (image, after) = parts(decoded.blob("respc0", &second, None));
        assert!(image.is_none(), "not an image type");
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(&*after, b"other blob");
    }

    #[test]
    fn the_same_text_in_a_newer_answer_is_parsed_afresh() {
        let mut decoded = Decoded::default();
        let text = r#"{"value": 1}"#.to_owned();
        decoded.begin_frame();
        decoded.answer(1);
        let first = decoded.json("respc0", &text).unwrap();
        assert!(Rc::ptr_eq(&first, &decoded.json("respc0", &text).unwrap()));
        decoded.answer(2);
        let second = decoded.json("respc0", &text).unwrap();
        assert!(!Rc::ptr_eq(&first, &second), "not the old answer's parse");
    }

    #[test]
    fn a_stamp_follows_the_text_and_the_answer() {
        let mut decoded = Decoded::default();
        decoded.begin_frame();
        decoded.answer(1);
        let text = "plain words".to_owned();
        let first = decoded.stamp("respc0", &text);
        assert_eq!(first, decoded.stamp("respc0", &text), "the same text");
        let other = "other words".to_owned();
        assert_ne!(first, decoded.stamp("respc0", &other));
        decoded.answer(2);
        assert_ne!(
            first,
            decoded.stamp("respc0", &text),
            "the same text in a newer answer"
        );
    }

    #[test]
    fn invalid_base64_is_reported() {
        let mut decoded = Decoded::default();
        decoded.begin_frame();
        let contents = decoded.blob("respx0", "not base64 at all!", Some(ImageFormat::Png));
        assert!(
            contents
                .as_ref()
                .is_err_and(|e| e.starts_with("invalid base64"))
        );
    }

    #[test]
    fn what_a_frame_did_not_draw_is_dropped() {
        let mut decoded = Decoded::default();
        let text = encoded(b"pixels");
        decoded.begin_frame();
        decoded.blob("a", &text, None);
        decoded.blob("b", &text, None);
        decoded.shared("c", "text");
        decoded.end_frame();
        assert_eq!((decoded.blobs.len(), decoded.texts.len()), (2, 1));
        decoded.begin_frame();
        decoded.blob("a", &text, None);
        decoded.end_frame();
        assert!(decoded.blobs.contains_key("a"));
        assert!(!decoded.blobs.contains_key("b"), "b was not drawn");
        assert!(decoded.texts.is_empty(), "c was not drawn");
    }

    #[test]
    fn a_text_block_is_parsed_and_copied_once_while_it_is_drawn() {
        let mut decoded = Decoded::default();
        let text = r#"{"rows": [1, 2, 3]}"#.to_owned();
        let structured = serde_json::json!({"rows": [1, 2, 3]});
        decoded.begin_frame();
        let parsed = decoded.json("respc0", &text).unwrap();
        let shared = decoded.shared("respc0", &text);
        assert!(decoded.mirrors("respc0", &text, &structured));
        decoded.end_frame();
        decoded.begin_frame();
        let again = decoded.json("respc0", &text).unwrap();
        assert!(Rc::ptr_eq(&parsed, &again), "not parsed again");
        assert_eq!(decoded.shared("respc0", &text), shared);
        // The comparison is kept with the text, not made again.
        assert!(decoded.mirrors("respc0", &text, &serde_json::json!(null)));
        decoded.end_frame();
        // Another text under the id starts over.
        let other = "plain words".to_owned();
        decoded.begin_frame();
        assert!(decoded.json("respc0", &other).is_none());
        assert!(!decoded.mirrors("respc0", &other, &structured));
        assert_eq!(decoded.shared("respc0", &other), "plain words");
    }
}
