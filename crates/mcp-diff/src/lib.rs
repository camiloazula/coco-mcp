//! Snapshot diffing with `Breaking` / `Compatible` / `Cosmetic` classification.
//!
//! Direction matters for schemas: an *input* schema describes what the
//! client must send, so tightening it (a new required field, a removed enum
//! value, a raised minimum) breaks existing callers, while loosening it is
//! compatible. An *output* schema describes what the client receives, so the
//! rules flip: loosening (a field that may now be absent, a new enum value)
//! breaks consumers, tightening is compatible. `$ref`s into `$defs` and
//! `definitions` are resolved before comparing, so a change inside a shared
//! definition is reported at every use.
//!
//! Hand-written: JSON Schema diff crates on crates.io either lack the
//! breaking/compatible semantics or are unmaintained. Validation stays with
//! `jsonschema`; this module only classifies differences.
//!
//! UI-free: must never depend on `gpui` or `gpui-kit`.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod schema;

use mcp_core::{Snapshot, list_method};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use schema::Direction;

/// How a change affects existing clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Only text or metadata changed.
    Cosmetic,
    /// Existing clients keep working.
    Compatible,
    /// Existing clients may break.
    Breaking,
}

impl Severity {
    /// Lowercase label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Breaking => "breaking",
            Self::Compatible => "compatible",
            Self::Cosmetic => "cosmetic",
        }
    }
}

/// What kind of item a change belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// `initialize` result (protocol version, capabilities, server info).
    Server,
    /// A tool.
    Tool,
    /// A concrete resource.
    Resource,
    /// A resource template.
    ResourceTemplate,
    /// A prompt.
    Prompt,
}

/// One difference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// Impact.
    pub severity: Severity,
    /// Item kind.
    pub kind: ItemKind,
    /// Tool/prompt name, resource URI, or `server`.
    pub name: String,
    /// Where inside the item (`inputSchema.properties.city`), empty for the item itself.
    pub path: String,
    /// Human-readable description.
    pub summary: String,
    /// Old value, when meaningful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    /// New value, when meaningful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}

/// Result of comparing two snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// No earlier snapshot to compare with.
    First,
    /// Digests match.
    Unchanged,
    /// At least one difference.
    Changed {
        /// All differences, most severe first.
        changes: Vec<Change>,
    },
}

/// A classified diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDiff {
    /// What happened.
    #[serde(flatten)]
    pub outcome: Outcome,
    /// Number of breaking changes.
    pub breaking: usize,
    /// Number of compatible changes.
    pub compatible: usize,
    /// Number of cosmetic changes.
    pub cosmetic: usize,
    /// Lists left out of the comparison because they failed on either side,
    /// by method (`resources/list`). Their items are unknown, not unchanged.
    /// Absent from the JSON when every list was compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

impl SnapshotDiff {
    /// Whether any change is breaking.
    pub fn has_breaking(&self) -> bool {
        self.breaking > 0
    }

    /// The changes, if any.
    pub fn changes(&self) -> &[Change] {
        match &self.outcome {
            Outcome::Changed { changes } => changes,
            _ => &[],
        }
    }

    /// One-line summary such as `2 breaking · 1 compatible · 3 cosmetic`,
    /// ending in [`Self::not_compared`] when a list was skipped, so no
    /// summary reads as a clean pass while a list went unread.
    pub fn summary(&self) -> String {
        let head = match &self.outcome {
            Outcome::First => "first snapshot".into(),
            Outcome::Unchanged => "unchanged".into(),
            Outcome::Changed { .. } => {
                let mut parts = Vec::new();
                for (n, label) in [
                    (self.breaking, "breaking"),
                    (self.compatible, "compatible"),
                    (self.cosmetic, "cosmetic"),
                ] {
                    if n > 0 {
                        parts.push(format!("{n} {label}"));
                    }
                }
                parts.join(" · ")
            }
        };
        match self.not_compared() {
            Some(note) => format!("{head} · {note}"),
            None => head,
        }
    }

    /// `resources/list not compared` when a list was skipped.
    pub fn not_compared(&self) -> Option<String> {
        (!self.skipped.is_empty()).then(|| format!("{} not compared", self.skipped.join(", ")))
    }

    fn from_changes(mut changes: Vec<Change>, skipped: Vec<String>) -> Self {
        changes.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then_with(|| a.name.cmp(&b.name))
        });
        let count = |s: Severity| changes.iter().filter(|c| c.severity == s).count();
        Self {
            breaking: count(Severity::Breaking),
            compatible: count(Severity::Compatible),
            cosmetic: count(Severity::Cosmetic),
            outcome: if changes.is_empty() {
                Outcome::Unchanged
            } else {
                Outcome::Changed { changes }
            },
            skipped,
        }
    }
}

