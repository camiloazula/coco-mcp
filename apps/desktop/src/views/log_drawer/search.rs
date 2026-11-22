//! The drawer's text filter: its field in the header, and where a row drawn
//! under it matched, so the row shows why it is there.

use std::collections::HashMap;
use std::ops::Range;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{AppContext as _, Entity, HighlightStyle, StyledText, Subscription};

use super::*;
use crate::actions::FilterLog;
use crate::find::{excerpt, find, near_start};
use crate::state::Gone;

impl Workspace {
    /// The state of the filter's field, and what hands its text to the
    /// model as it is typed.
    pub(crate) fn log_search_field(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Subscription) {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder("Filter log"));
        let typed = cx.subscribe_in(
            &field,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.state.update(cx, |s, cx| s.set_log_query(text, cx));
                }
            },
        );
        (field, typed)
    }

    /// Put the keyboard in the filter's field, showing the drawer first when
    /// it is hidden.
    pub(crate) fn on_filter_log(
        &mut self,
        _: &FilterLog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_palette(window, cx);
        self.state.update(cx, |s, cx| {
            if !s.drawer_open {
                s.toggle_drawer(cx);
            }
        });
        // Once this turn is over: the palette, when the action came from
        // it, hands the keyboard to the list as it closes.
        let field = self.log_search.clone();
        window.defer(cx, move |window, cx| {
            field.update(cx, |input, cx| input.focus(window, cx));
        });
    }
}

/// The narrowest window whose header holds the field at its narrowest and
/// the deprecation note of the level beside it, with the longest count.
pub(super) const ROOMY: f32 = 960.;

/// What a row's body cell shows under the text filter, and the match in it.
pub(super) type Marked = (SharedString, Range<usize>);

/// The body cells of the rows drawn under the text filter, by row id. A
/// match past the start of the preview is looked for in the whole payload,
/// which is printed for it: once per row and filter, not on every frame.
#[derive(Debug, Default)]
pub(crate) struct Hits {
    server: Option<SharedString>,
    /// The filter the cells were made for, lowercase.
    needle: String,
    /// `None` for a row with the filter in its method or its agent alone.
    bodies: HashMap<u64, Option<Marked>>,
}

impl Hits {
    /// The body cell of `row`, of `server`'s log, under the filter `needle`.
    pub(super) fn body(
        &mut self,
        server: &SharedString,
        needle: &str,
        row: &LogRow,
    ) -> Option<Marked> {
        if needle.is_empty() {
            return None;
        }
        if self.server.as_ref() != Some(server) || self.needle != needle {
            *self = Self {
                server: Some(server.clone()),
                needle: needle.to_owned(),
                bodies: HashMap::new(),
            };
        }
        self.bodies
            .entry(row.row_id)
            .or_insert_with(|| body(row, needle))
            .clone()
    }

    /// Drop the cells of rows, or of a server, that are gone.
    pub(crate) fn forget(&mut self, gone: &Gone) {
        let ours = |id: &str| self.server.as_deref() == Some(id);
        match gone {
            Gone::LogRows { server_id, before } if ours(server_id) => {
                self.bodies.retain(|id, _| id >= before);
            }
            Gone::Server { id, .. } if ours(id) => *self = Self::default(),
            _ => {}
        }
    }
}

/// What the body cell of `row` shows under `needle`: the preview when the
/// match is near its start, else the payload around its first match.
fn body(row: &LogRow, needle: &str) -> Option<Marked> {
    // The lowercased copy says whether the payload has it at all, before
    // the payload is printed to find where. Without it the row is there
    // for its method or its agent, and the preview's own words (a level, a
    // cut payload's size) are not what was looked for.
    if !row.search.contains(needle) {
        return None;
    }
    let previewed = find(&row.body, needle);
    if let Some(hit) = previewed.as_ref().filter(|hit| near_start(&row.body, hit)) {
        return Some((row.body.clone().into(), hit.clone()));
    }
    let text = row.text();
    if let Some(hit) = find(&text, needle) {
        let (shown, hit) = excerpt(&text, hit);
        return Some((shown.into(), hit));
    }
    previewed.map(|hit| (row.body.clone().into(), hit))
}

/// `text` with `hit` in the accent, the way a selection is filled.
pub(super) fn marked(text: SharedString, hit: Option<Range<usize>>, t: &Tokens) -> AnyElement {
    let Some(hit) = hit else {
        return text.into_any_element();
    };
    let style = HighlightStyle {
        color: Some(t.on_accent),
        background_color: Some(t.accent),
        ..Default::default()
    };
    StyledText::new(text)
        .with_highlights([(hit, style)])
        .into_any_element()
}

