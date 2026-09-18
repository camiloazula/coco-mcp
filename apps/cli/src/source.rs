//! Parsing of the `<source>` argument.
//!
//! * `stdio:<command> [args...]`, split the way a shell splits a command line
//!   (`mcp_core::split_command_line`: quotes group, a backslash escapes,
//!   nothing is expanded), so the label `coco` prints for a stdio server
//!   reads back as the same server
//! * the bare word `stdio` with the command after `--`, whose words are taken
//!   as they are: `coco snapshot stdio -- some-server --verbose`
//! * `http://…` / `https://…`
//! * a path to a snapshot JSON file (produced by `coco snapshot --out`)
//! * `saved:<name>` — the latest snapshot stored for that server with `--db`;
//!   `saved:<name>~1` is the one before it, and so on

use std::collections::BTreeMap;
use std::path::PathBuf;

use mcp_core::{AuthRef, ServerSpec, Snapshot};

/// Where a command reads its data from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// A live server.
    Server(ServerSpec),
    /// A snapshot saved earlier.
    File(PathBuf),
    /// A snapshot stored in the database: `back` steps before the latest.
    Saved {
        /// Server name as saved.
        name: String,
        /// 0 = latest, 1 = the one before, …
        back: usize,
    },
}

/// Parse `source`, appending `trailing` (the words after `--`) to a stdio
/// command and `headers` (`Name: value` or `Name=value`) to an HTTP spec.
pub fn parse(source: &str, trailing: &[String], headers: &[String]) -> Result<Source, String> {
    if let Some(rest) = source.strip_prefix("stdio:") {
        let words = mcp_core::split_command_line(rest).map_err(|e| format!("`{source}`: {e}"))?;
        return stdio(words, trailing);
    }
    if source == "stdio" {
        return stdio(Vec::new(), trailing);
    }
    if let Some(rest) = source.strip_prefix("saved:") {
        let (name, back) = match rest.rsplit_once('~') {
            Some((name, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => (
                name,
                n.parse::<usize>()
                    .map_err(|e| format!("`{source}`: bad offset: {e}"))?,
            ),
            _ => (rest, 0),
        };
        if name.is_empty() {
            return Err(format!("`{source}`: saved source needs a server name"));
        }
        return Ok(Source::Saved {
            name: name.to_owned(),
            back,
        });
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        let mut map = BTreeMap::new();
        for header in headers {
            let (name, value) = header
                .split_once(':')
                .or_else(|| header.split_once('='))
                .ok_or_else(|| format!("header `{header}` must be `Name: value`"))?;
            map.insert(name.trim().to_owned(), value.trim().to_owned());
        }
        return Ok(Source::Server(ServerSpec::Http {
            url: source.to_owned(),
            headers: map,
            auth: AuthRef::None,
        }));
    }
    let path = PathBuf::from(source);
    if path.is_file() {
        return Ok(Source::File(path));
    }
    Err(format!(
        "`{source}` is not a source: use `stdio:<command>`, `stdio -- <command> [args]`, an http(s) URL, a snapshot file, or `saved:<name>`"
    ))
}

fn stdio(mut words: Vec<String>, trailing: &[String]) -> Result<Source, String> {
    words.extend(trailing.iter().cloned());
    let mut iter = words.into_iter();
    let command = iter
        .next()
        .ok_or_else(|| "stdio source needs a command".to_string())?;
    Ok(Source::Server(ServerSpec::Stdio {
        command,
        args: iter.collect(),
        env: BTreeMap::new(),
        cwd: None,
    }))
}

/// Load a snapshot file.
pub fn load_snapshot(path: &std::path::Path) -> Result<Snapshot, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: not a snapshot: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_forms() {
        let s = parse("stdio:srv --verbose one", &[], &[]).unwrap();
        assert_eq!(
            s,
            Source::Server(ServerSpec::Stdio {
                command: "srv".into(),
                args: vec!["--verbose".into(), "one".into()],
                env: BTreeMap::new(),
                cwd: None,
            })
        );
        let s = parse("stdio", &["/bin/with space".into(), "x".into()], &[]).unwrap();
        match s {
            Source::Server(ServerSpec::Stdio { command, args, .. }) => {
                assert_eq!(command, "/bin/with space");
                assert_eq!(args, ["x"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse("stdio:", &[], &[]).is_err());
        assert!(parse("stdio", &[], &[]).is_err());
    }

    #[test]
    fn a_stdio_command_line_is_split_like_a_shell_splits_it() {
        let words = |source: &str| match parse(source, &[], &[]).unwrap() {
            Source::Server(ServerSpec::Stdio { command, args, .. }) => {
                std::iter::once(command).chain(args).collect::<Vec<_>>()
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(
            words(r#"stdio:"/bin/with space" 'a b' c\ d --port=3000"#),
            ["/bin/with space", "a b", "c d", "--port=3000"]
        );
        let spec = ServerSpec::Stdio {
            command: "srv".into(),
            args: vec!["--port=3000".into(), "it's".into()],
            env: BTreeMap::new(),
            cwd: None,
        };
        assert_eq!(
            parse(&format!("stdio:{}", spec.label()), &[], &[]).unwrap(),
            Source::Server(spec),
            "a printed label is the same server"
        );
        let err = parse("stdio:srv 'unclosed", &[], &[]).unwrap_err();
        assert!(err.contains("unclosed quote"), "{err}");
    }

    #[test]
    fn parses_http_with_headers() {
        let s = parse(
            "https://x.test/mcp",
            &[],
            &["X-A: 1".into(), "X-B=2".into()],
        )
        .unwrap();
        match s {
            Source::Server(ServerSpec::Http { url, headers, auth }) => {
                assert_eq!(url, "https://x.test/mcp");
                assert_eq!(headers["X-A"], "1");
                assert_eq!(headers["X-B"], "2");
                assert_eq!(auth, AuthRef::None);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse("https://x.test/mcp", &[], &["bad".into()]).is_err());
    }

    #[test]
    fn parses_saved() {
        assert_eq!(
            parse("saved:demo", &[], &[]).unwrap(),
            Source::Saved {
                name: "demo".into(),
                back: 0
            }
        );
        assert_eq!(
            parse("saved:my~server~2", &[], &[]).unwrap(),
            Source::Saved {
                name: "my~server".into(),
                back: 2
            }
        );
        assert_eq!(
            parse("saved:odd~", &[], &[]).unwrap(),
            Source::Saved {
                name: "odd~".into(),
                back: 0
            }
        );
        assert!(parse("saved:", &[], &[]).is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("ftp://x", &[], &[]).is_err());
        assert!(parse("/definitely/missing.json", &[], &[]).is_err());
    }
}
