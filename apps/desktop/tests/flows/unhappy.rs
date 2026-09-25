//! Headless flows for what goes wrong: a call the server refuses, a server
//! that dies mid-session, server requests that are cancelled, declined,
//! queued behind each other or sent in URL mode, and replaying the reads and
//! prompts History recorded.

use coco_mcp::calls::ResponseStatus;
use coco_mcp::state::{AppState, Mode, Screen, Status};
use gpui_kit::test::TestWindowExt as _;
use serde_json::{Map, Value, json};

use crate::macos::{item_index, snap, wait_for_request};
use crate::protocol::{Live, live};

/// The finished response of tool `name`, whether or not it is selected.
fn finished(live: &mut Live, name: &str) -> (ResponseStatus, Value) {
    let id = live
        .cx
        .update(|cx| live.state.read(cx).servers[0].record.id.clone());
    let key = (id, Mode::Tools, name.to_owned());
    let done = key.clone();
    live.wait(&format!("{name} answers"), move |s: &AppState| {
        s.responses
            .get(&done)
            .is_some_and(|r| r.status != ResponseStatus::Pending)
    });
    live.cx.update(|cx| {
        let response = live.state.read(cx).responses.get(&key).unwrap();
        (response.status.clone(), response.raw.clone())
    })
}

fn text(raw: &Value) -> String {
    raw["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// A request the server refuses shows why in place of a result.
pub fn failed_call_flow() {
    let mut live = live(&["--schema", "v1"], None);
    // A URI the server does not have: it answers resource not found, a
    // JSON-RPC error rather than a result.
    live.select(Mode::Resources, "mock://text/hello");
    let read = |live: &mut Live, uri: &str| {
        let uri = uri.to_owned();
        live.cx.update(|cx| {
            live.state.update(cx, |s, cx| {
                s.read_resource("mock://text/hello".into(), uri, cx);
            })
        });
    };
    read(&mut live, "mock://nope");
    let (status, _) = live.answered();
    assert!(matches!(status, ResponseStatus::Failed(_)), "{status:?}");
    live.ui(|window, _| {
        assert!(
            window.try_find("call-error").is_some(),
            "the error is shown"
        );
    });
    snap(&mut live.cx, live.handle, "49-failed-call");
    // The next good read replaces it.
    read(&mut live, "mock://text/hello");
    assert!(live.text().starts_with("Hello"));
    live.ui(|window, _| assert!(window.try_find("call-error").is_none()));
}

/// A server process that exits mid-session leaves its log, says the server
/// ended the session, and connects again.
pub fn crash_mid_session_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("stderr", json!({"lines": ["about to exit"]}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    let said = |s: &AppState| {
        s.servers[0]
            .log()
            .iter()
            .any(|row| row.method == "stderr" && row.body == "about to exit")
    };
    live.wait("the stderr line is logged", said);
    live.call("exit", json!({"code": 3}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.wait("the session ends", |s| {
        s.servers[0].status != Status::Connected
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert!(said(s), "the log outlives the process");
        assert!(!s.response_pending());
        assert_eq!(
            s.servers[0].status,
            Status::Error("The server ended the session.".into()),
            "a crash is not a disconnect"
        );
    });
    // The server is shown as its settings, the failure under the fields and
    // Connect as the way back.
    live.ui(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("connect-error").is_some(), "why");
        assert!(window.try_find("connect").is_some(), "a way back");
        // The settings are the pane: no pencil opens them again.
        assert!(window.try_find("edit-server").is_none());
        // The tools are the last connection's: said so, and not opened.
        assert!(window.try_find("list-stale").is_some());
    });
    let before = live.cx.update(|cx| live.state.read(cx).selected_item);
    live.ui(|window, cx| window.click(("item", 0usize), cx));
    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| s.move_item(1, cx));
        assert_eq!(
            live.state.read(cx).selected_item,
            before,
            "neither a click nor a key selects a row"
        );
    });
    snap(&mut live.cx, live.handle, "50-server-exited");
    // With the settings as the pane, ⌘E (the menu, the palette) goes to
    // them rather than opening a second copy.
    live.ui(|window, cx| window.press("cmd-e", cx));
    live.ui(|window, _| {
        assert_eq!(
            window.find("name").focused(),
            Some(true),
            "the keyboard is in the form"
        );
    });
    live.cx.update(|cx| {
        assert_eq!(live.state.read(cx).screen, Screen::Detail, "no second form");
    });
    // The pane's Connect, with the settings as saved, just connects: the
    // tool selected before the crash is selected again, nothing is saved.
    let tool = live.cx.update(|cx| live.state.read(cx).selected_name());
    live.ui(|window, cx| window.click("connect", cx));
    live.wait("it connects again", |s| {
        s.servers[0].status == Status::Connected
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert_eq!(s.selected_name(), tool, "the selection outlives the crash");
        assert_eq!(s.screen, Screen::Detail, "no edit form was opened");
    });
    live.ui(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("list-stale").is_none(),
            "the rows open again"
        );
        assert!(window.try_find("edit-server").is_some());
    });
    live.call("echo", json!({"text": "back"}));
    assert_eq!(live.text(), "back");
}

