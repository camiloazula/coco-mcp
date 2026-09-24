//! `import-config`: the servers a client configuration names, saved
//! together with the tokens the file carried.
//!
//! The keyring has no transactions, so the order is: write the secrets,
//! insert the rows in one transaction, and, if that fails, delete the
//! secrets written for it. Either every server of the file lands with its
//! credential or none does, and nothing is left in the keyring that no row
//! refers to.

use mcp_auth::SecretStore;
use mcp_core::{AuthRef, ProtocolMode, ServerRequestPolicy, ServerSpec};
use mcp_exchange::{ImportedConfig, ImportedServer};
use mcp_store::{NewServer, ServerRecord, Store};

use crate::style;

/// Save the servers of `config` that are not saved yet, all of them or
/// none, and return their names. A name already in the store is skipped
/// with a warning, as are the entries the file could not be read for.
pub(crate) fn import(
    store: &Store,
    secrets: &dyn SecretStore,
    config: ImportedConfig,
) -> Result<Vec<String>, String> {
    for (name, why) in &config.skipped {
        style::warn(format!("`{name}` skipped: {why}"));
    }
    for note in &config.notes {
        style::warn(note);
    }
    let mut fresh = Vec::with_capacity(config.servers.len());
    for server in config.servers {
        let existing = store
            .find_server_by_name(&server.name)
            .map_err(|e| e.to_string())?;
        if existing.is_some() {
            style::warn(format!("`{}` is already saved", server.name));
            continue;
        }
        fresh.push(server);
    }
    let mut written = Vec::new();
    match save(store, secrets, &fresh, &mut written) {
        Ok(records) => {
            for record in &records {
                style::status("Imported", &record.name);
            }
            Ok(records.into_iter().map(|r| r.name).collect())
        }
        Err(e) => {
            // No row was saved, so the entries written for them go too.
            for key in &written {
                if let Err(e) = secrets.delete(key) {
                    style::warn(format!("keyring entry `{key}` could not be removed: {e}"));
                }
            }
            Err(e)
        }
    }
}

/// Write each server's token under a fresh keyring entry, noting every
/// entry written in `written`, then save the rows in one transaction.
fn save(
    store: &Store,
    secrets: &dyn SecretStore,
    servers: &[ImportedServer],
    written: &mut Vec<String>,
) -> Result<Vec<ServerRecord>, String> {
    let mut specs = Vec::with_capacity(servers.len());
    for server in servers {
        let mut spec = server.spec.clone();
        if let ServerSpec::Http { auth, .. } = &mut spec
            && let AuthRef::Bearer { keyring_id } | AuthRef::OAuth { keyring_id } = auth
        {
            *keyring_id = mcp_auth::new_keyring_id();
            match &server.token {
                Some(token) => {
                    secrets
                        .set(keyring_id, token)
                        .map_err(|e| format!("keyring: {e}"))?;
                    written.push(keyring_id.clone());
                }
                // A placeholder leaves the entry empty, to be filled in the app.
                None => style::warn(format!("`{}` still needs its bearer token", server.name)),
            }
        }
        specs.push(spec);
    }
    let policy = ServerRequestPolicy::default();
    let batch: Vec<NewServer<'_>> = servers
        .iter()
        .zip(&specs)
        .map(|(server, spec)| NewServer {
            name: &server.name,
            spec,
            policy: &policy,
            protocol: ProtocolMode::Legacy,
        })
        .collect();
    store.add_servers(&batch).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_auth::MemoryStore;

    fn http(name: &str, token: Option<&str>) -> ImportedServer {
        ImportedServer {
            name: name.into(),
            spec: ServerSpec::Http {
                url: format!("https://{name}.test/mcp"),
                headers: Default::default(),
                auth: AuthRef::Bearer {
                    keyring_id: "from-the-file".into(),
                },
            },
            token: token.map(str::to_owned),
        }
    }

    fn stdio(name: &str, env: &[(&str, &str)]) -> ImportedServer {
        ImportedServer {
            name: name.into(),
            spec: ServerSpec::Stdio {
                command: "srv".into(),
                args: vec![],
                env: env
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                cwd: None,
            },
            token: None,
        }
    }

    fn config(servers: Vec<ImportedServer>) -> ImportedConfig {
        ImportedConfig {
            servers,
            ..Default::default()
        }
    }

    #[test]
    fn a_file_lands_whole_with_its_tokens() {
        let store = Store::open_in_memory().unwrap();
        let secrets = MemoryStore::new();
        let added = import(
            &store,
            &secrets,
            config(vec![http("remote", Some("s3cret")), stdio("files", &[])]),
        )
        .unwrap();
        assert_eq!(added, ["remote", "files"]);
        let saved = store.find_server_by_name("remote").unwrap().unwrap();
        let ServerSpec::Http {
            auth: AuthRef::Bearer { keyring_id },
            ..
        } = &saved.spec
        else {
            panic!("{:?}", saved.spec);
        };
        assert_ne!(keyring_id, "from-the-file", "a fresh entry");
        assert_eq!(secrets.get(keyring_id).unwrap().as_deref(), Some("s3cret"));
        assert_eq!(secrets.len(), 1);
    }

    #[test]
    fn a_refused_entry_saves_nothing_and_takes_its_tokens_back() {
        let store = Store::open_in_memory().unwrap();
        let secrets = MemoryStore::new();
        // The environment variable looks like a secret: the store refuses
        // the row, and with it the file.
        let err = import(
            &store,
            &secrets,
            config(vec![
                http("remote", Some("s3cret")),
                stdio("leaky", &[("API_TOKEN", "x")]),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("refusing to store a secret"), "{err}");
        assert!(store.list_servers().unwrap().is_empty(), "no row landed");
        assert!(secrets.is_empty(), "the token written for `remote` is gone");
    }

    #[test]
    fn a_saved_name_is_left_alone_and_the_rest_land() {
        let store = Store::open_in_memory().unwrap();
        let secrets = MemoryStore::new();
        store
            .add_server(
                "files",
                &stdio("files", &[]).spec,
                &ServerRequestPolicy::default(),
            )
            .unwrap();
        let added = import(
            &store,
            &secrets,
            config(vec![stdio("files", &[]), http("remote", None)]),
        )
        .unwrap();
        assert_eq!(added, ["remote"]);
        assert_eq!(store.list_servers().unwrap().len(), 2);
        assert!(secrets.is_empty(), "a placeholder writes no entry");
    }
}
