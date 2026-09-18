//! Saved servers as the `mcpServers` block other MCP clients read.

use std::collections::BTreeMap;

use mcp_core::{AuthRef, ServerSpec};
use mcp_store::ServerRecord;
use serde_json::{Map, Value, json};

/// A client configuration and what could not be written into it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientConfig {
    /// The `mcpServers` object.
    pub value: Value,
    /// Credentials that were left out, one line each, for the person pasting
    /// the config to act on.
    pub notes: Vec<String>,
}

impl ClientConfig {
    /// The configuration as indented JSON.
    pub fn text(&self) -> String {
        crate::pretty(&self.value)
    }
}

/// Turn saved servers into the `mcpServers` block MCP clients read.
///
/// A bearer token becomes `Bearer <token>` and never leaves the keyring: this
/// output is meant to be committed to a repository or pasted into a chat.
pub fn client_config(servers: &[ServerRecord]) -> ClientConfig {
    let mut notes = Vec::new();
    let mut map = Map::new();
    for server in servers {
        let entry = match &server.spec {
            ServerSpec::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let mut entry = json!({ "command": command, "args": args });
                if let Some(obj) = entry.as_object_mut() {
                    if !env.is_empty() {
                        obj.insert("env".into(), json!(env));
                    }
                    if let Some(cwd) = cwd {
                        obj.insert("cwd".into(), json!(cwd));
                    }
                }
                entry
            }
            ServerSpec::Http { url, headers, auth } => {
                let mut headers = headers.clone();
                match auth {
                    AuthRef::None => {}
                    AuthRef::Bearer { .. } => {
                        headers.insert("Authorization".into(), "Bearer <token>".into());
                        notes.push(format!(
                            "`{}` uses a bearer token; it stays in the keyring, replace `<token>` yourself",
                            server.name
                        ));
                    }
                    AuthRef::OAuth { .. } => notes.push(format!(
                        "`{}` uses OAuth; the client will run its own authorization",
                        server.name
                    )),
                }
                let mut entry = json!({ "url": url });
                if !headers.is_empty()
                    && let Some(obj) = entry.as_object_mut()
                {
                    obj.insert("headers".into(), json!(headers));
                }
                entry
            }
        };
        map.insert(server.name.clone(), entry);
    }
    ClientConfig {
        value: json!({ "mcpServers": Value::Object(map) }),
        notes,
    }
}

/// A server read out of a client configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedServer {
    /// Name the configuration gave it.
    pub name: String,
    /// How to connect.
    pub spec: ServerSpec,
    /// A bearer token the file carried, to be put in the keyring. `None`
    /// when the file had a placeholder instead, which is what this crate's
    /// own export writes.
    pub token: Option<String>,
}

/// What reading a client configuration produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportedConfig {
    /// Servers in file order.
    pub servers: Vec<ImportedServer>,
    /// Entries that could not be read: the name, and why. Kept apart from
    /// `notes` so a caller can count them without reading English.
    pub skipped: Vec<(String, String)>,
    /// Advisory lines about servers that were read.
    pub notes: Vec<String>,
}

/// Read an `mcpServers` block.
///
/// The protocol specifies the wire format, not the config file, so this is a
/// convention rather than a standard. Accepts what clients write in
/// practice: the entries under `mcpServers`, under `servers`, or a bare
/// object. An entry is stdio when it has a `command` and HTTP when it has a
/// `url`; a `type` or `transport` field is only used to break a tie.
///
/// An `Authorization: Bearer` header is lifted out of the headers into
/// [`ImportedServer::token`], because a token belongs in the keyring and
/// `ServerSpec` keeps only non-secret headers. A placeholder value, such as
/// the `<token>` this crate exports, yields a bearer server with no token
/// and a note.
pub fn read_client_config(text: &str) -> Result<ImportedConfig, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let entries = value
        .get("mcpServers")
        .or_else(|| value.get("servers"))
        .unwrap_or(&value)
        .as_object()
        .ok_or_else(|| "no `mcpServers` object in this file".to_owned())?;
    let mut config = ImportedConfig::default();
    for (name, entry) in entries {
        match read_entry(name, entry) {
            Ok((server, note)) => {
                config.servers.push(server);
                config.notes.extend(note);
            }
            Err(reason) => config.skipped.push((name.clone(), reason)),
        }
    }
    if config.servers.is_empty() && config.skipped.is_empty() {
        return Err("no servers in this file".to_owned());
    }
    Ok(config)
}