/// Compare `before` (the earlier snapshot, `None` on first contact) with `after`.
pub fn diff(before: Option<&Snapshot>, after: &Snapshot) -> SnapshotDiff {
    let Some(before) = before else {
        return SnapshotDiff {
            outcome: Outcome::First,
            breaking: 0,
            compatible: 0,
            cosmetic: 0,
            skipped: Vec::new(),
        };
    };
    // A list that could not be read on either side is not compared: its
    // items are unknown, not removed. Worked out before the digests are
    // compared, because the digest leaves the failures out.
    let skipped: Vec<String> = list_method::ALL
        .into_iter()
        .filter(|method| before.list_failed(method) || after.list_failed(method))
        .map(str::to_owned)
        .collect();
    if before.digest() == after.digest() {
        return SnapshotDiff::from_changes(Vec::new(), skipped);
    }
    let mut changes = Vec::new();
    diff_server(before, after, &mut changes);
    let compared = |method: &str| !skipped.iter().any(|s| s == method);
    if compared(list_method::TOOLS) {
        diff_tools(before, after, &mut changes);
    }
    if compared(list_method::RESOURCES) {
        diff_resources(before, after, &mut changes);
    }
    if compared(list_method::RESOURCE_TEMPLATES) {
        diff_templates(before, after, &mut changes);
    }
    if compared(list_method::PROMPTS) {
        diff_prompts(before, after, &mut changes);
    }
    SnapshotDiff::from_changes(changes, skipped)
}

fn change(
    severity: Severity,
    kind: ItemKind,
    name: &str,
    path: &str,
    summary: impl Into<String>,
    before: Option<Value>,
    after: Option<Value>,
) -> Change {
    Change {
        severity,
        kind,
        name: name.to_owned(),
        path: path.to_owned(),
        summary: summary.into(),
        before,
        after,
    }
}

