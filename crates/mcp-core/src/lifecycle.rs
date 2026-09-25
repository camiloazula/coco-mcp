//! Which protocol era a session speaks, and how it starts in each.
//!
//! `rmcp` starts a client with the `initialize` handshake or with
//! `server/discover` ([`ClientLifecycleMode`]). This module picks the start a
//! [`ProtocolMode`] asks for and puts a failed start into words.

use rmcp::model::{ErrorCode, ErrorData, ProtocolVersion};
use rmcp::service::{ClientInitializeError, ClientLifecycleMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::spec::ProtocolMode;

/// The first protocol version without a handshake.
pub const MODERN_VERSION: &str = "2026-07-28";

/// The version the handshake offers; the server may answer with an older one.
pub const LEGACY_VERSION: &str = "2025-11-25";

/// The protocol era of an agreed version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Era {
    /// Agreed through `initialize`: 2024-11-05 to 2025-11-25.
    Legacy,
    /// 2026-07-28 and later: no handshake, every request self-contained.
    Modern,
}

impl Era {
    /// The era `version` belongs to. Versions are dates, so they compare as
    /// text.
    pub fn of(version: &str) -> Self {
        if version >= MODERN_VERSION {
            Self::Modern
        } else {
            Self::Legacy
        }
    }

    /// Its label in the app.
    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "Legacy",
            Self::Modern => "Modern",
        }
    }
}

/// How `rmcp` starts a session in `mode`.
pub(crate) fn lifecycle(mode: ProtocolMode) -> ClientLifecycleMode {
    match mode {
        ProtocolMode::Legacy => ClientLifecycleMode::Initialize,
        ProtocolMode::Modern => ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
        // Discovery asks for the modern version alone: a discover lifecycle
        // at a legacy version belongs to neither era. A server without it
        // gets the handshake instead.
        ProtocolMode::Auto => ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        },
    }
}

/// Whether an Auto start that failed with `error` should try the handshake
/// on a fresh transport. `rmcp` falls back by itself when the server does not
/// answer `server/discover`, but not when it answers without a version this
/// client prefers.
pub(crate) fn falls_back(error: &ClientInitializeError) -> bool {
    matches!(
        error,
        ClientInitializeError::NoCompatibleProtocolVersion { .. }
    )
}

/// A failed start in words, for a session started in `mode`.
pub(crate) fn describe(error: &ClientInitializeError, mode: ProtocolMode) -> String {
    match error {
        ClientInitializeError::NoCompatibleProtocolVersion {
            client_supported,
            server_supported,
        } => {
            let hint = if mode == ProtocolMode::Modern {
                " (choose Legacy or Auto)"
            } else {
                ""
            };
            format!(
                "the server supports protocol {}, not {}{hint}",
                versions(server_supported),
                versions(client_supported)
            )
        }
        ClientInitializeError::JsonRpcError(data) => describe_rpc(data, mode),
        ClientInitializeError::LegacyFallbackFailed { discover, fallback } => format!(
            "the server did not answer server/discover ({}), and the initialize handshake failed: {}",
            describe(discover, ProtocolMode::Auto),
            describe(fallback, ProtocolMode::Legacy)
        ),
        ClientInitializeError::TransportError { error, context } => {
            let cause = crate::error::transport_cause(error);
            if first_send(context) {
                return cause;
            }
            // The step names what failed after the server answered, which
            // the cause alone (`broken pipe`) does not.
            let step = match context.strip_prefix("send ") {
                Some(what) => format!("sending the {what}"),
                None => context.to_string(),
            };
            let answered = match context.as_ref() {
                "send initialized notification" => " the initialize request",
                _ => "",
            };
            format!("the server answered{answered}, then {step} failed: {cause}")
        }
        other => other.to_string(),
    }
}

/// Whether a start that failed with `error` got an answer from the server.
/// A transport that failed to carry the first request met a server that
/// could not be reached; one that failed later met a server that answered.
pub(crate) fn reached(error: &ClientInitializeError) -> bool {
    match error {
        ClientInitializeError::TransportError { context, .. } => !first_send(context),
        _ => true,
    }
}

/// Whether `context`, where `rmcp` says a transport failed during a start,
/// is the sending of the start's first request, before any answer.
fn first_send(context: &str) -> bool {
    matches!(context, "send initialize request" | "send discover request")
}