fn read_entry(name: &str, entry: &Value) -> Result<(ImportedServer, Option<String>), String> {
    let entry = entry.as_object().ok_or("not an object")?;
    let text = |key: &str| entry.get(key).and_then(Value::as_str);
    let kind = text("type").or_else(|| text("transport")).unwrap_or("");
    let mut note = None;
    let (spec, token) = if let Some(url) = text("url").or_else(|| text("serverUrl")) {
        let mut headers = map_of(entry.get("headers"));
        let (auth, token) = match headers.remove("Authorization") {
            Some(value) => bearer(name, &value, &mut note),
            None => (AuthRef::None, None),
        };
        if kind == "sse" {
            note.get_or_insert_with(|| {
                format!("`{name}` is configured for SSE; Coco MCP speaks streamable HTTP")
            });
        }
        (
            ServerSpec::Http {
                url: url.to_owned(),
                headers,
                auth,
            },
            token,
        )
    } else if let Some(command) = text("command") {
        let args = entry
            .get("args")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        (
            ServerSpec::Stdio {
                command: command.to_owned(),
                args,
                env: map_of(entry.get("env")),
                cwd: text("cwd").map(Into::into),
            },
            None,
        )
    } else {
        return Err("no `command` and no `url`".to_owned());
    };
    Ok((
        ImportedServer {
            name: name.to_owned(),
            spec,
            token,
        },
        note,
    ))
}

/// Split an `Authorization` header into an auth mode and a real token.
fn bearer(name: &str, header: &str, note: &mut Option<String>) -> (AuthRef, Option<String>) {
    let value = header.trim_start_matches("Bearer").trim();
    let auth = AuthRef::Bearer {
        // The caller replaces this with the id of the server it creates.
        keyring_id: name.to_owned(),
    };
    // `<token>`, `$TOKEN` and `${TOKEN}` all mean "fill this in yourself",
    // and the first is what this crate exports.
    let placeholder = value.is_empty()
        || value.starts_with('<')
        || value.starts_with('$')
        || value.eq_ignore_ascii_case("bearer");
    if placeholder {
        *note = Some(format!(
            "`{name}` needs its bearer token; the file had a placeholder"
        ));
        return (auth, None);
    }
    (auth, Some(value.to_owned()))
}

