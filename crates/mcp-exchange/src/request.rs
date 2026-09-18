//! Reproducing a request outside Coco MCP: as a JSON-RPC message, or as
//! a `curl` command for a streamable HTTP server.

use std::collections::BTreeMap;

use mcp_core::AuthRef;
use serde_json::{Value, json};

/// Environment variable a redacted `curl` command reads its token from.
pub const CURL_TOKEN_VAR: &str = "MCP_TOKEN";

/// Which JSON-RPC frame a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// A call that expects an answer.
    Request,
    /// A call that does not.
    Notification,
    /// A successful answer.
    Response,
    /// A failed answer.
    Error,
}

/// The JSON-RPC 2.0 message Coco MCP would send. The id is always `1`:
/// the copy is a standalone request, not a message from a live session.
pub fn json_rpc(method: &str, params: &Value) -> Value {
    wire_message(Frame::Request, Some(&json!(1)), method, params)
}

/// Rebuild a message from the parts the log keeps.
///
/// The log drawer shows one field of each frame, because that is what a
/// person reads; copying a row should still give the frame that went over
/// the wire, which is what a bug report or a replay needs.
pub fn wire_message(frame: Frame, id: Option<&Value>, method: &str, payload: &Value) -> Value {
    let id = id.cloned().unwrap_or(Value::Null);
    match frame {
        Frame::Request | Frame::Notification => {
            let mut message = json!({ "jsonrpc": "2.0" });
            if let Some(object) = message.as_object_mut() {
                if frame == Frame::Request {
                    object.insert("id".into(), id);
                }
                object.insert("method".into(), Value::String(method.to_owned()));
                // An omitted `params` and a null one are not the same thing.
                if !payload.is_null() {
                    object.insert("params".into(), payload.clone());
                }
            }
            message
        }
        Frame::Response => json!({ "jsonrpc": "2.0", "id": id, "result": payload }),
        Frame::Error => json!({ "jsonrpc": "2.0", "id": id, "error": payload }),
    }
}

/// A request to render as `curl`.
#[derive(Debug)]
pub struct Curl<'a> {
    /// Endpoint of the streamable HTTP server.
    pub url: &'a str,
    /// Static headers from the server's settings.
    pub headers: &'a BTreeMap<String, String>,
    /// How the server authenticates.
    pub auth: &'a AuthRef,
    /// The JSON-RPC message to post.
    pub body: &'a Value,
}

/// A `curl` command that posts `body` to the server.
///
/// The two MCP headers are always present: a streamable HTTP endpoint answers
/// either JSON or an event stream, and rejects a request that does not accept
/// both. The token is always `$MCP_TOKEN`, which the shell expands, so the
/// command runs once that variable is exported.
pub fn curl(request: Curl<'_>) -> String {
    let mut headers: Vec<(String, String)> = vec![
        ("Content-Type".to_owned(), "application/json".to_owned()),
        (
            "Accept".to_owned(),
            "application/json, text/event-stream".to_owned(),
        ),
    ];
    for (name, value) in request.headers {
        headers.push((name.clone(), value.clone()));
    }
    let mut lines = vec![format!("curl -sS -X POST {}", quote(request.url))];
    for (name, value) in headers {
        lines.push(format!("  -H {}", quote(&format!("{name}: {value}"))));
    }
    if let Some(authorization) = authorization(&request) {
        lines.push(format!("  -H {authorization}"));
    }
    lines.push(format!(
        "  -d {}",
        quote(&serde_json::to_string(request.body).unwrap_or_else(|_| "{}".to_owned()))
    ));
    format!("{}\n", lines.join(" \\\n"))
}

/// The `Authorization` header, or `None` for an unauthenticated server. The
/// token is the placeholder, double-quoted so the shell expands it.
fn authorization(request: &Curl<'_>) -> Option<String> {
    match request.auth {
        AuthRef::None => None,
        _ => Some(format!("\"Authorization: Bearer ${CURL_TOKEN_VAR}\"")),
    }
}

/// Single-quote for a POSIX shell: `it's` becomes `'it'\''s'`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json_rpc("tools/call", &json!({"name": "get_weather"}))
    }

    #[test]
    fn frames_round_trip_what_the_log_kept() {
        let params = json!({"name": "get_weather"});
        assert_eq!(
            wire_message(Frame::Request, Some(&json!(7)), "tools/call", &params),
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": params})
        );
        assert_eq!(
            wire_message(
                Frame::Notification,
                None,
                "notifications/initialized",
                &Value::Null
            ),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            "no id, and no params key when there were none"
        );
        assert_eq!(
            wire_message(
                Frame::Response,
                Some(&json!("a")),
                "tools/call",
                &json!({"ok": true})
            ),
            json!({"jsonrpc": "2.0", "id": "a", "result": {"ok": true}})
        );
        let error = json!({"code": -32601, "message": "no such tool"});
        assert_eq!(
            wire_message(Frame::Error, None, "tools/call", &error),
            json!({"jsonrpc": "2.0", "id": null, "error": error})
        );
    }

    #[test]
    fn json_rpc_is_a_standalone_request() {
        assert_eq!(
            body(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "get_weather"},
            })
        );
    }

    #[test]
    fn curl_writes_the_token_as_a_placeholder() {
        let headers = BTreeMap::from([("X-Tenant".to_owned(), "acme".to_owned())]);
        let auth = AuthRef::Bearer {
            keyring_id: "srv".to_owned(),
        };
        let text = curl(Curl {
            url: "https://api.example.com/mcp",
            headers: &headers,
            auth: &auth,
            body: &body(),
        });
        assert!(!text.contains("s3cret"), "{text}");
        assert!(
            text.contains("\"Authorization: Bearer $MCP_TOKEN\""),
            "{text}"
        );
        assert!(text.contains("-H 'Accept: application/json, text/event-stream'"));
        assert!(text.contains("-H 'X-Tenant: acme'"));
        assert!(text.contains(r#"-d '{"jsonrpc":"2.0","id":1,"method":"tools/call""#));
    }

    #[test]
    fn an_oauth_server_gets_the_placeholder_too() {
        let auth = AuthRef::OAuth {
            keyring_id: "srv".to_owned(),
        };
        let text = curl(Curl {
            url: "https://api.example.com/mcp",
            headers: &BTreeMap::new(),
            auth: &auth,
            body: &body(),
        });
        assert!(text.contains("$MCP_TOKEN"), "{text}");
    }

    #[test]
    fn no_authorization_header_without_auth() {
        let text = curl(Curl {
            url: "https://api.example.com/mcp",
            headers: &BTreeMap::new(),
            auth: &AuthRef::None,
            body: &body(),
        });
        assert!(!text.contains("Authorization"), "{text}");
    }

    #[test]
    fn quotes_survive_the_shell() {
        let text = curl(Curl {
            url: "https://example.com/it's",
            headers: &BTreeMap::new(),
            auth: &AuthRef::None,
            body: &json!({"q": "it's"}),
        });
        assert!(text.contains(r"'https://example.com/it'\''s'"), "{text}");
        assert!(text.contains(r#"'{"q":"it'\''s"}'"#), "{text}");
    }
}
