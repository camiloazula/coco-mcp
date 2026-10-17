//! What a JSON-RPC message is, read just far enough to route it.

use serde_json::Value;

use crate::observe::ClientInfo;

/// The part a message plays.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    /// A request, with the id its response will carry.
    Request(Value),
    /// A notification: a method and no id.
    Notification,
    /// A success or error response, with the id of its request.
    Response(Value),
    /// Anything else: passed on, never routed.
    Other,
}

/// Classify one message.
pub(crate) fn kind(message: &Value) -> Kind {
    let Some(object) = message.as_object() else {
        return Kind::Other;
    };
    let id = object.get("id").filter(|id| !id.is_null());
    if object.contains_key("method") {
        return match id {
            Some(id) => Kind::Request(id.clone()),
            None => Kind::Notification,
        };
    }
    match id {
        Some(id) if object.contains_key("result") || object.contains_key("error") => {
            Kind::Response(id.clone())
        }
        _ => Kind::Other,
    }
}

/// The key a request and its response are matched by: the id as JSON, so
/// `1` and `"1"` stay different ids.
pub(crate) fn id_key(id: &Value) -> String {
    id.to_string()
}

/// Who is connecting, from an `initialize` request.
pub(crate) fn client_info(message: &Value) -> Option<ClientInfo> {
    if message.get("method")?.as_str()? != "initialize" {
        return None;
    }
    let info = message.get("params")?.get("clientInfo")?;
    Some(ClientInfo {
        name: info.get("name")?.as_str()?.to_owned(),
        version: info
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_are_told_apart() {
        assert_eq!(
            kind(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})),
            Kind::Request(json!(1))
        );
        assert_eq!(
            kind(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
            Kind::Notification
        );
        assert_eq!(
            kind(&json!({"jsonrpc": "2.0", "id": "a", "result": {}})),
            Kind::Response(json!("a"))
        );
        assert_eq!(
            kind(&json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -1, "message": "x"}})),
            Kind::Response(json!(2))
        );
        // A null id is no id, and a bare object is nothing to route.
        assert_eq!(
            kind(&json!({"jsonrpc": "2.0", "id": null, "error": {}})),
            Kind::Other
        );
        assert_eq!(kind(&json!([1, 2])), Kind::Other);
        assert_ne!(id_key(&json!(1)), id_key(&json!("1")));
    }

    #[test]
    fn the_client_is_named_by_initialize() {
        let message = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"clientInfo": {"name": "agent", "version": "1.2"}}
        });
        assert_eq!(
            client_info(&message),
            Some(ClientInfo {
                name: "agent".into(),
                version: Some("1.2".into())
            })
        );
        assert_eq!(client_info(&json!({"method": "tools/list"})), None);
    }
}