/// The edit form's button says what saving will do to the session as it is
/// now: Save & reconnect while connected, Connect once the row's plug has
/// disconnected the server under the open form, and back again.
pub fn edit_form_follows_the_session_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.show_edit_selected(cx)));
    live.ui(|window, _| {
        assert_eq!(window.find("connect").label(), Some("Save & reconnect"));
    });
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.toggle_connection(0, cx)));
    live.ui(|window, _| {
        assert_eq!(
            window.find("connect").label(),
            Some("Connect"),
            "no session to end"
        );
    });
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.toggle_connection(0, cx)));
    live.wait("connected again", |s| {
        s.servers[0].status == Status::Connected
    });
    live.ui(|window, _| {
        assert_eq!(window.find("connect").label(), Some("Save & reconnect"));
    });
}

/// Server requests the user does not simply accept: Esc cancels, Decline
/// declines, a second request waits for the first, roots can be sent empty,
/// and a URL elicitation shows the URL it would open.
pub fn request_outcomes_flow() {
    let mut live = live(&["--schema", "v1"], None);

    live.call("elicit", json!({"question": "stay?"}));
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.ui(|window, cx| window.press("escape", cx));
    assert_eq!(live.text(), "cancelled");

    // Two requests at once: the second waits behind the first, in whichever
    // order they arrived.
    live.show(Mode::Tools);
    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            s.call_tool("elicit".into(), json!({"question": "first?"}), cx);
            s.call_tool("sample".into(), json!({"prompt": "second"}), cx);
        })
    });
    live.wait("both requests arrive", |s| s.pending.len() == 2);
    let sampling_first = |live: &mut Live| {
        live.cx.update(|cx| {
            matches!(
                live.state
                    .read(cx)
                    .current_request()
                    .map(|p| &p.request.kind),
                Some(mcp_core::ServerRequestKind::Sampling(_))
            )
        })
    };
    let first_sampling = sampling_first(&mut live);
    snap(&mut live.cx, live.handle, "51-queued-requests");
    live.ui(|window, cx| window.click("req-decline", cx));
    live.wait("the first is answered", |s| s.pending.len() == 1);
    assert_ne!(
        sampling_first(&mut live),
        first_sampling,
        "the other is next"
    );
    live.ui(|window, cx| window.click("req-cancel", cx));
    let elicited = text(&finished(&mut live, "elicit").1);
    let expected = if first_sampling {
        "cancelled"
    } else {
        "declined"
    };
    assert_eq!(elicited, expected);
    let refused = text(&finished(&mut live, "sample").1);
    assert!(refused.starts_with("sampling error"), "{refused}");
    live.ui(|window, _| assert!(window.try_find("request-dialog").is_none()));

    // Roots answered with none at all.
    live.call("roots", json!({}));
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.cx
        .update(|cx| live.workspace.update(cx, |ws, cx| ws.decline_request(cx)));
    let roots: Value = serde_json::from_str(&live.text()).unwrap();
    assert_eq!(roots["roots"], json!([]));

    // A URL elicitation is shown for what it is, and declining it opens
    // nothing.
    live.call(
        "elicit",
        json!({"question": "verify?", "url": "https://example.com/verify"}),
    );
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert!(matches!(
            s.current_request().map(|p| &p.request.kind),
            Some(mcp_core::ServerRequestKind::Elicitation {
                mode: mcp_core::ElicitationMode::Url { .. },
                ..
            })
        ));
    });
    snap(&mut live.cx, live.handle, "52-url-elicitation");
    live.ui(|window, cx| window.click("req-decline", cx));
    assert_eq!(live.text(), "declined");
}