/// The filter's field, between the count and the kind filters. It gives way
/// to them in a narrow window, and a click in it does not reach the header's
/// toggle.
pub(super) fn field(ws: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    div()
        .id("log-search")
        .w(px(200.))
        .min_w(px(112.))
        .flex_shrink(1.)
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            Input::new(&ws.log_search)
                .id("log-filter")
                .xsmall()
                .cleanable(true)
                .prefix(
                    Icon::new(IconName::Search)
                        .with_size(px(12.))
                        .text_color(t.muted),
                ),
        )
        .into_any_element()
}

/// What the empty drawer says: nothing logged, or nothing passing the kind
/// filter, the level and the text filter in force.
pub(super) fn empty_text(filter: LogFilter, min_level: Option<&str>, query: &str) -> String {
    if filter == LogFilter::All && min_level.is_none() && query.is_empty() {
        return "No messages yet".to_owned();
    }
    let mut text = match filter {
        LogFilter::All => "No messages".to_owned(),
        filter => format!("No {} messages", filter.label()),
    };
    if let Some(level) = min_level {
        text.push_str(&format!(" at {level} or above"));
    }
    if !query.is_empty() {
        text.push_str(&format!(" matching \"{query}\""));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LogRow;
    use serde_json::json;

    fn row(method: &str, payload: serde_json::Value) -> LogRow {
        let mut row = LogRow::new(
            time::OffsetDateTime::UNIX_EPOCH,
            Dir::In,
            method.into(),
            payload,
            false,
        );
        row.row_id = 7;
        row
    }

    #[test]
    fn a_match_near_the_start_of_the_preview_is_marked_there() {
        let row = row("tools/list", json!({ "tools": [{ "name": "Add" }] }));
        let (shown, hit) = body(&row, "add").unwrap();
        assert_eq!(shown.as_ref(), row.body);
        assert_eq!(&shown[hit], "Add");
    }

    #[test]
    fn a_match_deep_in_the_payload_is_shown_around_it() {
        let filler = "x".repeat(500);
        let row = row(
            "resources/list",
            json!({ "filler": filler, "uri": "mock://text/Hello" }),
        );
        assert!(!row.body.contains("Hello"), "past the preview");
        let (shown, hit) = body(&row, "hello").unwrap();
        assert_eq!(&shown[hit], "Hello");
        assert!(shown.starts_with('…') && shown.contains("mock://text/Hello"));

        // A string payload is read as its own text, not as JSON.
        let line = format!("{} said \"Hello\" twice", "y".repeat(300));
        let stderr = self::row("stderr", json!(line));
        let (shown, hit) = body(&stderr, "said \"hello\"").unwrap();
        assert_eq!(&shown[hit], "said \"Hello\"");
    }

    #[test]
    fn a_match_in_the_method_alone_leaves_the_body_as_it_is() {
        let row = row("tools/list", json!({ "cursor": null }));
        assert!(row.matches("tools/"));
        assert_eq!(body(&row, "tools/"), None);
    }

    #[test]
    fn the_cells_are_made_once_and_go_with_their_rows() {
        let server = SharedString::from("a");
        let row = row("ping", json!({ "note": "Hello" }));
        let mut hits = Hits::default();
        assert_eq!(hits.body(&server, "", &row), None, "no filter, no cell");
        assert!(hits.bodies.is_empty());
        assert!(hits.body(&server, "hello", &row).is_some());
        assert_eq!(hits.bodies.len(), 1);

        // Rows of another server's log share ids and nothing else.
        hits.forget(&Gone::LogRows {
            server_id: "b".into(),
            before: 100,
        });
        assert_eq!(hits.bodies.len(), 1);
        hits.forget(&Gone::LogRows {
            server_id: "a".into(),
            before: 8,
        });
        assert!(hits.bodies.is_empty());

        // Another filter, or another server, starts over.
        hits.body(&server, "hello", &row);
        hits.body(&server, "hell", &row);
        assert_eq!((hits.needle.as_str(), hits.bodies.len()), ("hell", 1));
        hits.forget(&Gone::Server {
            id: "a".into(),
            calls: vec![],
        });
        assert!(hits.server.is_none() && hits.bodies.is_empty());
    }

    #[test]
    fn the_empty_drawer_names_every_filter_in_force() {
        let all = LogFilter::All;
        assert_eq!(empty_text(all, None, ""), "No messages yet");
        assert_eq!(
            empty_text(LogFilter::Errors, None, ""),
            "No errors messages"
        );
        assert_eq!(
            empty_text(all, Some("warning"), ""),
            "No messages at warning or above"
        );
        assert_eq!(empty_text(all, None, "Foo"), "No messages matching \"Foo\"");
        assert_eq!(
            empty_text(LogFilter::Stderr, Some("error"), "x y"),
            "No stderr messages at error or above matching \"x y\""
        );
    }
}
