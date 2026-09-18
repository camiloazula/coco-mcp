//! The names `coco --db` saves live servers under, and the way back from a
//! name to its server.
//!
//! A live server is saved under its label, the command line the `Connecting`
//! line prints, with each word that needs it quoted (`srv '--port=3000'`).
//! Nobody should have to type that quoting back, so a name given to
//! `saved:`, `history --server` or `export-config --server` that no server
//! is saved under is read as a command line: any spelling a shell splits
//! into the same words finds the server.

use mcp_core::{ProtocolMode, ServerRequestPolicy, ServerSpec};
use mcp_store::{ServerRecord, Store};

/// The saved server a live source is recorded under, if there is one: the
/// server saved under its label. A stdio label quotes the arguments that
/// need it, but a database written before it did holds such a server under
/// its words joined by spaces; that row is kept, so its history and its
/// `saved:` name carry on, as long as it was saved for the same words (the
/// joined name cannot tell `srv 'a b'` from `srv a b`).
pub fn existing_for(store: &Store, spec: &ServerSpec) -> mcp_store::Result<Option<ServerRecord>> {
    let label = spec.label();
    if let Some(saved) = store.find_server_by_name(&label)? {
        return Ok(Some(saved));
    }
    if let ServerSpec::Stdio { command, args, .. } = spec {
        let joined = std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        if joined != label
            && let Some(existing) = store.find_server_by_name(&joined)?
            && matches!(
                &existing.spec,
                ServerSpec::Stdio { command: c, args: a, .. } if c == command && a == args
            )
        {
            return Ok(Some(existing));
        }
    }
    Ok(None)
}

/// [`existing_for`], or a new server saved under the label, connected in
/// `protocol`.
pub fn server_for(
    store: &Store,
    spec: &ServerSpec,
    protocol: ProtocolMode,
) -> mcp_store::Result<ServerRecord> {
    match existing_for(store, spec)? {
        Some(saved) => Ok(saved),
        None => store.add_server_with(
            &spec.label(),
            spec,
            &ServerRequestPolicy::default(),
            protocol,
        ),
    }
}

/// The saved server `name` names: the one saved under exactly that name, or
/// else the stdio server whose command and arguments are the words of `name`
/// read as a command line. Of several such servers, the one saved under the
/// label of those words is the one [`server_for`] records under; any other
/// tie is refused rather than guessed.
pub fn find(store: &Store, name: &str) -> Result<ServerRecord, String> {
    if let Some(server) = store.find_server_by_name(name).map_err(|e| e.to_string())? {
        return Ok(server);
    }
    let missing = || format!("no server named `{name}`");
    let words = mcp_core::split_command_line(name).map_err(|_| missing())?;
    let Some((command, args)) = words.split_first() else {
        return Err(missing());
    };
    let mut runs: Vec<ServerRecord> = store
        .list_servers()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|server| {
            matches!(
                &server.spec,
                ServerSpec::Stdio { command: c, args: a, .. } if c == command && a == args
            )
        })
        .collect();
    let label = mcp_core::command_line(words.iter().map(String::as_str));
    if let Some(at) = runs.iter().position(|server| server.name == label) {
        return Ok(runs.swap_remove(at));
    }
    match runs.len() {
        0 => Err(missing()),
        1 => Ok(runs.swap_remove(0)),
        n => Err(format!(
            "`{name}` is the command line of {n} saved servers ({}); use one of their names",
            runs.iter()
                .map(|server| format!("`{}`", server.name))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(args: &[&str]) -> ServerSpec {
        ServerSpec::Stdio {
            command: "srv".into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            env: Default::default(),
            cwd: None,
        }
    }

    const LEGACY: ProtocolMode = ProtocolMode::Legacy;

    #[test]
    fn a_server_saved_under_its_words_joined_by_spaces_is_kept() {
        let store = Store::open_in_memory().unwrap();
        let policy = ServerRequestPolicy::default();
        let port = stdio(&["--port=3000"]);
        assert_eq!(port.label(), "srv '--port=3000'");
        let kept = store.add_server("srv --port=3000", &port, &policy).unwrap();
        assert_eq!(server_for(&store, &port, LEGACY).unwrap(), kept);

        let split = store
            .add_server("srv a b", &stdio(&["a", "b"]), &policy)
            .unwrap();
        let spaced = server_for(&store, &stdio(&["a b"]), LEGACY).unwrap();
        assert_ne!(spaced.id, split.id, "other words, other server");
        assert_eq!(spaced.name, "srv 'a b'");
        assert_eq!(
            server_for(&store, &stdio(&["a b"]), LEGACY).unwrap(),
            spaced
        );
        assert_eq!(
            server_for(&store, &stdio(&["a", "b"]), LEGACY).unwrap(),
            split
        );
        assert_eq!(store.list_servers().unwrap().len(), 3);
    }

    #[test]
    fn a_server_is_found_by_any_spelling_of_its_command_line() {
        let store = Store::open_in_memory().unwrap();
        let saved = server_for(&store, &stdio(&["--port=3000"]), LEGACY).unwrap();
        assert_eq!(saved.name, "srv '--port=3000'");
        for name in [
            "srv '--port=3000'",
            "srv --port=3000",
            r#"srv "--port=3000""#,
            r"srv --port\=3000",
        ] {
            assert_eq!(find(&store, name).unwrap(), saved, "{name}");
        }
        for name in ["srv --port=3001", "srv", "srv 'unclosed", ""] {
            let err = find(&store, name).unwrap_err();
            assert_eq!(err, format!("no server named `{name}`"));
        }
    }

    #[test]
    fn of_several_servers_for_one_command_line_only_the_label_is_chosen() {
        let store = Store::open_in_memory().unwrap();
        let policy = ServerRequestPolicy::default();
        let spec = stdio(&["a b"]);
        let one = store.add_server("one", &spec, &policy).unwrap();
        assert_eq!(find(&store, r"srv a\ b").unwrap(), one);

        store.add_server("two", &spec, &policy).unwrap();
        let tie = find(&store, r"srv a\ b").unwrap_err();
        assert!(tie.contains("`one`, `two`"), "{tie}");

        let recorded = server_for(&store, &spec, LEGACY).unwrap();
        assert_eq!(recorded.name, "srv 'a b'");
        assert_eq!(find(&store, r"srv a\ b").unwrap(), recorded);
        assert_eq!(find(&store, "one").unwrap(), one);
    }

    #[test]
    fn a_new_server_is_saved_in_its_mode_and_an_existing_one_keeps_its_own() {
        let store = Store::open_in_memory().unwrap();
        let spec = stdio(&["modern"]);
        assert!(existing_for(&store, &spec).unwrap().is_none());
        let saved = server_for(&store, &spec, ProtocolMode::Modern).unwrap();
        assert_eq!(saved.protocol, ProtocolMode::Modern);
        assert_eq!(existing_for(&store, &spec).unwrap(), Some(saved.clone()));
        assert_eq!(server_for(&store, &spec, LEGACY).unwrap(), saved);
    }
}
