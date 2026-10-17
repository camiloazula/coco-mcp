//! What the gateway reports about the traffic it relays.

use std::time::Duration;

use mcp_core::EventKind;
use serde_json::Value;

use crate::Direction;
use crate::frame::{Kind, kind};

/// Who the agent says it is, from its `initialize` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// `clientInfo.name`.
    pub name: String,
    /// `clientInfo.version`, when it gave one.
    pub version: Option<String>,
}

/// One message that went by.
#[derive(Debug, Clone, Copy)]
pub struct Traffic<'a> {
    /// [`Direction::Outbound`] from the agent to the server,
    /// [`Direction::Inbound`] the other way.
    pub direction: Direction,
    /// The message, as it was.
    pub frame: &'a Value,
    /// For a response, the method of the request it answers.
    pub method: Option<&'a str>,
    /// For a response, the time since its request went by.
    pub elapsed: Option<Duration>,
}

impl Traffic<'_> {
    /// The event the log shows for it, or `None` for something that is no
    /// JSON-RPC message.
    pub fn event(&self) -> Option<EventKind> {
        let direction = self.direction;
        let object = self.frame.as_object()?;
        let params = || object.get("params").cloned();
        let method = || object.get("method")?.as_str().map(str::to_owned);
        match kind(self.frame) {
            Kind::Request(id) => Some(EventKind::Request {
                direction,
                id,
                method: method()?,
                params: params(),
            }),
            Kind::Notification => Some(EventKind::Notification {
                direction,
                method: method()?,
                params: params(),
            }),
            Kind::Response(id) => {
                let answers = self.method.map(str::to_owned);
                match object.get("error") {
                    Some(error) => Some(EventKind::Error {
                        direction,
                        id: Some(id),
                        method: answers,
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        data: error.get("data").cloned(),
                        elapsed: self.elapsed,
                    }),
                    None => Some(EventKind::Response {
                        direction,
                        id,
                        method: answers,
                        result: object.get("result").cloned().unwrap_or(Value::Null),
                        elapsed: self.elapsed,
                    }),
                }
            }
            Kind::Other => None,
        }
    }
}

/// Told about every agent connection and every message that goes through
/// it. Called from the gateway's tasks, so an implementation does its work
/// quickly and hands anything slow to a channel.
pub trait Observer: Send + Sync + 'static {
    /// An agent connected to `namespace`; `session` names the connection.
    fn opened(&self, _namespace: &str, _session: &str) {}

    /// The agent said who it is.
    fn client(&self, _namespace: &str, _session: &str, _info: ClientInfo) {}

    /// A message went by, unchanged.
    fn frame(&self, _namespace: &str, _session: &str, _traffic: Traffic<'_>) {}

    /// A line the server process wrote to its stderr.
    fn stderr(&self, _namespace: &str, _session: &str, _line: &str) {}

    /// The connection ended, and why.
    fn closed(&self, _namespace: &str, _session: &str, _reason: &str) {}
}

/// An observer that ignores everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct Silent;

impl Observer for Silent {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(direction: Direction, frame: Value, answers: Option<&str>) -> Option<EventKind> {
        Traffic {
            direction,
            frame: &frame,
            method: answers,
            elapsed: Some(Duration::from_millis(7)),
        }
        .event()
    }

    #[test]
    fn traffic_becomes_the_events_the_log_shows() {
        let request =
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"a": 1}});
        assert!(matches!(
            event(Direction::Outbound, request, None),
            Some(EventKind::Request { direction: Direction::Outbound, method, params: Some(_), .. })
                if method == "tools/call"
        ));
        let note = json!({"jsonrpc": "2.0", "method": "notifications/progress"});
        assert!(matches!(
            event(Direction::Inbound, note, None),
            Some(EventKind::Notification { params: None, .. })
        ));
        let result = json!({"jsonrpc": "2.0", "id": 4, "result": {"ok": true}});
        assert!(matches!(
            event(Direction::Inbound, result, Some("tools/call")),
            Some(EventKind::Response { method: Some(m), elapsed: Some(_), .. }) if m == "tools/call"
        ));
        let error = json!({"jsonrpc": "2.0", "id": 4,
            "error": {"code": -32602, "message": "bad", "data": {"x": 1}}});
        assert!(matches!(
            event(Direction::Inbound, error, Some("tools/call")),
            Some(EventKind::Error {
                code: -32602,
                data: Some(_),
                ..
            })
        ));
        assert!(event(Direction::Inbound, json!([1]), None).is_none());
    }
}
