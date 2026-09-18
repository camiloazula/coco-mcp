//! File formats: one JSON document per artefact, JSON Lines for the two
//! streams, Markdown for a diff that is going into a pull request.

use mcp_core::Snapshot;
use mcp_diff::{ItemKind, SnapshotDiff};
use mcp_store::CallRecord;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// One wire message, as a log export writes it.
#[derive(Debug, Clone, Copy)]
pub struct LogLine<'a> {
    /// When it was observed.
    pub at: OffsetDateTime,
    /// `out`, `in` or `note`.
    pub direction: &'a str,
    /// JSON-RPC method, or the label of a note.
    pub method: &'a str,
    /// Params, result or error body.
    pub payload: &'a Value,
}

#[derive(Serialize)]
struct LogRecord<'a> {
    at: String,
    direction: &'a str,
    method: &'a str,
    payload: &'a Value,
}

/// A snapshot as indented JSON. This is the artefact `mcp-diff` compares, so
/// an exported snapshot can serve as a baseline in a repository.
pub fn snapshot_json(snapshot: &Snapshot) -> String {
    line(serde_json::to_value(snapshot).unwrap_or(Value::Null))
}

/// The log as JSON Lines, oldest first. One message per line, so the file
/// greps and streams; the wall-clock time is RFC 3339 rather than the
/// drawer's `HH:MM:SS.mmm`, which is only good for reading on screen.
pub fn log_jsonl(lines: &[LogLine<'_>]) -> String {
    jsonl(lines.iter().map(|line| LogRecord {
        at: line.at.format(&Rfc3339).unwrap_or_default(),
        direction: line.direction,
        method: line.method,
        payload: line.payload,
    }))
}

/// Recorded calls as JSON Lines, oldest first.
pub fn history_jsonl(records: &[CallRecord]) -> String {
    jsonl(records.iter().rev())
}

/// A diff as a Markdown table, for a changelog or a pull request.
pub fn diff_markdown(server: &str, diff: &SnapshotDiff) -> String {
    let mut out = format!(
        "# {server}: changes since the last snapshot\n\n{}\n",
        diff.summary()
    );
    let changes = diff.changes();
    if changes.is_empty() {
        return out;
    }
    out.push_str("\n| Impact | Item | Path | Change |\n| --- | --- | --- | --- |\n");
    for change in changes {
        let kind = match change.kind {
            ItemKind::Server => "server",
            ItemKind::Tool => "tool",
            ItemKind::Resource => "resource",
            ItemKind::ResourceTemplate => "template",
            ItemKind::Prompt => "prompt",
        };
        let item = if change.kind == ItemKind::Server {
            "server".to_owned()
        } else {
            format!("{kind} `{}`", change.name)
        };
        let path = if change.path.is_empty() {
            String::new()
        } else {
            format!("`{}`", change.path)
        };
        out.push_str(&format!(
            "| {} | {item} | {path} | {} |\n",
            change.severity.label(),
            cell(&change.summary)
        ));
    }
    out
}

/// A suggested file name: `weather-log.jsonl`.
pub fn file_name(server: &str, what: &str, extension: &str) -> String {
    let stem = slug(server);
    if stem.is_empty() {
        format!("{what}.{extension}")
    } else {
        format!("{stem}-{what}.{extension}")
    }
}

/// A pipe would end the Markdown cell early, and a newline the row.
fn cell(text: &str) -> String {
    text.replace('|', r"\|").replace('\n', " ")
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

fn jsonl<T: Serialize>(items: impl Iterator<Item = T>) -> String {
    let mut out = String::new();
    for item in items {
        if let Ok(text) = serde_json::to_string(&item) {
            out.push_str(&text);
            out.push('\n');
        }
    }
    out
}

fn line(value: Value) -> String {
    let mut out = crate::pretty(&value);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_store::{CallKind, CallStatus};
    use serde_json::json;

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).unwrap()
    }

    #[test]
    fn log_lines_are_one_message_each() {
        let params = json!({"name": "get_weather"});
        let note = json!("spawned");
        let text = log_jsonl(&[
            LogLine {
                at: at(0),
                direction: "out",
                method: "tools/call",
                payload: &params,
            },
            LogLine {
                at: at(61),
                direction: "note",
                method: "stderr",
                payload: &note,
            },
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["at"], "1970-01-01T00:00:00Z");
        assert_eq!(first["direction"], "out");
        assert_eq!(first["payload"]["name"], "get_weather");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn history_is_written_oldest_first() {
        let call = |id: &str, secs: i64| CallRecord {
            id: id.to_owned(),
            server_id: "s".into(),
            kind: CallKind::Tool,
            name: "get_weather".into(),
            args: json!({}),
            result: None,
            status: CallStatus::Ok,
            error: None,
            elapsed_ms: 3,
            at: at(secs),
        };
        // The app keeps history newest first; a file reads better in order.
        let text = history_jsonl(&[call("new", 100), call("old", 1)]);
        let ids: Vec<String> = text
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].to_string())
            .collect();
        assert_eq!(ids, vec!["\"old\"", "\"new\""]);
    }

    #[test]
    fn a_summary_cannot_break_the_table() {
        let mut snapshot = Snapshot {
            protocol_version: "2025-06-18".into(),
            server_info: json!({"name": "weather"}),
            capabilities: json!({}),
            instructions: None,
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            list_failures: Vec::new(),
            taken_at: at(0),
        };
        let before = snapshot.clone();
        snapshot.instructions = Some("a | b".into());
        let diff = mcp_diff::diff(Some(&before), &snapshot);
        let text = diff_markdown("weather", &diff);
        assert!(text.starts_with("# weather: changes since the last snapshot"));
        for row in text.lines().filter(|l| l.starts_with("| ")) {
            assert_eq!(row.matches(" | ").count(), 3, "{row}");
        }
    }

    #[test]
    fn a_list_that_was_not_compared_is_named() {
        let before = Snapshot {
            protocol_version: "2025-06-18".into(),
            server_info: json!({"name": "weather"}),
            capabilities: json!({"resources": {}}),
            instructions: None,
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            list_failures: Vec::new(),
            taken_at: at(0),
        };
        let mut after = before.clone();
        after.instructions = Some("new".into());
        after.list_failures.push(mcp_core::ListFailure {
            method: mcp_core::list_method::RESOURCES.into(),
            error: "server error -32603: resources are unavailable".into(),
        });
        let text = diff_markdown("weather", &mcp_diff::diff(Some(&before), &after));
        assert!(
            text.contains("\n1 cosmetic · resources/list not compared\n"),
            "{text}"
        );
    }

    #[test]
    fn names_are_derived_from_the_server() {
        assert_eq!(file_name("weather", "log", "jsonl"), "weather-log.jsonl");
        assert_eq!(
            file_name("My Server (2)", "snapshot", "json"),
            "my-server-2-snapshot.json"
        );
        assert_eq!(file_name("", "history", "jsonl"), "history.jsonl");
    }
}