fn describe_rpc(data: &ErrorData, mode: ProtocolMode) -> String {
    let code = data.code;
    let message = &data.message;
    if code == ErrorCode::UNSUPPORTED_PROTOCOL_VERSION {
        let supported = data
            .data
            .as_ref()
            .and_then(|d| d.get("supported"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|list| !list.is_empty());
        match supported {
            Some(list) => format!(
                "the server does not support the requested protocol version; it supports {list} ({message})"
            ),
            None => {
                format!("the server does not support the requested protocol version ({message})")
            }
        }
    } else if code == ErrorCode::HEADER_MISMATCH {
        format!("the server found the request's headers and body disagree: {message}")
    } else if code == ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY {
        let wanted = data
            .data
            .as_ref()
            .map(|d| format!(" {d}"))
            .unwrap_or_default();
        format!(
            "the server requires a client capability this client does not declare: {message}{wanted}"
        )
    } else if mode == ProtocolMode::Modern {
        format!(
            "the server did not answer server/discover, so it may only speak the initialize handshake (choose Legacy or Auto): error {}: {message}",
            code.0
        )
    } else {
        format!("error {}: {message}", code.0)
    }
}

fn versions(list: &[ProtocolVersion]) -> String {
    if list.is_empty() {
        return "no version".into();
    }
    list.iter()
        .map(ProtocolVersion::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_belong_to_their_era() {
        assert_eq!(Era::of("2024-11-05"), Era::Legacy);
        assert_eq!(Era::of(LEGACY_VERSION), Era::Legacy);
        assert_eq!(Era::of(MODERN_VERSION), Era::Modern);
        assert_eq!(Era::of("2027-01-01"), Era::Modern);
        assert_eq!(ProtocolVersion::V_2026_07_28.as_str(), MODERN_VERSION);
        assert_eq!(ProtocolVersion::V_2025_11_25.as_str(), LEGACY_VERSION);
    }

    #[test]
    fn each_mode_starts_its_own_way() {
        assert_eq!(
            lifecycle(ProtocolMode::Legacy),
            ClientLifecycleMode::Initialize
        );
        assert!(matches!(
            lifecycle(ProtocolMode::Modern),
            ClientLifecycleMode::Discover { preferred_versions } if preferred_versions == [ProtocolVersion::V_2026_07_28]
        ));
        assert!(matches!(
            lifecycle(ProtocolMode::Auto),
            ClientLifecycleMode::Auto { preferred_versions, legacy_version: Some(legacy) }
                if preferred_versions == [ProtocolVersion::V_2026_07_28] && legacy == ProtocolVersion::V_2025_11_25
        ));
    }

    fn rpc(code: ErrorCode, data: Option<Value>) -> ClientInitializeError {
        ClientInitializeError::JsonRpcError(ErrorData::new(code, "refused", data))
    }

    #[test]
    fn protocol_errors_are_put_into_words() {
        let unsupported = describe(
            &rpc(
                ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
                Some(json!({"supported": ["2026-07-28"]})),
            ),
            ProtocolMode::Legacy,
        );
        assert!(
            unsupported.contains("it supports 2026-07-28"),
            "{unsupported}"
        );

        let header = describe(&rpc(ErrorCode::HEADER_MISMATCH, None), ProtocolMode::Modern);
        assert!(header.contains("headers and body disagree"), "{header}");

        let capability = describe(
            &rpc(
                ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY,
                Some(json!({"sampling": {}})),
            ),
            ProtocolMode::Modern,
        );
        assert!(capability.contains("does not declare"), "{capability}");
        assert!(capability.contains("sampling"), "{capability}");

        let legacy_server = describe(
            &rpc(ErrorCode::METHOD_NOT_FOUND, None),
            ProtocolMode::Modern,
        );
        assert!(legacy_server.contains("Legacy or Auto"), "{legacy_server}");
        let plain = describe(
            &rpc(ErrorCode::METHOD_NOT_FOUND, None),
            ProtocolMode::Legacy,
        );
        assert_eq!(plain, "error -32601: refused");
    }

    #[test]
    fn a_version_mismatch_names_both_sides() {
        let error = ClientInitializeError::NoCompatibleProtocolVersion {
            client_supported: vec![ProtocolVersion::V_2026_07_28],
            server_supported: vec![ProtocolVersion::V_2025_06_18, ProtocolVersion::V_2025_11_25],
        };
        assert!(falls_back(&error));
        assert_eq!(
            describe(&error, ProtocolMode::Modern),
            "the server supports protocol 2025-06-18, 2025-11-25, not 2026-07-28 (choose Legacy or Auto)"
        );
        assert_eq!(
            describe(&error, ProtocolMode::Auto),
            "the server supports protocol 2025-06-18, 2025-11-25, not 2026-07-28"
        );
        assert!(!falls_back(&rpc(ErrorCode::METHOD_NOT_FOUND, None)));
    }

    fn transport(kind: std::io::ErrorKind, context: &'static str) -> ClientInitializeError {
        ClientInitializeError::TransportError {
            error: rmcp::transport::DynamicTransportError::from_parts(
                "test",
                std::any::TypeId::of::<()>(),
                Box::new(std::io::Error::from(kind)),
            ),
            context: context.into(),
        }
    }

    #[test]
    fn a_server_never_reached_is_told_by_the_cause() {
        for context in ["send initialize request", "send discover request"] {
            let error = transport(std::io::ErrorKind::ConnectionRefused, context);
            assert!(!reached(&error), "{context}");
            assert_eq!(
                describe(&error, ProtocolMode::Auto),
                "connection refused",
                "{context}"
            );
        }
    }

    #[test]
    fn a_server_that_answered_keeps_the_step_that_failed() {
        let error = transport(
            std::io::ErrorKind::BrokenPipe,
            "send initialized notification",
        );
        assert!(reached(&error));
        assert_eq!(
            describe(&error, ProtocolMode::Legacy),
            "the server answered the initialize request, then sending the initialized notification failed: broken pipe"
        );
        let later = transport(std::io::ErrorKind::BrokenPipe, "send something else");
        assert!(reached(&later));
        assert_eq!(
            describe(&later, ProtocolMode::Legacy),
            "the server answered, then sending the something else failed: broken pipe"
        );
        assert!(reached(&rpc(ErrorCode::METHOD_NOT_FOUND, None)));
    }

    #[test]
    fn a_failed_fallback_says_what_both_attempts_met() {
        let error = ClientInitializeError::LegacyFallbackFailed {
            discover: Box::new(rpc(ErrorCode::METHOD_NOT_FOUND, None)),
            fallback: Box::new(rpc(
                ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
                Some(json!({"supported": ["2026-07-28"]})),
            )),
        };
        let text = describe(&error, ProtocolMode::Auto);
        assert!(text.contains("did not answer server/discover"), "{text}");
        assert!(text.contains("it supports 2026-07-28"), "{text}");
    }
}