/// A resource read and a prompt get are recorded, and History replays both.
pub fn replay_reads_and_prompts_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.select(Mode::Resources, "mock://text/hello");
    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            s.read_resource("mock://text/hello".into(), "mock://text/hello".into(), cx)
        })
    });
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.select(Mode::Prompts, "greet");
    let mut args = Map::new();
    args.insert("name".into(), json!("Ada"));
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.get_prompt("greet".into(), args, cx))
    });
    assert_eq!(live.answered().0, ResponseStatus::Ok);

    live.show(Mode::History);
    live.wait("both are recorded", |s| s.count(Mode::History) == Some(2));
    for (name, recorded) in [("greet", 3), ("mock://text/hello", 4)] {
        let ix = item_index(&mut live.cx, &live.state, name);
        live.ui(|window, cx| window.click(("item", ix), cx));
        live.ui(|window, cx| window.click("replay", cx));
        live.wait(&format!("{name} is replayed"), move |s| {
            s.count(Mode::History) == Some(recorded)
        });
        assert_eq!(live.answered().0, ResponseStatus::Ok, "{name}");
    }
    let (_, raw) = live.answered();
    assert!(
        raw["contents"][0]["text"]
            .as_str()
            .is_some_and(|t| t.starts_with("Hello")),
        "{raw}"
    );
}

/// A window on an HTTP server that wants `expected` as its bearer token,
/// with `held` saved as the token to send.
fn bearer_live(expected: &str, held: &str) -> (mcp_mockserver::http::HttpServer, Live) {
    let bridge = coco_mcp::bridge::Bridge::new().unwrap();
    let server = futures::executor::block_on(bridge.run(mcp_mockserver::http::serve_http(
        mcp_mockserver::Schema::V1,
        mcp_mockserver::http::HttpAuth::Bearer(expected.into()),
    )))
    .unwrap()
    .unwrap();
    let mut model = AppState::new(Some(bridge), None);
    model.add_demo_server(
        "remote",
        mcp_core::ServerSpec::Http {
            url: server.url.clone(),
            headers: Default::default(),
            auth: mcp_core::AuthRef::Bearer {
                keyring_id: "remote-token".into(),
            },
        },
        Status::Off,
    );
    model.secrets.set("remote-token", held).unwrap();
    let live = crate::protocol::open(model, mcp_store::Store::open_in_memory().unwrap());
    (server, live)
}

/// The failure text of the selected server.
fn failure(live: &mut Live) -> String {
    live.cx
        .update(|cx| match &live.state.read(cx).servers[0].status {
            Status::Error(text) => text.clone(),
            other => panic!("not failed: {other:?}"),
        })
}

/// A bearer token the server refuses is named as the problem, in a
/// sentence, under the fields of the settings the pane shows.
pub fn refused_token_flow() {
    let (_server, mut live) = bearer_live("s3cret", "expired");
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.connect(0, cx)));
    live.wait("the token is refused", |s| {
        matches!(s.servers[0].status, Status::Error(_))
    });
    let text = failure(&mut live);
    assert_eq!(
        text,
        "The server refused the token. Update it in the server settings."
    );
    live.ui(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("token").is_some(), "the token to update");
        assert!(window.try_find("connect-error").is_some(), "the failure");
        assert!(window.try_find("authorize").is_none());
    });
    snap(&mut live.cx, live.handle, "68-token-refused");
}

