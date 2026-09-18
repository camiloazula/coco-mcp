//! What a server can do now: one table, worked out from the protocol era it
//! agreed on, the capabilities it declared and whether it is connected.
//!
//! Every view, menu item and palette entry that offers one of these asks
//! here rather than reading capabilities itself, so a feature is enabled
//! only where the agreed version and the server support it, and a control
//! that cannot run says why.

use mcp_core::Era;
use serde_json::Value;

use crate::state::Status;

/// Why a feature 2026-07-28 took out cannot run.
pub const REMOVED: &str = "Removed in 2026-07-28";
/// The note beside a feature 2026-07-28 keeps but deprecates.
pub const DEPRECATED: &str = "Deprecated in 2026-07-28";
/// Why a feature that needs a session cannot run without one.
pub const CONNECT: &str = "Connect to use this";

/// A feature the app offers, as the feature table lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Listing and calling tools.
    Tools,
    /// Listing and reading resources and templates.
    Resources,
    /// Listing and getting prompts.
    Prompts,
    /// Suggestions for prompt arguments and template variables.
    Completions,
    /// Asking the server to send log messages from a level.
    LogLevel,
    /// Hearing when a subscribed resource changes.
    Subscriptions,
    /// Hearing when a list changes.
    ListChanges,
    /// Answering the server's sampling requests.
    Sampling,
    /// Answering the server's elicitation requests, form and URL.
    Elicitation,
    /// Offering roots when the server asks.
    Roots,
    /// Telling the server the roots changed when they are saved.
    RootsChanged,
    /// Pinging a server that has gone quiet.
    Keepalive,
}

/// Whether a feature can run, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Works as it always has.
    Available,
    /// Works, through a mechanism the era changed, described.
    AvailableAs(&'static str),
    /// Works but is deprecated in the agreed version; how it works now.
    Deprecated(&'static str),
    /// Cannot run, for this reason.
    Unavailable(&'static str),
}

impl Availability {
    /// Whether the feature can run.
    pub fn is_available(&self) -> bool {
        !matches!(self, Self::Unavailable(_))
    }

    /// Why it cannot run.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            _ => None,
        }
    }

    /// Whether it is deprecated in the agreed version.
    pub fn is_deprecated(&self) -> bool {
        matches!(self, Self::Deprecated(_))
    }

    /// How it works in this era, when that is not how it always has.
    pub fn mechanism(&self) -> Option<&'static str> {
        match self {
            Self::AvailableAs(how) | Self::Deprecated(how) => Some(how),
            _ => None,
        }
    }
}

/// The feature table of one server.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    connected: bool,
    /// The era of the agreed version; `None` before a session ever started.
    era: Option<Era>,
    capabilities: Value,
}

impl Features {
    /// The table for a server in `status`, whose session agreed on a version
    /// of `era` and declared `capabilities`.
    pub fn of(status: &Status, era: Option<Era>, capabilities: &Value) -> Self {
        Self {
            connected: *status == Status::Connected,
            era,
            capabilities: capabilities.clone(),
        }
    }

    /// The era the table was worked out for.
    pub fn era(&self) -> Option<Era> {
        self.era
    }

    fn modern(&self) -> bool {
        self.era == Some(Era::Modern)
    }

    fn declares(&self, name: &str) -> bool {
        self.capabilities.get(name).is_some_and(|v| !v.is_null())
    }

