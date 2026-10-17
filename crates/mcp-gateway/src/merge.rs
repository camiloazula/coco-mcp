//! What merging several servers into one takes that needs no I/O: the names
//! an agent sees, which server a name or a URI belongs to, and the
//! capabilities of the whole.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

/// Between a namespace and a tool or prompt name.
pub(crate) const SEPARATOR: &str = "__";
/// The longest tool name the protocol allows; a longer merged one is left out.
pub(crate) const MAX_NAME: usize = 128;

/// `namespace__name`, or `None` when that is longer than a name may be.
pub(crate) fn prefixed(namespace: &str, name: &str) -> Option<String> {
    let merged = format!("{namespace}{SEPARATOR}{name}");
    (merged.len() <= MAX_NAME).then_some(merged)
}

/// Which namespace a merged name belongs to, and the name there. A namespace
/// may hold `_` and a name `__`, so every namespace is tried and the longest
/// that fits wins: `a_b__t` is `t` of `a_b`, not `b__t` of `a`.
pub(crate) fn split<'a>(
    merged: &'a str,
    namespaces: impl IntoIterator<Item = &'a str>,
) -> Option<(&'a str, &'a str)> {
    namespaces
        .into_iter()
        .filter_map(|namespace| {
            let rest = merged.strip_prefix(namespace)?.strip_prefix(SEPARATOR)?;
            Some((namespace, rest))
        })
        .max_by_key(|(namespace, _)| namespace.len())
}

/// Whether `uri` is what `template` (RFC 6570, simple `{variable}`
/// expressions) expands to for some values: its literal parts in order, and
/// at least one character where each expression stands.
pub(crate) fn template_matches(template: &str, uri: &str) -> bool {
    // The literal pieces around the expressions.
    let mut literals: Vec<&str> = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        literals.push(&rest[..open]);
        match rest[open..].find('}') {
            Some(close) => rest = &rest[open + close + 1..],
            None => return false,
        }
    }
    literals.push(rest);
    let Some(mut tail) = uri.strip_prefix(literals[0]) else {
        return false;
    };
    let expressions = literals.len() - 1;
    if expressions == 0 {
        return tail.is_empty();
    }
    for (i, literal) in literals.iter().enumerate().skip(1) {
        // The expression before this literal takes a character at least.
        let Some(first) = tail.chars().next() else {
            return false;
        };
        let after = &tail[first.len_utf8()..];
        if i == expressions {
            return after.ends_with(literal);
        }
        if literal.is_empty() {
            tail = after;
        } else {
            match after.find(literal) {
                Some(at) => tail = &after[at + literal.len()..],
                None => return false,
            }
        }
    }
    false
}

/// The resource URIs a tool result points at: `resource_link` blocks and
/// embedded `resource` blocks, which the server will serve reads of.
pub(crate) fn uris_in_result(result: &Value) -> Vec<String> {
    let mut found = Vec::new();
    let blocks = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("resource_link") => {
                if let Some(uri) = block.get("uri").and_then(Value::as_str) {
                    found.push(uri.to_owned());
                }
            }
            Some("resource") => {
                if let Some(uri) = block
                    .get("resource")
                    .and_then(|r| r.get("uri"))
                    .and_then(Value::as_str)
                {
                    found.push(uri.to_owned());
                }
            }
            _ => {}
        }
    }
    found
}

/// What the gateway tells an agent it can do: what any server can, with a
/// flag set when any server sets it.
pub(crate) fn capabilities<'a>(servers: impl IntoIterator<Item = &'a Value>) -> Value {
    let mut merged: Map<String, Value> = Map::new();
    for capabilities in servers {
        let Some(object) = capabilities.as_object() else {
            continue;
        };
        for key in ["tools", "resources", "prompts", "logging", "completions"] {
            let Some(value) = object.get(key) else {
                continue;
            };
            let entry = merged.entry(key).or_insert_with(|| json!({}));
            if let (Some(entry), Some(flags)) = (entry.as_object_mut(), value.as_object()) {
                for (flag, on) in flags {
                    if on.as_bool() == Some(true) {
                        entry.insert(flag.clone(), Value::Bool(true));
                    } else {
                        entry.entry(flag.clone()).or_insert_with(|| on.clone());
                    }
                }
            }
        }
    }
    Value::Object(merged)
}

/// Where each resource URI, template and prompt a server listed belongs.
#[derive(Debug, Default)]
pub(crate) struct Owners {
    /// A listed or linked URI, by the namespaces that served it.
    uris: HashMap<String, Vec<String>>,
    /// A listed template, by the namespace that served it.
    templates: Vec<(String, String)>,
}

impl Owners {
    /// A concrete resource of `namespace`.
    pub(crate) fn uri(&mut self, namespace: &str, uri: &str) {
        let owners = self.uris.entry(uri.to_owned()).or_default();
        if !owners.iter().any(|n| n == namespace) {
            owners.push(namespace.to_owned());
        }
    }

    /// A resource template of `namespace`.
    pub(crate) fn template(&mut self, namespace: &str, template: &str) {
        let entry = (namespace.to_owned(), template.to_owned());
        if !self.templates.contains(&entry) {
            self.templates.push(entry);
        }
    }