/// A token the server stops accepting mid-session fails the next call in
/// the same words, so the pane points at the setting rather than at the
/// transport.
pub fn expired_token_flow() {
    let (server, mut live) = bearer_live("s3cret", "s3cret");
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.connect(0, cx)));
    live.wait("connected", |s| s.servers[0].status == Status::Connected);
    server.revoke_tokens();
    live.call("add", json!({"a": 1, "b": 2}));
    match live.answered().0 {
        ResponseStatus::Failed(text) => assert_eq!(
            text,
            "The server refused the token. Update it in the server settings."
        ),
        other => panic!("{other:?}"),
    }
    snap(&mut live.cx, live.handle, "68-token-expired");
}

/// A server that cannot be reached is named by its address, in a sentence,
/// with the transport's reason after it and none of its type paths.
pub fn unreachable_server_flow() {
    let mut model = AppState::new(Some(coco_mcp::bridge::Bridge::new().unwrap()), None);
    model.add_demo_server(
        "nowhere",
        mcp_core::ServerSpec::Http {
            url: "http://127.0.0.1:9/mcp".into(),
            headers: Default::default(),
            auth: mcp_core::AuthRef::None,
        },
        Status::Off,
    );
    let mut live = crate::protocol::open(model, mcp_store::Store::open_in_memory().unwrap());
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.connect(0, cx)));
    live.wait("the connect fails", |s| {
        matches!(s.servers[0].status, Status::Error(_))
    });
    let text = failure(&mut live);
    assert!(
        text.starts_with("Could not reach the server at http://127.0.0.1:9/mcp: "),
        "{text}"
    );
    assert!(!text.contains('['), "no type paths: {text}");
    // The cause, not the layers that only say where it happened.
    assert_eq!(
        text,
        "Could not reach the server at http://127.0.0.1:9/mcp: connection refused."
    );
    snap(&mut live.cx, live.handle, "68-unreachable");

    // Connecting again from outside the form (as ⌘R or the plug do) keeps
    // the pane's form, and what was typed in it, through the connect.
    live.ui(|window, cx| {
        window.click("name", cx);
        window.press("cmd-a", cx);
        window.input("still typing", cx);
    });
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.connect(0, cx)));
    live.ui(|window, _| {
        assert_eq!(
            window.find("name").value(),
            Some("still typing"),
            "kept while connecting"
        );
    });
    live.wait("the connect fails again", |s| {
        matches!(s.servers[0].status, Status::Error(_))
    });
    live.ui(|window, _| {
        assert_eq!(
            window.find("name").value(),
            Some("still typing"),
            "kept after it failed"
        );
    });
}

/// A server saved from the form is connected with the form kept open: a
/// command that cannot start leaves the form there with the failure under
/// its fields, and once the command is fixed and connects, the form closes.
pub fn form_stays_until_connected_flow() {
    let store = mcp_store::Store::open_in_memory().unwrap();
    let model = AppState::new(
        Some(coco_mcp::bridge::Bridge::new().unwrap()),
        Some(store.clone()),
    );
    let mut live = crate::protocol::open(model, store);
    live.ui(|window, cx| {
        window.click("add-server", cx);
        window.render_frame(cx);
        window.click("name", cx);
        window.input("local", cx);
        window.click("command", cx);
        window.input("nope-not-a-server", cx);
        window.click("connect", cx);
    });
    live.wait("the connect fails", |s| {
        matches!(s.servers.first().map(|e| &e.status), Some(Status::Error(_)))
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert_eq!(s.screen, Screen::AddServer, "the form stays");
        assert_eq!(s.editing, Some(0), "on the saved server");
    });
    live.ui(|window, _| {
        assert!(
            window.try_find("connect-error").is_some(),
            "the failure is under the fields"
        );
        assert!(window.try_find("connect").is_some(), "ready to try again");
    });
    snap(&mut live.cx, live.handle, "69-form-connect-failed");

    let mock = crate::macos::mock_binary().display().to_string();
    live.ui(|window, cx| {
        window.click("command", cx);
        window.press("cmd-a", cx);
        window.input(&mock, cx);
        window.click("connect", cx);
    });
    live.wait("the fixed command connects", |s| {
        s.servers[0].status == Status::Connected
    });
    live.wait("the form closes", |s| {
        s.screen == Screen::Detail && s.editing.is_none()
    });
    assert_eq!(
        live.store.list_servers().unwrap().len(),
        1,
        "saved in place, not added again"
    );
}
