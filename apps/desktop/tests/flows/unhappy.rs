//! Headless flows for what goes wrong: a call the server refuses, a server
//! that dies mid-session, server requests that are cancelled, declined,
//! queued behind each other or sent in URL mode, and replaying the reads and
//! prompts History recorded.

use coco_mcp::calls::ResponseStatus;
use coco_mcp::state::{AppState, Mode, Status};
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

/// A server process that exits mid-session leaves its log, says the session
/// ended, and connects again.
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
    });
    live.ui(|window, _| {
        assert!(window.try_find("connect-server").is_some(), "a way back");
    });
    snap(&mut live.cx, live.handle, "50-server-exited");
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.connect(0, cx)));
    live.wait("it connects again", |s| {
        s.servers[0].status == Status::Connected
    });
    live.call("echo", json!({"text": "back"}));
    assert_eq!(live.text(), "back");
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
