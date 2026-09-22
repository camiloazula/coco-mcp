//! End-to-end tests of the command-line mode: the real `coco-mcp` binary,
//! `--cli` first, against the standalone mock server binary (a
//! dev-dependency, not something the CLI itself knows about).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

fn coco() -> &'static str {
    env!("CARGO_BIN_EXE_coco-mcp")
}

/// `coco-mcp --cli`, ready for the command's own words.
fn cli() -> Command {
    let mut command = Command::new(coco());
    command.arg("--cli");
    command
}

/// The standalone mock server binary: a dev-dependency of these tests only,
/// the CLI itself carries no knowledge of it. Cargo does not export
/// `CARGO_BIN_EXE_mcp-mockserver` across the package boundary, so it is
/// located next to `coco-mcp` in the same target directory; `just test-app`
/// (and CI) build the mock server first.
fn mockserver() -> PathBuf {
    // Same directory and, on every platform, the same file extension as
    // `coco-mcp` itself.
    let coco = PathBuf::from(coco());
    let mut path = coco.with_file_name("mcp-mockserver");
    if let Some(extension) = coco.extension() {
        path.set_extension(extension);
    }
    assert!(
        path.is_file(),
        "{} not built; run `cargo build -p mcp-mockserver` or `just test`",
        path.display()
    );
    path
}

/// `coco-mcp --cli <args...> -- <mcp-mockserver> --schema <v>`
fn run(args: &[&str], schema: &str) -> Output {
    run_with(args, schema, &[])
}

/// [`run`] with more mock server flags after the schema, e.g. its faults.
fn run_with(args: &[&str], schema: &str, mock_flags: &[&str]) -> Output {
    cli()
        .args(args)
        .args(["--"])
        .arg(mockserver())
        .args(["--schema", schema])
        .args(mock_flags)
        .output()
        .expect("run coco")
}

fn stdout_json(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {text}"))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("coco-test-{}-{name}", std::process::id()))
}