    /// Forget the resources `namespace` listed, before it lists them again.
    pub(crate) fn forget_uris(&mut self, namespace: &str) {
        for owners in self.uris.values_mut() {
            owners.retain(|owner| owner != namespace);
        }
        self.uris.retain(|_, owners| !owners.is_empty());
    }

    /// Forget the templates `namespace` listed, before it lists them again.
    pub(crate) fn forget_templates(&mut self, namespace: &str) {
        self.templates.retain(|(owner, _)| owner != namespace);
    }

    /// The namespace that serves `uri`: the one that listed it or linked to
    /// it, else the first whose template it matches. `order` is the order
    /// servers are tried in, which is how a URI two servers hold is settled.
    pub(crate) fn owner<'a>(&'a self, uri: &str, order: &[&'a str]) -> Option<&'a str> {
        if let Some(owners) = self.uris.get(uri)
            && let Some(first) = order
                .iter()
                .copied()
                .find(|n| owners.iter().any(|o| o == n))
        {
            return Some(first);
        }
        order.iter().copied().find(|namespace| {
            self.templates
                .iter()
                .any(|(owner, template)| owner == namespace && template_matches(template, uri))
        })
    }

    /// URIs more than one server of `order` lists, for a warning.
    pub(crate) fn clashes(listed: &[(String, Vec<String>)]) -> Vec<String> {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        let mut clashes = Vec::new();
        for (namespace, uris) in listed {
            for uri in uris {
                match seen.get(uri.as_str()) {
                    Some(first) if *first != namespace.as_str() => {
                        if !clashes.contains(uri) {
                            clashes.push(uri.clone());
                        }
                    }
                    _ => {
                        seen.insert(uri, namespace);
                    }
                }
            }
        }
        clashes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_prefixed_and_split_back() {
        assert_eq!(
            prefixed("weather", "get_forecast").as_deref(),
            Some("weather__get_forecast")
        );
        assert_eq!(prefixed("a", &"x".repeat(MAX_NAME)), None, "over the limit");
        let namespaces = ["a", "a_b", "weather"];
        assert_eq!(
            split("weather__get_forecast", namespaces),
            Some(("weather", "get_forecast"))
        );
        // The longest namespace that fits, and a name that holds the separator.
        assert_eq!(split("a_b__tool", namespaces), Some(("a_b", "tool")));
        assert_eq!(split("a__b__tool", namespaces), Some(("a", "b__tool")));
        assert_eq!(split("nobody__tool", namespaces), None);
        assert_eq!(split("weather_tool", namespaces), None, "no separator");
    }

    #[test]
    fn templates_match_what_they_expand_to() {
        assert!(template_matches(
            "weather://city/{name}",
            "weather://city/oslo"
        ));
        assert!(!template_matches(
            "weather://city/{name}",
            "weather://city/"
        ));
        assert!(!template_matches(
            "weather://city/{name}",
            "weather://station/oslo"
        ));
        assert!(template_matches("file:///{path}", "file:///a/b/c.txt"));
        assert!(template_matches(
            "big://{kind}/{kilobytes}",
            "big://prose/12"
        ));
        assert!(!template_matches("big://{kind}/{kilobytes}", "big://prose"));
        assert!(template_matches("mock://plain", "mock://plain"));
        assert!(!template_matches("mock://plain", "mock://plain/more"));
        assert!(!template_matches("broken://{open", "broken://x"));
    }

    #[test]
    fn a_result_names_the_resources_it_points_at() {
        let result = json!({"content": [
            {"type": "text", "text": "hi"},
            {"type": "resource_link", "uri": "mock://a", "name": "a"},
            {"type": "resource", "resource": {"uri": "mock://b", "text": "b"}},
        ]});
        assert_eq!(uris_in_result(&result), ["mock://a", "mock://b"]);
        assert!(uris_in_result(&json!({"isError": true})).is_empty());
    }

    #[test]
    fn capabilities_are_what_any_server_can_do() {
        let a = json!({"tools": {"listChanged": true}, "logging": {}});
        let b = json!({"tools": {}, "resources": {"subscribe": true, "listChanged": false}});
        let all = capabilities([&a, &b]);
        assert_eq!(all["tools"]["listChanged"], true);
        assert_eq!(all["resources"]["subscribe"], true);
        assert_eq!(all["resources"]["listChanged"], false);
        assert!(all.get("logging").is_some());
        assert!(all.get("prompts").is_none(), "no server has prompts");
    }

    #[test]
    fn a_uri_belongs_to_the_server_that_listed_it() {
        let mut owners = Owners::default();
        owners.uri("a", "file:///x");
        owners.uri("b", "file:///x");
        owners.template("b", "weather://city/{name}");
        let order = ["a", "b"];
        assert_eq!(
            owners.owner("file:///x", &order),
            Some("a"),
            "the first listed"
        );
        assert_eq!(
            owners.owner("weather://city/oslo", &order),
            Some("b"),
            "by template"
        );
        assert_eq!(owners.owner("nowhere://x", &order), None);
        owners.forget_uris("a");
        assert_eq!(owners.owner("file:///x", &order), Some("b"));
        let listed = vec![
            (
                "a".to_owned(),
                vec!["file:///x".to_owned(), "mock://a".to_owned()],
            ),
            ("b".to_owned(), vec!["file:///x".to_owned()]),
        ];
        assert_eq!(Owners::clashes(&listed), ["file:///x"]);
    }
}