    fn flag(&self, name: &str, key: &str) -> bool {
        self.capabilities
            .get(name)
            .and_then(|c| c.get(key))
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// Whether `feature` can run on this server now, and how.
    pub fn get(&self, feature: Feature) -> Availability {
        use Availability::{Available, AvailableAs, Deprecated, Unavailable};
        match feature {
            Feature::Tools if !self.declares("tools") => {
                Unavailable("The server doesn't declare tools")
            }
            Feature::Resources if !self.declares("resources") => {
                Unavailable("The server doesn't declare resources")
            }
            Feature::Prompts if !self.declares("prompts") => {
                Unavailable("The server doesn't declare prompts")
            }
            Feature::Tools | Feature::Resources | Feature::Prompts => Available,
            Feature::Completions => {
                if !self.connected {
                    Unavailable(CONNECT)
                } else if !self.declares("completions") {
                    Unavailable("The server doesn't declare completions")
                } else {
                    Available
                }
            }
            Feature::LogLevel => {
                if !self.connected {
                    Unavailable(CONNECT)
                } else if !self.declares("logging") {
                    Unavailable("The server doesn't declare logging")
                } else if self.modern() {
                    Deprecated("Sent with each request; the server logs only while one runs")
                } else {
                    Available
                }
            }
            Feature::Subscriptions => {
                if !self.connected {
                    Unavailable(CONNECT)
                } else if !self.flag("resources", "subscribe") {
                    Unavailable("The server doesn't offer resource subscriptions")
                } else if self.modern() {
                    AvailableAs("Through the subscriptions/listen stream")
                } else {
                    Available
                }
            }
            Feature::ListChanges => {
                let announces = ["tools", "resources", "prompts"]
                    .iter()
                    .any(|list| self.flag(list, "listChanged"));
                if !self.connected {
                    Unavailable(CONNECT)
                } else if !announces {
                    Unavailable("The server doesn't announce list changes")
                } else if self.modern() {
                    AvailableAs("Opted into on the subscriptions/listen stream")
                } else {
                    Available
                }
            }
            Feature::Sampling if self.modern() => Deprecated("Asked for inside a request"),
            Feature::Elicitation if self.modern() => AvailableAs("Asked for inside a request"),
            Feature::Roots if self.modern() => Deprecated("Asked for inside a request"),
            Feature::Sampling | Feature::Elicitation | Feature::Roots => Available,
            Feature::RootsChanged if self.modern() => Unavailable(REMOVED),
            Feature::RootsChanged if !self.connected => Unavailable(CONNECT),
            Feature::RootsChanged => Available,
            Feature::Keepalive if self.modern() => Unavailable(REMOVED),
            Feature::Keepalive if !self.connected => Unavailable(CONNECT),
            Feature::Keepalive => Available,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: [Feature; 12] = [
        Feature::Tools,
        Feature::Resources,
        Feature::Prompts,
        Feature::Completions,
        Feature::LogLevel,
        Feature::Subscriptions,
        Feature::ListChanges,
        Feature::Sampling,
        Feature::Elicitation,
        Feature::Roots,
        Feature::RootsChanged,
        Feature::Keepalive,
    ];

    fn full() -> Value {
        json!({
            "tools": {"listChanged": true},
            "resources": {"subscribe": true, "listChanged": true},
            "prompts": {},
            "logging": {},
            "completions": {},
        })
    }

    #[test]
    fn a_legacy_server_that_declares_everything_offers_everything() {
        let table = Features::of(&Status::Connected, Some(Era::Legacy), &full());
        for feature in ALL {
            assert_eq!(table.get(feature), Availability::Available, "{feature:?}");
        }
    }

    #[test]
    fn a_modern_server_works_differently_and_drops_what_the_revision_removed() {
        let table = Features::of(&Status::Connected, Some(Era::Modern), &full());
        for feature in [
            Feature::Tools,
            Feature::Resources,
            Feature::Prompts,
            Feature::Completions,
        ] {
            assert_eq!(table.get(feature), Availability::Available, "{feature:?}");
        }
        for feature in [Feature::LogLevel, Feature::Sampling, Feature::Roots] {
            assert!(table.get(feature).is_deprecated(), "{feature:?}");
            assert!(table.get(feature).is_available(), "{feature:?}");
        }
        for feature in [
            Feature::Subscriptions,
            Feature::ListChanges,
            Feature::Elicitation,
        ] {
            assert!(
                matches!(table.get(feature), Availability::AvailableAs(_)),
                "{feature:?}"
            );
        }
        for feature in [Feature::RootsChanged, Feature::Keepalive] {
            assert_eq!(table.get(feature).reason(), Some(REMOVED), "{feature:?}");
        }
    }

    #[test]
    fn a_server_that_declares_nothing_says_what_is_missing() {
        for era in [Era::Legacy, Era::Modern] {
            let table = Features::of(&Status::Connected, Some(era), &json!({}));
            for (feature, missing) in [
                (Feature::Tools, "tools"),
                (Feature::Resources, "resources"),
                (Feature::Prompts, "prompts"),
                (Feature::Completions, "completions"),
                (Feature::LogLevel, "logging"),
                (Feature::Subscriptions, "resource subscriptions"),
                (Feature::ListChanges, "list changes"),
            ] {
                let reason = table.get(feature).reason().unwrap_or_default();
                assert!(reason.contains(missing), "{era:?} {feature:?}: {reason}");
            }
            // What the client declares does not depend on the server.
            for feature in [Feature::Sampling, Feature::Elicitation, Feature::Roots] {
                assert!(table.get(feature).is_available(), "{era:?} {feature:?}");
            }
        }
    }

    #[test]
    fn a_partial_server_offers_only_what_it_declared() {
        let table = Features::of(
            &Status::Connected,
            Some(Era::Legacy),
            &json!({"resources": {}, "logging": {}}),
        );
        assert!(table.get(Feature::Resources).is_available());
        assert!(table.get(Feature::LogLevel).is_available());
        assert!(!table.get(Feature::Subscriptions).is_available());
        assert!(!table.get(Feature::ListChanges).is_available());
        assert!(!table.get(Feature::Tools).is_available());
    }

    #[test]
    fn a_disconnected_server_asks_to_connect_for_what_needs_a_session() {
        for status in [
            Status::Off,
            Status::Connecting,
            Status::Error("gone".into()),
        ] {
            let table = Features::of(&status, Some(Era::Legacy), &full());
            for feature in [
                Feature::Completions,
                Feature::LogLevel,
                Feature::Subscriptions,
                Feature::ListChanges,
                Feature::RootsChanged,
                Feature::Keepalive,
            ] {
                assert_eq!(table.get(feature).reason(), Some(CONNECT), "{feature:?}");
            }
            // What it listed is still there to read.
            assert!(table.get(Feature::Tools).is_available());
        }
        let never = Features::of(&Status::Off, None, &Value::Null);
        assert_eq!(never.era(), None);
        assert!(!never.get(Feature::Tools).is_available());
    }
}
