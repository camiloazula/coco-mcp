//! What right-clicking a log row offers.
//!
//! A row is one frame of the session, so it copies as the payload the drawer
//! shows, as the whole JSON-RPC message, and, for something the app sent to
//! an HTTP server, as the `curl` that would send it again. A row whose payload
//! was cut keeps only its head, which would not send the same request, so it
//! offers no `curl`. The entries are
//! built from the model when the menu opens: printing every drawn row's
//! payload on every frame, in case someone right-clicks it, is more than a
//! long log can afford.

use mcp_core::ServerSpec;

use crate::clip::MenuEntry;
use crate::state::{Dir, LogRow};

/// The entries for `row`, logged by the server `spec` describes.
pub(crate) fn row_entries(row: &LogRow, spec: &ServerSpec) -> Vec<MenuEntry> {
    let mut entries = vec![MenuEntry::new(
        "Copy message",
        "message",
        mcp_exchange::pretty(&row.payload),
    )];
    if let Some(message) = row.message() {
        entries.push(MenuEntry::new(
            "Copy as JSON-RPC",
            "message",
            mcp_exchange::pretty(&message),
        ));
        if let (Dir::Out, None, ServerSpec::Http { url, headers, auth }) =
            (row.dir, row.truncated, spec)
        {
            entries.push(MenuEntry::new(
                "Copy as curl",
                "curl command",
                mcp_exchange::curl(mcp_exchange::Curl {
                    url,
                    headers,
                    auth,
                    body: &message,
                }),
            ));
        }
    }
    entries.push(MenuEntry::new(
        "Copy line",
        "line",
        format!(
            "{} {} {} {}",
            row.time,
            row.dir.arrow(),
            row.method,
            row.body
        ),
    ));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LOG_PAYLOAD_LIMIT, log_row, size_label};
    use mcp_core::{Direction, EventKind, EventSink};
    use serde_json::{Value, json};

    fn http() -> ServerSpec {
        ServerSpec::Http {
            url: "https://api.example.com/mcp".into(),
            headers: Default::default(),
            auth: mcp_core::AuthRef::Bearer {
                keyring_id: "remote-token".into(),
            },
        }
    }

    fn stdio() -> ServerSpec {
        ServerSpec::Stdio {
            command: "weather".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        }
    }

    fn request(params: Value) -> LogRow {
        let sink = EventSink::new(8);
        log_row(&sink.emit(EventKind::Request {
            direction: Direction::Outbound,
            id: json!(1),
            method: "tools/call".into(),
            params: Some(params),
        }))
        .unwrap()
    }

    fn labels(entries: &[MenuEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.label.as_ref()).collect()
    }

    fn text<'a>(entries: &'a [MenuEntry], label: &str) -> &'a str {
        &entries.iter().find(|e| e.label == label).unwrap().text
    }

    #[test]
    fn a_request_to_an_http_server_copies_four_ways() {
        let row = request(json!({"name": "get_weather", "arguments": {"city": "Paris"}}));
        let entries = row_entries(&row, &http());
        assert_eq!(
            labels(&entries),
            [
                "Copy message",
                "Copy as JSON-RPC",
                "Copy as curl",
                "Copy line"
            ]
        );
        let message: Value = serde_json::from_str(text(&entries, "Copy message")).unwrap();
        assert_eq!(message["arguments"]["city"], "Paris");
        let frame: Value = serde_json::from_str(text(&entries, "Copy as JSON-RPC")).unwrap();
        assert_eq!(frame["id"], json!(1));
        assert_eq!(frame["method"], "tools/call");
        let curl = text(&entries, "Copy as curl");
        assert!(curl.contains("$MCP_TOKEN"), "{curl}");
        assert!(curl.contains(r#""method":"tools/call""#), "{curl}");
        assert_eq!(
            text(&entries, "Copy line"),
            format!("{} → tools/call {}", row.time, row.body)
        );
    }

    #[test]
    fn only_a_frame_sent_to_an_http_server_copies_as_curl() {
        let entries = row_entries(&request(json!({})), &stdio());
        assert_eq!(
            labels(&entries),
            ["Copy message", "Copy as JSON-RPC", "Copy line"],
            "a stdio server has no endpoint"
        );
        let sink = EventSink::new(8);
        let stderr = log_row(&sink.emit(EventKind::Stderr {
            line: "booting".into(),
        }))
        .unwrap();
        let entries = row_entries(&stderr, &http());
        assert_eq!(
            labels(&entries),
            ["Copy message", "Copy line"],
            "stderr was never a frame"
        );
        assert_eq!(
            text(&entries, "Copy line"),
            format!("{} · stderr booting", stderr.time)
        );
    }

    #[test]
    fn a_truncated_row_names_its_size_in_every_entry() {
        let blob = "x".repeat(LOG_PAYLOAD_LIMIT * 3);
        let row = request(json!({"name": "upload", "arguments": {"blob": blob}}));
        let size = size_label(row.truncated.expect("over the limit"));
        let entries = row_entries(&row, &http());
        assert_eq!(
            labels(&entries),
            ["Copy message", "Copy as JSON-RPC", "Copy line"],
            "the head alone would send another request"
        );
        for entry in &entries {
            assert!(entry.text.contains(&size), "{} lacks {size}", entry.label);
        }
        for label in ["Copy message", "Copy as JSON-RPC"] {
            assert!(text(&entries, label).contains("originalBytes"), "{label}");
        }
    }
}