fn diff_server(a: &Snapshot, b: &Snapshot, out: &mut Vec<Change>) {
    let kind = ItemKind::Server;
    if a.protocol_version != b.protocol_version {
        out.push(change(
            Severity::Compatible,
            kind,
            "server",
            "protocolVersion",
            format!(
                "protocol version {} → {}",
                a.protocol_version, b.protocol_version
            ),
            Some(Value::String(a.protocol_version.clone())),
            Some(Value::String(b.protocol_version.clone())),
        ));
    }
    if a.server_info != b.server_info {
        out.push(change(
            Severity::Cosmetic,
            kind,
            "server",
            "serverInfo",
            "server info changed",
            Some(a.server_info.clone()),
            Some(b.server_info.clone()),
        ));
    }
    if a.instructions != b.instructions {
        out.push(change(
            Severity::Cosmetic,
            kind,
            "server",
            "instructions",
            "instructions changed",
            a.instructions.clone().map(Value::String),
            b.instructions.clone().map(Value::String),
        ));
    }
    let caps = |s: &Snapshot| -> Vec<String> {
        s.capabilities
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    let (ca, cb) = (caps(a), caps(b));
    for removed in ca.iter().filter(|c| !cb.contains(c)) {
        out.push(change(
            Severity::Breaking,
            kind,
            "server",
            &format!("capabilities.{removed}"),
            format!("capability `{removed}` removed"),
            a.capabilities.get(removed).cloned(),
            None,
        ));
    }
    for added in cb.iter().filter(|c| !ca.contains(c)) {
        out.push(change(
            Severity::Compatible,
            kind,
            "server",
            &format!("capabilities.{added}"),
            format!("capability `{added}` added"),
            None,
            b.capabilities.get(added).cloned(),
        ));
    }
}

fn diff_tools(a: &Snapshot, b: &Snapshot, out: &mut Vec<Change>) {
    let kind = ItemKind::Tool;
    for old in &a.tools {
        match b.tool(&old.name) {
            None => out.push(change(
                Severity::Breaking,
                kind,
                &old.name,
                "",
                "tool removed",
                None,
                None,
            )),
            Some(new) => {
                if old.description != new.description || old.title != new.title {
                    out.push(change(
                        Severity::Cosmetic,
                        kind,
                        &old.name,
                        "description",
                        "description changed",
                        old.description.clone().map(Value::String),
                        new.description.clone().map(Value::String),
                    ));
                }
                if old.annotations != new.annotations {
                    out.push(change(
                        Severity::Cosmetic,
                        kind,
                        &old.name,
                        "annotations",
                        "annotations changed",
                        old.annotations.clone(),
                        new.annotations.clone(),
                    ));
                }
                schema::compare(
                    &old.input_schema,
                    &new.input_schema,
                    Direction::Input,
                    kind,
                    &old.name,
                    "inputSchema",
                    out,
                );
                match (&old.output_schema, &new.output_schema) {
                    (Some(o), Some(n)) => schema::compare(
                        o,
                        n,
                        Direction::Output,
                        kind,
                        &old.name,
                        "outputSchema",
                        out,
                    ),
                    (None, Some(n)) => out.push(change(
                        Severity::Compatible,
                        kind,
                        &old.name,
                        "outputSchema",
                        "output schema declared",
                        None,
                        Some(n.clone()),
                    )),
                    (Some(o), None) => out.push(change(
                        Severity::Breaking,
                        kind,
                        &old.name,
                        "outputSchema",
                        "output schema removed",
                        Some(o.clone()),
                        None,
                    )),
                    (None, None) => {}
                }
            }
        }
    }
    for new in b.tools.iter().filter(|t| a.tool(&t.name).is_none()) {
        out.push(change(
            Severity::Compatible,
            kind,
            &new.name,
            "",
            "tool added",
            None,
            None,
        ));
    }
}

fn diff_resources(a: &Snapshot, b: &Snapshot, out: &mut Vec<Change>) {
    let kind = ItemKind::Resource;
    for old in &a.resources {
        match b.resource(&old.uri) {
            None => out.push(change(
                Severity::Breaking,
                kind,
                &old.uri,
                "",
                "resource removed",
                None,
                None,
            )),
            Some(new) => {
                if old.mime_type != new.mime_type {
                    out.push(change(
                        Severity::Breaking,
                        kind,
                        &old.uri,
                        "mimeType",
                        "mime type changed",
                        old.mime_type.clone().map(Value::String),
                        new.mime_type.clone().map(Value::String),
                    ));
                }
                if old.name != new.name
                    || old.title != new.title
                    || old.description != new.description
                {
                    out.push(change(
                        Severity::Cosmetic,
                        kind,
                        &old.uri,
                        "description",
                        "name or description changed",
                        None,
                        None,
                    ));
                }
            }
        }
    }
    for new in b.resources.iter().filter(|r| a.resource(&r.uri).is_none()) {
        out.push(change(
            Severity::Compatible,
            kind,
            &new.uri,
            "",
            "resource added",
            None,
            None,
        ));
    }
}

fn diff_templates(a: &Snapshot, b: &Snapshot, out: &mut Vec<Change>) {
    let kind = ItemKind::ResourceTemplate;
    let find = |s: &Snapshot, t: &str| {
        s.resource_templates
            .iter()
            .find(|x| x.uri_template == t)
            .cloned()
    };
    for old in &a.resource_templates {
        match find(b, &old.uri_template) {
            None => out.push(change(
                Severity::Breaking,
                kind,
                &old.uri_template,
                "",
                "resource template removed",
                None,
                None,
            )),
            Some(new) => {
                if old.mime_type != new.mime_type {
                    out.push(change(
                        Severity::Breaking,
                        kind,
                        &old.uri_template,
                        "mimeType",
                        "mime type changed",
                        old.mime_type.clone().map(Value::String),
                        new.mime_type.clone().map(Value::String),
                    ));
                }
                if old.name != new.name
                    || old.title != new.title
                    || old.description != new.description
                {
                    out.push(change(
                        Severity::Cosmetic,
                        kind,
                        &old.uri_template,
                        "description",
                        "name or description changed",
                        None,
                        None,
                    ));
                }
            }
        }
    }
    for new in b
        .resource_templates
        .iter()
        .filter(|t| find(a, &t.uri_template).is_none())
    {
        out.push(change(
            Severity::Compatible,
            kind,
            &new.uri_template,
            "",
            "resource template added",
            None,
            None,
        ));
    }
}

fn diff_prompts(a: &Snapshot, b: &Snapshot, out: &mut Vec<Change>) {
    let kind = ItemKind::Prompt;
    for old in &a.prompts {
        match b.prompt(&old.name) {
            None => out.push(change(
                Severity::Breaking,
                kind,
                &old.name,
                "",
                "prompt removed",
                None,
                None,
            )),
            Some(new) => {
                if old.description != new.description || old.title != new.title {
                    out.push(change(
                        Severity::Cosmetic,
                        kind,
                        &old.name,
                        "description",
                        "description changed",
                        None,
                        None,
                    ));
                }
                for arg in &old.arguments {
                    match new.arguments.iter().find(|n| n.name == arg.name) {
                        None => out.push(change(
                            Severity::Compatible,
                            kind,
                            &old.name,
                            &format!("arguments.{}", arg.name),
                            "argument removed (extra arguments are ignored)",
                            None,
                            None,
                        )),
                        Some(n) => {
                            let was = arg.required == Some(true);
                            let is = n.required == Some(true);
                            if !was && is {
                                out.push(change(
                                    Severity::Breaking,
                                    kind,
                                    &old.name,
                                    &format!("arguments.{}", arg.name),
                                    "argument is now required",
                                    None,
                                    None,
                                ));
                            } else if was && !is {
                                out.push(change(
                                    Severity::Compatible,
                                    kind,
                                    &old.name,
                                    &format!("arguments.{}", arg.name),
                                    "argument is now optional",
                                    None,
                                    None,
                                ));
                            }
                            if arg.description != n.description {
                                out.push(change(
                                    Severity::Cosmetic,
                                    kind,
                                    &old.name,
                                    &format!("arguments.{}", arg.name),
                                    "argument description changed",
                                    None,
                                    None,
                                ));
                            }
                        }
                    }
                }
                for added in new
                    .arguments
                    .iter()
                    .filter(|n| !old.arguments.iter().any(|o| o.name == n.name))
                {
                    let severity = if added.required == Some(true) {
                        Severity::Breaking
                    } else {
                        Severity::Compatible
                    };
                    out.push(change(
                        severity,
                        kind,
                        &old.name,
                        &format!("arguments.{}", added.name),
                        if severity == Severity::Breaking {
                            "required argument added"
                        } else {
                            "optional argument added"
                        },
                        None,
                        None,
                    ));
                }
            }
        }
    }
    for new in b.prompts.iter().filter(|p| a.prompt(&p.name).is_none()) {
        out.push(change(
            Severity::Compatible,
            kind,
            &new.name,
            "",
            "prompt added",
            None,
            None,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(tools: Value, prompts: Value, caps: Value) -> Snapshot {
        serde_json::from_value(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "s", "version": "1"},
            "capabilities": caps,
            "tools": tools,
            "prompts": prompts,
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap()
    }

    fn resources(resources: Value, templates: Value) -> Snapshot {
        serde_json::from_value(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "s", "version": "1"},
            "capabilities": {"resources": {}},
            "resources": resources,
            "resourceTemplates": templates,
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn resources_and_templates() {
        let a = resources(
            json!([
                {"uri": "mock://a", "name": "a", "mimeType": "text/plain"},
                {"uri": "mock://gone", "name": "gone"},
                {"uri": "mock://t", "name": "t", "description": "old"}
            ]),
            json!([
                {"uriTemplate": "mock://item/{id}", "name": "item", "mimeType": "text/plain"},
                {"uriTemplate": "mock://old/{id}", "name": "old"},
                {"uriTemplate": "mock://doc/{id}", "name": "doc", "mimeType": "text/plain"}
            ]),
        );
        let b = resources(
            json!([
                {"uri": "mock://a", "name": "a", "mimeType": "application/json"},
                {"uri": "mock://t", "name": "t", "description": "new"},
                {"uri": "mock://new", "name": "new"}
            ]),
            json!([
                {"uriTemplate": "mock://item/{id}", "name": "item", "title": "Item", "mimeType": "text/plain"},
                {"uriTemplate": "mock://fresh/{id}", "name": "fresh"},
                {"uriTemplate": "mock://doc/{id}", "name": "doc", "mimeType": "text/markdown"}
            ]),
        );
        let d = diff(Some(&a), &b);
        let severity = |name: &str, summary: &str| {
            d.changes()
                .iter()
                .find(|c| c.name == name && c.summary == summary)
                .map(|c| c.severity)
        };
        let breaking = Some(Severity::Breaking);
        assert_eq!(severity("mock://gone", "resource removed"), breaking);
        assert_eq!(severity("mock://a", "mime type changed"), breaking);
        assert_eq!(
            severity("mock://old/{id}", "resource template removed"),
            breaking
        );
        assert_eq!(severity("mock://doc/{id}", "mime type changed"), breaking);
        let compatible = Some(Severity::Compatible);
        assert_eq!(severity("mock://new", "resource added"), compatible);
        assert_eq!(
            severity("mock://fresh/{id}", "resource template added"),
            compatible
        );
        let cosmetic = Some(Severity::Cosmetic);
        assert_eq!(
            severity("mock://t", "name or description changed"),
            cosmetic
        );
        assert_eq!(
            severity("mock://item/{id}", "name or description changed"),
            cosmetic
        );
        assert_eq!((d.breaking, d.compatible, d.cosmetic), (4, 2, 2));
    }

    #[test]
    fn first_and_unchanged() {
        let s = snap(json!([]), json!([]), json!({}));
        assert_eq!(diff(None, &s).outcome, Outcome::First);
        assert_eq!(diff(Some(&s), &s).outcome, Outcome::Unchanged);
        assert_eq!(diff(Some(&s), &s).summary(), "unchanged");
    }

    #[test]
    fn tools_added_removed_and_described() {
        let a = snap(
            json!([{"name": "echo", "description": "old", "inputSchema": {"type": "object"}}, {"name": "gone", "inputSchema": {"type": "object"}}]),
            json!([]),
            json!({"tools": {}}),
        );
        let b = snap(
            json!([{"name": "echo", "description": "new", "inputSchema": {"type": "object"}}, {"name": "fresh", "inputSchema": {"type": "object"}}]),
            json!([]),
            json!({"tools": {}, "logging": {}}),
        );
        let d = diff(Some(&a), &b);
        assert_eq!((d.breaking, d.compatible, d.cosmetic), (1, 2, 1));
        assert!(d.has_breaking());
        let first = &d.changes()[0];
        assert_eq!(
            (first.severity, first.name.as_str(), first.summary.as_str()),
            (Severity::Breaking, "gone", "tool removed")
        );
        assert_eq!(d.summary(), "1 breaking · 2 compatible · 1 cosmetic");
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["outcome"], "changed");
    }

    #[test]
    fn a_list_that_failed_is_not_reported_as_removed() {
        let before = snap(
            json!([{"name": "echo", "inputSchema": {"type": "object"}}]),
            json!([{"name": "p"}]),
            json!({"tools": {}, "prompts": {}}),
        );
        let mut after = snap(json!([]), json!([]), json!({"tools": {}, "prompts": {}}));
        after.list_failures.push(mcp_core::ListFailure {
            method: "tools/list".into(),
            error: "request timed out after 60s".into(),
        });
        let d = diff(Some(&before), &after);
        let summaries: Vec<&str> = d.changes().iter().map(|c| c.summary.as_str()).collect();
        assert!(!summaries.contains(&"tool removed"), "{summaries:?}");
        assert_eq!(summaries, ["prompt removed"], "prompts are still compared");
        assert_eq!(d.skipped, ["tools/list"]);
        assert_eq!(d.summary(), "1 breaking · tools/list not compared");
        assert_eq!(
            serde_json::to_value(&d).unwrap()["skipped"],
            json!(["tools/list"])
        );

        // The same holds with the failure on the earlier side.
        let d = diff(Some(&after), &before);
        let summaries: Vec<&str> = d.changes().iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["prompt added"]);
        assert_eq!(d.skipped, ["tools/list"]);
    }

    #[test]
    fn a_skipped_list_is_named_even_when_nothing_else_changed() {
        // Both sides advertise no tools, so the digests match; the later
        // side never read its tools, so that is not a clean pass.
        let before = snap(json!([]), json!([]), json!({"tools": {}}));
        let mut after = before.clone();
        after.list_failures.push(mcp_core::ListFailure {
            method: "tools/list".into(),
            error: "server error -32603: tools are unavailable".into(),
        });
        let d = diff(Some(&before), &after);
        assert_eq!(d.outcome, Outcome::Unchanged);
        assert_eq!(d.skipped, ["tools/list"]);
        assert_eq!(d.summary(), "unchanged · tools/list not compared");

        let complete = diff(Some(&before), &before);
        assert!(complete.skipped.is_empty());
        assert_eq!(complete.not_compared(), None);
        let json = serde_json::to_value(&complete).unwrap();
        assert!(json.get("skipped").is_none(), "{json}");
        let back: SnapshotDiff = serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn prompts_and_capabilities() {
        let a = snap(
            json!([]),
            json!([{"name": "p", "arguments": [{"name": "x"}]}]),
            json!({"prompts": {}, "resources": {}}),
        );
        let b = snap(
            json!([]),
            json!([{"name": "p", "arguments": [{"name": "x", "required": true}, {"name": "y", "required": true}]}]),
            json!({"prompts": {}}),
        );
        let d = diff(Some(&a), &b);
        let summaries: Vec<&str> = d.changes().iter().map(|c| c.summary.as_str()).collect();
        assert!(summaries.contains(&"argument is now required"));
        assert!(summaries.contains(&"required argument added"));
        assert!(summaries.contains(&"capability `resources` removed"));
        assert_eq!(d.breaking, 3);
    }
}
