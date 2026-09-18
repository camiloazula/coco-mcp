//! A point-in-time capture of everything a server advertises.
//!
//! All schema-bearing fields are `serde_json::Value` so unknown spec fields
//! survive a round trip through the store and the CLI. Wire names are
//! camelCase, matching the MCP JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// A tool advertised by `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Unique name.
    pub name: String,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the arguments.
    pub input_schema: Value,
    /// JSON Schema for `structuredContent`, if declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Behaviour hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    /// Any other fields the server sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A concrete resource advertised by `resources/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    /// URI to read.
    pub uri: String,
    /// Programmatic name.
    pub name: String,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Size in bytes, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Any other fields the server sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A resource template advertised by `resources/templates/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    /// RFC 6570 URI template.
    pub uri_template: String,
    /// Programmatic name.
    pub name: String,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type, if uniform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Any other fields the server sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One argument of a prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptArgument {
    /// Argument name.
    pub name: String,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the argument must be supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// A prompt advertised by `prompts/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// Unique name.
    pub name: String,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<PromptArgument>,
    /// Any other fields the server sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The list requests a snapshot is taken with, by JSON-RPC method name. The
/// session records a failure under one of these and every reader matches
/// against the same constant, so a misspelt name cannot compile.
pub mod list_method {
    /// Tools.
    pub const TOOLS: &str = "tools/list";
    /// Concrete resources.
    pub const RESOURCES: &str = "resources/list";
    /// Resource templates.
    pub const RESOURCE_TEMPLATES: &str = "resources/templates/list";
    /// Prompts.
    pub const PROMPTS: &str = "prompts/list";
    /// All four, in the order a snapshot lists them.
    pub const ALL: [&str; 4] = [TOOLS, RESOURCES, RESOURCE_TEMPLATES, PROMPTS];
}

/// A list the server advertised but could not deliver when the snapshot was
/// taken. Its field in [`Snapshot`] is left empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFailure {
    /// The list request, one of [`list_method`]. A string rather than an
    /// enum so a snapshot file naming a method this version does not know
    /// still parses.
    pub method: String,
    /// What went wrong, as the error's text.
    pub error: String,
}

/// Everything a server advertised at one moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Negotiated protocol version.
    pub protocol_version: String,
    /// `serverInfo` from `initialize` (name, version, ...).
    pub server_info: Value,
    /// `capabilities` from `initialize`.
    pub capabilities: Value,
    /// `instructions` from `initialize`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Tools, in server order.
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Resources, in server order.
    #[serde(default)]
    pub resources: Vec<Resource>,
    /// Resource templates, in server order.
    #[serde(default)]
    pub resource_templates: Vec<ResourceTemplate>,
    /// Prompts, in server order.
    #[serde(default)]
    pub prompts: Vec<Prompt>,
    /// Lists that failed, whose fields above are empty for that reason.
    /// Absent from the JSON of a complete snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list_failures: Vec<ListFailure>,
    /// When the snapshot was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
}

impl Snapshot {
    /// Server name from `serverInfo`, if present.
    pub fn server_name(&self) -> Option<&str> {
        self.server_info.get("name").and_then(Value::as_str)
    }

    /// Server version from `serverInfo`, if present.
    pub fn server_version(&self) -> Option<&str> {
        self.server_info.get("version").and_then(Value::as_str)
    }

    /// Whether the server declared a capability (`tools`, `resources`, `prompts`, ...).
    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.get(name).is_some_and(|v| !v.is_null())
    }

    /// Whether `method` (`tools/list`, `resources/list`, ...) failed, so its
    /// list is empty for want of an answer rather than by the server's word.
    pub fn list_failed(&self, method: &str) -> bool {
        self.list_failures.iter().any(|f| f.method == method)
    }

    /// The digest-relevant content: what the server advertises, without
    /// `taken_at` and without the lists that failed.
    pub fn content(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(obj) = value.as_object_mut() {
            obj.remove("takenAt");
            obj.remove("listFailures");
        }
        value
    }

    /// Stable SHA-256 hex digest of [`Self::content`] with sorted object keys.
    pub fn digest(&self) -> String {
        let canonical = canonicalize(self.content());
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        hex::encode(Sha256::digest(bytes))
    }

    /// Find a tool by name.
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// Find a prompt by name.
    pub fn prompt(&self, name: &str) -> Option<&Prompt> {
        self.prompts.iter().find(|p| p.name == name)
    }

    /// Find a resource by URI.
    pub fn resource(&self, uri: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.uri == uri)
    }
}

/// Recursively sort object keys so serialization is order-independent.
///
/// Hand-written (about ten lines) rather than pulling in a canonical-JSON
/// crate: we only need key ordering, not full RFC 8785 number formatting.
pub fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, canonicalize(v)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Snapshot {
        serde_json::from_value(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "mock", "version": "1.0"},
            "capabilities": {"tools": {"listChanged": true}},
            "tools": [{
                "name": "echo",
                "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                "customField": 42
            }],
            "prompts": [{"name": "greet", "arguments": [{"name": "who", "required": true}]}],
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn unknown_fields_survive_round_trip() {
        let snap = sample();
        assert_eq!(snap.tools[0].extra["customField"], 42);
        let back: Snapshot = serde_json::from_value(serde_json::to_value(&snap).unwrap()).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn digest_ignores_taken_at_and_key_order() {
        let a = sample();
        let mut b = sample();
        b.taken_at = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(a.digest(), b.digest());

        let mut c = sample();
        c.tools[0].input_schema =
            json!({"properties": {"text": {"type": "string"}}, "type": "object"});
        assert_eq!(a.digest(), c.digest());

        let mut d = sample();
        d.tools[0].description = Some("changed".into());
        assert_ne!(a.digest(), d.digest());
    }

    #[test]
    fn list_failures_round_trip_and_stay_out_of_the_digest() {
        let complete = sample();
        let mut partial = sample();
        partial.list_failures.push(ListFailure {
            method: "resources/list".into(),
            error: "server error -32603: resources are unavailable".into(),
        });
        let json = serde_json::to_value(&partial).unwrap();
        assert_eq!(json["listFailures"][0]["method"], "resources/list");
        let back: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(back, partial);
        assert!(back.list_failed("resources/list"));
        assert!(!back.list_failed("tools/list"));
        assert_eq!(partial.digest(), complete.digest());

        // A method this version does not know still parses.
        let mut unknown = serde_json::to_value(&complete).unwrap();
        unknown["listFailures"] = json!([{"method": "widgets/list", "error": "boom"}]);
        let parsed: Snapshot = serde_json::from_value(unknown).unwrap();
        assert!(parsed.list_failed("widgets/list"));
    }

    #[test]
    fn a_complete_snapshot_has_no_list_failures_field() {
        let json = serde_json::to_value(sample()).unwrap();
        assert!(json.get("listFailures").is_none(), "{json}");
    }

    #[test]
    fn helpers_find_items() {
        let snap = sample();
        assert_eq!(snap.server_name(), Some("mock"));
        assert!(snap.has_capability("tools"));
        assert!(!snap.has_capability("resources"));
        assert!(snap.tool("echo").is_some());
        assert!(snap.prompt("greet").is_some());
        assert!(snap.resource("x").is_none());
    }
}