fn map_of(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn record(name: &str, spec: ServerSpec) -> ServerRecord {
        ServerRecord {
            id: name.to_owned(),
            name: name.to_owned(),
            spec,
            policy: mcp_core::ServerRequestPolicy::default(),
            protocol: mcp_core::ProtocolMode::default(),
        }
    }

    #[test]
    fn reads_the_common_mcp_servers_shape() {
        let config = read_client_config(
            r#"{"mcpServers": {
                "files": {"command": "srv", "args": ["--verbose", "filesystem-server", "/tmp"],
                          "env": {"LOG": "debug"}, "cwd": "/work"}
            }}"#,
        )
        .unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers[0].name, "files");
        assert_eq!(
            config.servers[0].spec,
            ServerSpec::Stdio {
                command: "srv".into(),
                args: vec![
                    "--verbose".into(),
                    "filesystem-server".into(),
                    "/tmp".into()
                ],
                env: BTreeMap::from([("LOG".to_owned(), "debug".to_owned())]),
                cwd: Some("/work".into()),
            }
        );
        assert!(config.notes.is_empty());
    }

    #[test]
    fn reads_the_key_vs_code_writes_and_a_bare_object() {
        let vs_code = read_client_config(r#"{"servers": {"a": {"command": "x"}}}"#).unwrap();
        assert_eq!(vs_code.servers.len(), 1);
        let bare = read_client_config(r#"{"a": {"command": "x"}}"#).unwrap();
        assert_eq!(bare.servers, vs_code.servers);
    }

    #[test]
    fn a_real_token_is_lifted_out_of_the_headers() {
        let config = read_client_config(
            r#"{"mcpServers": {"remote": {"url": "https://x.test/mcp",
                "headers": {"Authorization": "Bearer s3cret", "X-Tenant": "acme"}}}}"#,
        )
        .unwrap();
        let server = &config.servers[0];
        assert_eq!(server.token.as_deref(), Some("s3cret"));
        let ServerSpec::Http { headers, auth, .. } = &server.spec else {
            panic!("http");
        };
        assert!(
            !headers.contains_key("Authorization"),
            "the token does not stay in the headers"
        );
        assert_eq!(headers["X-Tenant"], "acme");
        assert!(matches!(auth, AuthRef::Bearer { .. }));
        assert!(config.notes.is_empty());
    }

    #[test]
    fn our_own_export_reads_back_without_inventing_a_secret() {
        let exported = client_config(&[record(
            "remote",
            ServerSpec::Http {
                url: "https://x.test/mcp".into(),
                headers: BTreeMap::new(),
                auth: AuthRef::Bearer {
                    keyring_id: "remote".into(),
                },
            },
        )]);
        let config = read_client_config(&exported.text()).unwrap();
        let server = &config.servers[0];
        assert_eq!(server.name, "remote");
        assert!(matches!(server.spec, ServerSpec::Http { .. }));
        assert_eq!(server.token, None, "`<token>` is not a token");
        assert_eq!(config.notes.len(), 1, "{:?}", config.notes);
        assert!(config.notes[0].contains("needs its bearer token"));
    }

    #[test]
    fn an_sse_entry_is_imported_with_a_warning() {
        let config = read_client_config(
            r#"{"mcpServers": {"old": {"type": "sse", "url": "https://x.test/sse"}}}"#,
        )
        .unwrap();
        assert_eq!(config.servers.len(), 1);
        assert!(
            config.notes[0].contains("streamable HTTP"),
            "{:?}",
            config.notes
        );
    }

    #[test]
    fn unreadable_entries_are_named_not_dropped_silently() {
        let config =
            read_client_config(r#"{"mcpServers": {"good": {"command": "x"}, "bad": {"note": 1}}}"#)
                .unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.skipped.len(), 1);
        assert_eq!(config.skipped[0].0, "bad");
        assert!(
            config.skipped[0].1.contains("no `command`"),
            "{:?}",
            config.skipped
        );
        assert!(read_client_config("not json").is_err());
        assert!(read_client_config(r#"{"mcpServers": []}"#).is_err());
        assert!(read_client_config(r#"{"mcpServers": {}}"#).is_err());
    }

    #[test]
    fn a_bearer_token_never_reaches_the_config() {
        let servers = [record(
            "remote",
            ServerSpec::Http {
                url: "https://x.test/mcp".into(),
                headers: BTreeMap::new(),
                auth: AuthRef::Bearer {
                    keyring_id: "remote".into(),
                },
            },
        )];
        let config = client_config(&servers);
        assert_eq!(
            config.value["mcpServers"]["remote"]["headers"]["Authorization"],
            "Bearer <token>"
        );
        assert_eq!(config.notes.len(), 1);
        assert!(config.text().contains("mcpServers"));
    }

    #[test]
    fn a_stdio_server_keeps_its_command_and_environment() {
        let servers = [record(
            "local",
            ServerSpec::Stdio {
                command: "srv".into(),
                args: vec!["--verbose".into(), "srv".into()],
                env: BTreeMap::from([("LOG".to_owned(), "debug".to_owned())]),
                cwd: None,
            },
        )];
        let config = client_config(&servers);
        assert_eq!(config.value["mcpServers"]["local"]["command"], "srv");
        assert_eq!(config.value["mcpServers"]["local"]["args"][1], "srv");
        assert_eq!(config.value["mcpServers"]["local"]["env"]["LOG"], "debug");
        assert!(config.notes.is_empty());
    }
}