#[test]
fn snapshot_prints_tools_resources_prompts() {
    let out = run(&["snapshot", "stdio"], "v1");
    assert!(out.status.success(), "{}", stderr(&out));
    let snap = stdout_json(&out);
    assert_eq!(snap["serverInfo"]["name"], "mcp-mockserver");
    let tools: Vec<&str> = snap["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(tools.contains(&"echo"));
    assert_eq!(snap["resources"].as_array().unwrap().len(), 8);
    assert_eq!(snap["prompts"].as_array().unwrap().len(), 3);
    assert_eq!(snap["digest"].as_str().unwrap().len(), 64);
}

#[test]
fn snapshot_connects_in_each_protocol_mode() {
    for (mode, flags, version) in [
        ("legacy", &[][..], "2025-11-25"),
        ("modern", &[], "2026-07-28"),
        ("auto", &[], "2026-07-28"),
        ("auto", &["--legacy-only"], "2025-11-25"),
        ("auto", &["--legacy-versions"], "2025-11-25"),
    ] {
        let out = run_with(&["snapshot", "--protocol", mode, "stdio"], "v1", flags);
        assert!(out.status.success(), "{mode} {flags:?}: {}", stderr(&out));
        let snap = stdout_json(&out);
        assert_eq!(snap["protocolVersion"], version, "{mode} {flags:?}");
        assert!(
            snap["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "echo")
        );
    }
    let out = run_with(
        &["snapshot", "--protocol", "modern", "stdio"],
        "v1",
        &["--legacy-only"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("Legacy or Auto"), "{}", stderr(&out));
}

#[test]
fn a_modern_round_trip_is_refused_the_way_a_server_request_is() {
    let out = run(
        &[
            "call",
            "--protocol",
            "modern",
            "stdio",
            "elicit",
            "--args",
            r#"{"question": "anyone?"}"#,
        ],
        "v1",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout_json(&out)["content"][0]["text"], "declined");
}

#[test]
fn snapshot_out_file_round_trips_as_a_source() {
    let file = temp_path("snap.json");
    let out = run(
        &["snapshot", "stdio", "--out", file.to_str().unwrap()],
        "v2",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    // Cargo-style status line: a right-aligned verb, then the detail.
    assert!(
        stderr(&out).contains("Wrote snapshot of"),
        "{}",
        stderr(&out)
    );

    let again = cli()
        .args(["--compact", "snapshot", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(again.status.success(), "{}", stderr(&again));
    let text = String::from_utf8_lossy(&again.stdout);
    assert_eq!(text.lines().count(), 1, "compact output is one line");
    let snap: Value = serde_json::from_str(&text).unwrap();
    assert!(
        snap["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "multiply")
    );
    let _ = std::fs::remove_file(file);
}

#[test]
fn call_prints_result_with_elapsed() {
    let out = run(
        &["call", "stdio", "add", "--args", r#"{"a": 2, "b": 3}"#],
        "v1",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let result = stdout_json(&out);
    assert_eq!(result["structuredContent"]["value"], 5.0);
    assert!(result["_elapsedMs"].is_u64());
}

#[test]
fn tool_error_exits_2_but_prints_result() {
    let out = run(
        &["call", "stdio", "fail", "--args", r#"{"message": "nope"}"#],
        "v1",
    );
    assert_eq!(out.status.code(), Some(2));
    let result = stdout_json(&out);
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], "nope");
    assert!(stderr(&out).contains("isError"));
}

#[test]
fn unknown_tool_and_bad_args_exit_2() {
    let out = run(&["call", "stdio", "nope"], "v1");
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));

    let out = run(&["call", "stdio", "echo", "--args", "[1]"], "v1");
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("JSON object"));
}

#[test]
fn read_and_prompt() {
    let out = run(&["read", "stdio", "mock://json/config"], "v1");
    assert!(out.status.success(), "{}", stderr(&out));
    let result = stdout_json(&out);
    assert_eq!(result["contents"][0]["mimeType"], "application/json");

    let out = run(
        &["prompt", "stdio", "greet", "--args", r#"{"name": "Ada"}"#],
        "v1",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let result = stdout_json(&out);
    assert!(
        result["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("Ada")
    );
}

/// A list the server cannot deliver is a warning, not a failed command, and
/// the commands that never list are untouched by it.
#[test]
fn snapshot_warns_about_a_list_that_failed() {
    let faults = ["--no-templates", "--broken-resources"];
    let out = run_with(&["snapshot", "stdio"], "v1", &faults);
    assert!(out.status.success(), "{}", stderr(&out));
    let snap = stdout_json(&out);
    assert!(
        snap["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "echo")
    );
    assert_eq!(snap["listFailures"].as_array().unwrap().len(), 1);
    assert_eq!(snap["listFailures"][0]["method"], "resources/list");
    let err = stderr(&out);
    assert!(err.contains("warning"), "{err}");
    assert!(err.contains("resources/list"), "{err}");
    assert!(!err.contains("resources/templates/list"), "{err}");

    let out = run_with(
        &["call", "stdio", "echo", "--args", r#"{"text": "x"}"#],
        "v1",
        &faults,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout_json(&out)["content"][0]["text"], "x");
    let out = run_with(&["read", "stdio", "mock://text/hello"], "v1", &faults);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run_with(
        &["prompt", "stdio", "greet", "--args", r#"{"name": "Ada"}"#],
        "v1",
        &faults,
    );
    assert!(out.status.success(), "{}", stderr(&out));
}

/// A snapshot missing a list is printed but not stored with `--db`, the rule
/// the app follows too, so it never becomes the baseline a `saved:` diff is
/// measured against.
#[test]
fn a_snapshot_missing_a_list_is_not_stored() {
    let db = temp_path("partial.db");
    let db_arg = db.to_str().unwrap();
    // The fault comes from the environment so both runs share one command
    // line, and with it one saved server.
    let partial = cli()
        .env("MCP_MOCK_FAULTS", "broken-resources")
        .args(["--db", db_arg, "snapshot", "stdio", "--"])
        .arg(mockserver())
        .args(["--schema", "v1"])
        .output()
        .unwrap();
    assert!(partial.status.success(), "{}", stderr(&partial));
    let err = stderr(&partial);
    assert!(err.contains("snapshot not stored"), "{err}");
    assert!(err.contains("`resources/list` failed"), "{err}");
    assert_eq!(
        stdout_json(&partial)["listFailures"][0]["method"],
        "resources/list"
    );
    let store = mcp_store::Store::open(&db).unwrap();
    let servers = store.list_servers().unwrap();
    assert_eq!(servers.len(), 1);
    assert!(store.latest_snapshot(&servers[0].id).unwrap().is_none());

    let complete = run(&["--db", db_arg, "snapshot", "stdio"], "v1");
    assert!(complete.status.success(), "{}", stderr(&complete));
    assert!(
        !stderr(&complete).contains("not stored"),
        "{}",
        stderr(&complete)
    );
    let servers = store.list_servers().unwrap();
    assert_eq!(servers.len(), 1, "both runs saved under one server");
    let stored = store.list_snapshots(&servers[0].id, 10).unwrap();
    assert_eq!(stored.len(), 1);
    let latest = store.get_snapshot(&stored[0].id).unwrap().unwrap().snapshot;
    assert_eq!(latest.resources.len(), 8);
    assert!(latest.list_failures.is_empty());
    let _ = std::fs::remove_file(&db);
}

/// A list one side could not read is not compared, whichever kind of source
/// that side is, and the diff does not exit 0 as if it had been.
#[test]
fn diff_exits_2_when_a_list_could_not_be_compared() {
    // A `stdio:` source is read like a shell command line, so a path with a
    // backslash in it is quoted.
    let command = mcp_core::command_line([mockserver().to_str().unwrap()]);
    let live = |schema: &str, flags: &str| format!("stdio:{command} --schema {schema}{flags}");
    let out = cli()
        .args(["diff", &live("v1", ""), &live("v1", " --broken-resources")])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let json = stdout_json(&out);
    assert_eq!(json["outcome"], "unchanged", "{json}");
    assert_eq!(json["skipped"], serde_json::json!(["resources/list"]));
    let err = stderr(&out);
    assert!(
        err.contains("--broken-resources: `resources/list` failed"),
        "{err}"
    );
    assert!(err.contains("resources/list not compared"), "{err}");

    // Snapshot files carry their failures, so the same holds for them.
    let complete = temp_path("complete.json");
    let partial = temp_path("partial.json");
    let out = run(
        &["snapshot", "stdio", "--out", complete.to_str().unwrap()],
        "v1",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run_with(
        &["snapshot", "stdio", "--out", partial.to_str().unwrap()],
        "v1",
        &["--broken-resources"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let out = cli()
        .args([
            "diff",
            partial.to_str().unwrap(),
            complete.to_str().unwrap(),
            "--text",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(
        listing.starts_with("unchanged · resources/list not compared"),
        "{listing}"
    );
    let err = stderr(&out);
    assert!(
        err.contains(&format!("{}: `resources/list` failed", partial.display())),
        "{err}"
    );

    // A breaking change among the lists that were compared still exits 1.
    let out = cli()
        .args(["diff", &live("v2", ""), &live("v1", " --broken-resources")])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stdout_json(&out)["skipped"],
        serde_json::json!(["resources/list"])
    );
    for f in [&complete, &partial] {
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn missing_server_binary_exits_2() {
    let out = cli()
        .args(["snapshot", "stdio:/no/such/binary"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = stderr(&out);
    assert!(text.contains("error:"), "{text}");
    assert!(!text.contains('\x1b'), "piped stderr carries no ANSI codes");
}

#[test]
fn events_flag_streams_wire_log_to_stderr() {
    let out = run(
        &[
            "--events",
            "call",
            "stdio",
            "echo",
            "--args",
            r#"{"text": "x"}"#,
        ],
        "v1",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("[wire] → tools/call"), "{err}");
    assert!(err.contains("[state]"), "{err}");
}

/// `--events` listens to the session, so a policy that asks would wait for
/// an answer nobody can give; the CLI refuses at once instead. The request
/// timeout is shorter than the policy's, so a wait would fail the call.
#[test]
fn server_requests_are_refused_without_a_dialog() {
    let call = |tool: &str, args: &str| {
        let out = run(
            &[
                "--events",
                "--timeout",
                "10",
                "call",
                "stdio",
                tool,
                "--args",
                args,
            ],
            "v1",
        );
        assert!(out.status.success(), "{tool}: {}", stderr(&out));
        stdout_json(&out)["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let sample = call("sample", r#"{"prompt": "p"}"#);
    assert!(sample.contains("sampling error"), "{sample}");
    assert_eq!(call("elicit", r#"{"question": "?"}"#), "declined");
    assert_eq!(call("roots", "{}"), r#"{"roots":[]}"#);
}

/// Spawn the mock over HTTP with a bearer token, then hit it with --bearer-env.
#[test]
fn http_source_with_bearer_env() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut server = Command::new(mockserver())
        .args(["--http", "127.0.0.1:0", "--bearer", "hunter2"])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    // Read the `listening on <url>` line on a helper thread with a
    // deadline, so a changed message fails the test instead of hanging it.
    let child_err = server.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(child_err).lines() {
            let Ok(line) = line else { break };
            if let Some(rest) = line
                .trim_start()
                .strip_prefix("mcp-mockserver listening on ")
            {
                let _ = tx.send(rest.trim().to_owned());
                break;
            }
            let _ = tx.send(format!("(ignored) {line}"));
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut seen = Vec::new();
    let url = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) if line.starts_with("http://") => break line,
            Ok(other) => seen.push(other),
            Err(_) => {
                let _ = server.kill();
                panic!("mock server never announced its URL; stderr so far: {seen:?}");
            }
        }
    };
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");

    let denied = cli().args(["snapshot", &url]).output().unwrap();
    assert_eq!(denied.status.code(), Some(2));
    assert!(
        stderr(&denied).contains("authorization required"),
        "{}",
        stderr(&denied)
    );

    let ok = cli()
        .args([
            "call",
            &url,
            "echo",
            "--args",
            r#"{"text": "remote"}"#,
            "--bearer-env",
            "COCO_TEST_TOKEN",
        ])
        .env("COCO_TEST_TOKEN", "hunter2")
        .output()
        .unwrap();
    assert!(ok.status.success(), "{}", stderr(&ok));
    assert_eq!(stdout_json(&ok)["content"][0]["text"], "remote");

    let unset = cli()
        .args(["snapshot", &url, "--bearer-env", "COCO_MISSING_VAR"])
        .env_remove("COCO_MISSING_VAR")
        .output()
        .unwrap();
    assert_eq!(unset.status.code(), Some(2));
    assert!(stderr(&unset).contains("COCO_MISSING_VAR"));
    let _ = server.kill();
    let _ = server.wait();
}

#[test]
fn db_records_snapshots_and_calls() {
    let db = temp_path("history.db");
    let db_arg = db.to_str().unwrap();
    let out = run(&["--db", db_arg, "snapshot", "stdio"], "v1");
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(
        &[
            "--db",
            db_arg,
            "call",
            "stdio",
            "echo",
            "--args",
            r#"{"text": "kept"}"#,
        ],
        "v1",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(&["--db", db_arg, "call", "stdio", "nope"], "v1");
    assert_eq!(out.status.code(), Some(2));

    let history = cli().args(["--db", db_arg, "history"]).output().unwrap();
    assert!(history.status.success(), "{}", stderr(&history));
    let rows = stdout_json(&history);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["status"], "failed");
    assert_eq!(rows[1]["status"], "ok");
    assert_eq!(rows[1]["args"]["text"], "kept");

    let store = mcp_store::Store::open(&db).unwrap();
    let servers = store.list_servers().unwrap();
    assert_eq!(servers.len(), 1, "same source label reuses the server row");
    assert!(store.latest_snapshot(&servers[0].id).unwrap().is_some());

    let missing = cli().args(["history"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("--db"));
    let _ = std::fs::remove_file(&db);
}

/// A server saved with a protocol mode connects in it, unless `--protocol`
/// says otherwise.
#[test]
fn a_saved_server_connects_in_its_saved_mode_unless_told_otherwise() {
    let db = temp_path("mode.db");
    let db_arg = db.to_str().unwrap();
    let mock = mockserver();
    let spec = mcp_core::ServerSpec::Stdio {
        command: mock.to_str().unwrap().to_owned(),
        args: vec!["--schema".into(), "v1".into()],
        env: Default::default(),
        cwd: None,
    };
    let store = mcp_store::Store::open(&db).unwrap();
    store
        .add_server_with(
            &spec.label(),
            &spec,
            &mcp_core::ServerRequestPolicy::default(),
            mcp_core::ProtocolMode::Modern,
        )
        .unwrap();
    let snapshot = |flags: &[&str]| {
        cli()
            .args(["--db", db_arg, "snapshot"])
            .args(flags)
            .args(["stdio", "--"])
            .arg(&mock)
            .args(["--schema", "v1"])
            .output()
            .unwrap()
    };
    let saved = snapshot(&[]);
    assert!(saved.status.success(), "{}", stderr(&saved));
    assert_eq!(stdout_json(&saved)["protocolVersion"], "2026-07-28");
    let told = snapshot(&["--protocol", "legacy"]);
    assert!(told.status.success(), "{}", stderr(&told));
    assert_eq!(stdout_json(&told)["protocolVersion"], "2025-11-25");
    assert_eq!(store.list_servers().unwrap().len(), 1);
    let _ = std::fs::remove_file(&db);
}

/// A database that saved a server under its words joined by spaces keeps
/// using that row, although the label now quotes an argument with `=`: a
/// second server would cut the old history and `saved:` name off.
#[test]
fn db_keeps_a_server_saved_under_its_unquoted_command_line() {
    let db = temp_path("unquoted.db");
    let db_arg = db.to_str().unwrap();
    let mock = mockserver();
    let command = mock.to_str().unwrap().to_owned();
    let spec = mcp_core::ServerSpec::Stdio {
        command: command.clone(),
        args: vec!["--schema=v1".into()],
        env: Default::default(),
        cwd: None,
    };
    let name = format!("{command} --schema=v1");
    assert_ne!(spec.label(), name);
    let store = mcp_store::Store::open(&db).unwrap();
    let saved = store
        .add_server(&name, &spec, &mcp_core::ServerRequestPolicy::default())
        .unwrap();

    let out = cli()
        .args(["--db", db_arg, "snapshot", "stdio", "--"])
        .arg(&mock)
        .arg("--schema=v1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let servers = store.list_servers().unwrap();
    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].id, saved.id);
    assert_eq!(servers[0].name, name);
    assert!(store.latest_snapshot(&saved.id).unwrap().is_some());

    let same = cli()
        .args([
            "--db",
            db_arg,
            "diff",
            &format!("saved:{name}"),
            &format!("saved:{name}"),
        ])
        .output()
        .unwrap();
    assert_eq!(same.status.code(), Some(0), "{}", stderr(&same));
    let _ = std::fs::remove_file(&db);
}

/// A server whose argument has `=` in it is saved under the command line the
/// `Connecting` line prints, quoted; that line is a `stdio:` source for the
/// same server, and every command that takes a server name finds it by the
/// printed line or by the command line as it was typed.
#[test]
fn a_saved_server_is_found_by_any_spelling_of_its_command_line() {
    let db = temp_path("spelling.db");
    let db_arg = db.to_str().unwrap();
    let mock = mockserver();
    let command = mock.to_str().unwrap();
    let out = cli()
        .args(["--db", db_arg, "snapshot", "stdio", "--"])
        .arg(&mock)
        .arg("--schema=v1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let printed = mcp_core::command_line([command, "--schema=v1"]);
    assert!(stderr(&out).contains(&printed), "{}", stderr(&out));
    // As typed at a prompt: the path is quoted only when it must be.
    let typed = format!("{} --schema=v1", mcp_core::command_line([command]));
    assert_ne!(printed, typed);
    let store = mcp_store::Store::open(&db).unwrap();
    let servers = store.list_servers().unwrap();
    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].name, printed);

    let out = cli()
        .args([
            "--db",
            db_arg,
            "call",
            &format!("stdio:{printed}"),
            "echo",
            "--args",
            r#"{"text": "again"}"#,
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        store.list_servers().unwrap().len(),
        1,
        "the printed line is the same server"
    );

    for name in [&printed, &typed] {
        let source = format!("saved:{name}");
        let out = cli()
            .args(["--db", db_arg, "diff", &source, &source])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{source}: {}", stderr(&out));
        let out = cli()
            .args(["--db", db_arg, "history", "--server", name])
            .output()
            .unwrap();
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        assert_eq!(stdout_json(&out)[0]["args"]["text"], "again");
        let out = cli()
            .args(["--db", db_arg, "export-config", "--server", name])
            .output()
            .unwrap();
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        assert_eq!(
            stdout_json(&out)["mcpServers"][printed.as_str()]["args"][0],
            "--schema=v1"
        );
    }
    let _ = std::fs::remove_file(&db);
}

#[test]
fn diff_classifies_schemas_and_exits_1_on_breaking() {
    let db = temp_path("diff.db");
    let db_arg = db.to_str().unwrap();
    let a_file = temp_path("a.json");
    let b_file = temp_path("b.json");
    // Schema v1 twice (so `~1` resolves), v2 once; the schemas are
    // separate saved servers because their command lines differ.
    for (schema, file) in [("v1", &a_file), ("v1", &a_file), ("v2", &b_file)] {
        let out = run(
            &[
                "--db",
                db_arg,
                "snapshot",
                "stdio",
                "--out",
                file.to_str().unwrap(),
            ],
            schema,
        );
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let store = mcp_store::Store::open(&db).unwrap();
    let servers = store.list_servers().unwrap();
    let saved = |schema: &str| {
        servers
            .iter()
            .find(|s| s.name.ends_with(&format!("--schema {schema}")))
            .map(|s| s.name.clone())
            .unwrap()
    };
    let (name, name_b) = (saved("v1"), saved("v2"));

    let out = cli()
        .args(["diff", a_file.to_str().unwrap(), b_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let json = stdout_json(&out);
    assert_eq!(json["outcome"], "changed");
    assert_eq!(json["breaking"], 2);
    let removed = json["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "echo")
        .unwrap();
    assert_eq!(removed["severity"], "breaking");

    let text = cli()
        .args([
            "--db",
            db_arg,
            "diff",
            &format!("saved:{name}~1"),
            &format!("saved:{name_b}"),
            "--text",
        ])
        .output()
        .unwrap();
    assert_eq!(text.status.code(), Some(1), "{}", stderr(&text));
    let listing = String::from_utf8_lossy(&text.stdout);
    assert!(listing.starts_with("2 breaking"), "{listing}");
    assert!(
        listing.contains("breaking    tool echo: tool removed"),
        "{listing}"
    );
    assert!(
        listing.contains("cosmetic    tool sleep description: description changed"),
        "{listing}"
    );

    let same = cli()
        .args([
            "diff",
            a_file.to_str().unwrap(),
            a_file.to_str().unwrap(),
            "--compact",
        ])
        .output()
        .unwrap();
    assert_eq!(same.status.code(), Some(0));
    assert_eq!(stdout_json(&same)["outcome"], "unchanged");

    let missing = cli()
        .args([
            "--db",
            db_arg,
            "diff",
            &format!("saved:{name}~5"),
            a_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        stderr(&missing).contains("none at offset ~5"),
        "{}",
        stderr(&missing)
    );
    let no_db = cli()
        .args(["diff", "saved:x", a_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(no_db.status.code(), Some(2));
    assert!(stderr(&no_db).contains("--db"));

    for f in [&db, &a_file, &b_file] {
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn export_config_prints_saved_servers_without_secrets() {
    let db = temp_path("export.db");
    let store = mcp_store::Store::open(&db).unwrap();
    let mut env = std::collections::BTreeMap::new();
    env.insert("LOG".to_owned(), "debug".to_owned());
    store
        .add_server(
            "local",
            &mcp_core::ServerSpec::Stdio {
                command: "srv".into(),
                args: vec!["--verbose".into(), "srv".into()],
                env,
                cwd: None,
            },
            &mcp_core::ServerRequestPolicy::default(),
        )
        .unwrap();
    store
        .add_server(
            "remote",
            &mcp_core::ServerSpec::Http {
                url: "https://x.test/mcp".into(),
                headers: Default::default(),
                auth: mcp_core::AuthRef::Bearer {
                    keyring_id: "server:abc".into(),
                },
            },
            &mcp_core::ServerRequestPolicy::default(),
        )
        .unwrap();
    drop(store);

    let out = cli()
        .args(["--db", db.to_str().unwrap(), "export-config"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let json = stdout_json(&out);
    assert_eq!(json["mcpServers"]["local"]["command"], "srv");
    assert_eq!(json["mcpServers"]["local"]["args"][1], "srv");
    assert_eq!(json["mcpServers"]["local"]["env"]["LOG"], "debug");
    assert_eq!(json["mcpServers"]["remote"]["url"], "https://x.test/mcp");
    assert_eq!(
        json["mcpServers"]["remote"]["headers"]["Authorization"],
        "Bearer <token>"
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("server:abc"));
    assert!(stderr(&out).contains("keyring"));

    let one = cli()
        .args([
            "--db",
            db.to_str().unwrap(),
            "export-config",
            "--server",
            "local",
        ])
        .output()
        .unwrap();
    assert!(stdout_json(&one)["mcpServers"]["remote"].is_null());
    let unknown = cli()
        .args([
            "--db",
            db.to_str().unwrap(),
            "export-config",
            "--server",
            "zzz",
        ])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
    let _ = std::fs::remove_file(&db);
}

#[test]
fn import_config_adds_the_servers_a_client_file_names() {
    let db = temp_path("import.db");
    let config = temp_path("mcp.json");
    std::fs::write(
        &config,
        r#"{"mcpServers": {
            "files": {"command": "srv", "args": ["--verbose", "filesystem-server", "/tmp"]},
            "remote": {"url": "https://x.test/mcp", "headers": {"X-Tenant": "acme"}},
            "broken": {"note": "neither a command nor a url"}
        }}"#,
    )
    .unwrap();

    let out = cli()
        .args([
            "--db",
            db.to_str().unwrap(),
            "import-config",
            config.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let imported = stdout_json(&out)["imported"].clone();
    assert_eq!(imported.as_array().map(Vec::len), Some(2));
    assert!(stderr(&out).contains("broken"), "{}", stderr(&out));

    // An imported server asks a person about server requests, the same as
    // one added in the app, so opening the database there changes nothing.
    {
        let store = mcp_store::Store::open(&db).unwrap();
        let servers = store.list_servers().unwrap();
        assert_eq!(servers.len(), 2);
        for server in servers {
            assert_eq!(
                server.policy,
                mcp_core::ServerRequestPolicy::default(),
                "{}",
                server.name
            );
            assert_eq!(server.policy.sampling, mcp_core::SamplingPolicy::Prompt);
        }
    }

    // What was imported is what export-config writes back.
    let out = cli()
        .args(["--db", db.to_str().unwrap(), "export-config"])
        .output()
        .unwrap();
    let json = stdout_json(&out);
    assert_eq!(json["mcpServers"]["files"]["command"], "srv");
    assert_eq!(json["mcpServers"]["files"]["args"][2], "/tmp");
    assert_eq!(json["mcpServers"]["remote"]["url"], "https://x.test/mcp");
    assert_eq!(json["mcpServers"]["remote"]["headers"]["X-Tenant"], "acme");

    // A second run adds nothing and says why.
    let again = cli()
        .args([
            "--db",
            db.to_str().unwrap(),
            "import-config",
            config.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(
        stdout_json(&again)["imported"].as_array().map(Vec::len),
        Some(0)
    );
    assert!(
        stderr(&again).contains("already saved"),
        "{}",
        stderr(&again)
    );

    let no_db = cli()
        .args(["import-config", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(no_db.status.code(), Some(2));
    assert!(stderr(&no_db).contains("--db"));

    for f in [&db, &config] {
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn the_binary_names_an_unknown_mode() {
    // A bare `coco-mcp` opens the window, which a test cannot run headless;
    // the other answers are checked here.
    // An unknown mode is named.
    let out = Command::new(coco()).arg("--gui").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("`--gui`"), "{}", stderr(&out));

    // The binary's own version and help, without a mode.
    let out = Command::new(coco()).arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Coco MCP "));
    let out = Command::new(coco()).arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("coco-mcp --cli <COMMAND>"));

    // `--cli --version` is the command line's own, same version.
    let out = cli().arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn bare_invocation_prints_help_and_exits_0() {
    let out = cli().output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Usage: coco-mcp --cli [OPTIONS] [COMMAND]"),
        "{text}"
    );
    for command in [
        "snapshot",
        "call",
        "read",
        "prompt",
        "diff",
        "export-config",
        "import-config",
        "history",
    ] {
        assert!(text.contains(command), "help lists {command}: {text}");
    }
    assert!(stderr(&out).is_empty(), "help goes to stdout");

    // A bare run must not touch the database given with --db.
    let db = temp_path("bare.db");
    let out = cli().args(["--db", db.to_str().unwrap()]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(!db.exists(), "no database is created just to print help");

    // An unknown command is still an error.
    let bad = cli().args(["nope"]).output().unwrap();
    assert_ne!(bad.status.code(), Some(0));
}
