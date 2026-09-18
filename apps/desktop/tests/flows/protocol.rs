//! Headless flows for the protocol a live session carries beyond calls:
//! cancelling, progress, output schemas, lists that change, subscriptions,
//! the server's log level, completions, a server that stops answering, the
//! Server view and roots, and requests a server withdraws. Each drives the
//! real UI against the mock server binary.

use std::time::Duration;

use coco_mcp::calls::ResponseStatus;
use coco_mcp::state::{AppState, Mode, Screen, Status};
use coco_mcp::views::Workspace;
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, HeadlessAppContext, SharedString, WindowHandle, px, size};
use mcp_core::ServerSpec;
use serde_json::{Value, json};

use crate::macos::{context, item_index, mock_binary, snap, wait_for_request};

/// A window on the mock server, started with `args` and connected.
///
/// Fields drop in order, and the context checks for leaked entities when it
/// drops, so it comes after every handle it owns.
pub(crate) struct Live {
    pub(crate) handle: WindowHandle<Root>,
    pub(crate) state: Entity<AppState>,
    pub(crate) workspace: Entity<Workspace>,
    pub(crate) store: mcp_store::Store,
    pub(crate) cx: HeadlessAppContext,
}

pub(crate) fn live(args: &[&str], keepalive: Option<Duration>) -> Live {
    live_in(mcp_core::ProtocolMode::Legacy, args, keepalive)
}

/// [`live`], with the server saved to connect in `protocol`.
pub(crate) fn live_in(
    protocol: mcp_core::ProtocolMode,
    args: &[&str],
    keepalive: Option<Duration>,
) -> Live {
    let mock = mock_binary();
    let bridge = coco_mcp::bridge::Bridge::new().unwrap();
    let store = mcp_store::Store::open_in_memory().unwrap();
    let spec = ServerSpec::Stdio {
        command: mock.display().to_string(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        env: Default::default(),
        cwd: None,
    };
    store
        .add_server_with(
            "mock",
            &spec,
            &mcp_core::ServerRequestPolicy::default(),
            protocol,
        )
        .unwrap();
    let mut model = AppState::new(Some(bridge), Some(store.clone()));
    model.keepalive = keepalive;
    let mut live = open(model, store);
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.connect(0, cx)));
    live.wait("the mock server connects", |s| {
        s.servers[0].status == Status::Connected
    });
    live
}

/// A window on `model`, which `store` backs if anything does.
pub(crate) fn open(model: AppState, store: mcp_store::Store) -> Live {
    let mut cx = context();
    let state = cx.update(|cx| cx.new(|_| model));
    let for_window = state.clone();
    let mut workspace = None;
    let handle = cx
        .open_window(size(px(1200.), px(760.)), |window, cx| {
            let view = cx.new(|cx| Workspace::new(for_window, window, cx));
            workspace = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    Live {
        cx,
        handle,
        state,
        workspace: workspace.unwrap(),
        store,
    }
}

impl Live {
    /// Pump frames until `done` holds for the model.
    pub(crate) fn wait(&mut self, what: &str, done: impl Fn(&AppState) -> bool) {
        let state = self.state.clone();
        for _ in 0..400 {
            std::thread::sleep(Duration::from_millis(25));
            self.cx.run_until_parked();
            let ok = self
                .cx
                .update_window(self.handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    done(state.read(cx))
                })
                .unwrap();
            if ok {
                return;
            }
        }
        panic!("timed out waiting until {what}");
    }

    /// Run `f` against the window, with a frame drawn before and after.
    pub(crate) fn ui<R>(
        &mut self,
        f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
    ) -> R {
        self.cx
            .update_window(self.handle.into(), |_, window, cx| {
                window.render_frame(cx);
                let out = f(window, cx);
                window.render_frame(cx);
                out
            })
            .unwrap()
    }

    pub(crate) fn show(&mut self, mode: Mode) {
        let ix = Mode::ALL.iter().position(|m| *m == mode).unwrap();
        self.ui(|window, cx| window.click(("mode", ix), cx));
    }

    pub(crate) fn select(&mut self, mode: Mode, label: &str) {
        self.show(mode);
        let ix = item_index(&mut self.cx, &self.state, label);
        self.ui(|window, cx| window.click(("item", ix), cx));
    }

    /// Select tool `name` and call it with `args`, as the Call button does
    /// once the form is filled in.
    pub(crate) fn call(&mut self, name: &str, args: Value) {
        self.select(Mode::Tools, name);
        let name = name.to_owned();
        self.cx
            .update(|cx| self.state.update(cx, |s, cx| s.call_tool(name, args, cx)));
    }

    /// The selection's finished response.
    pub(crate) fn answered(&mut self) -> (ResponseStatus, Value) {
        self.wait("the response arrives", |s| {
            s.response()
                .is_some_and(|r| r.status != ResponseStatus::Pending)
        });
        self.cx.update(|cx| {
            let response = self.state.read(cx).response().unwrap();
            (response.status.clone(), response.raw.clone())
        })
    }

    pub(crate) fn text(&mut self) -> String {
        let (_, raw) = self.answered();
        raw["content"][0]["text"]
            .as_str()
            .or_else(|| raw["contents"][0]["text"].as_str())
            .unwrap_or_default()
            .to_owned()
    }

    pub(crate) fn logged(&mut self, method: &str) -> bool {
        self.cx.update(|cx| {
            self.state.read(cx).servers[0]
                .log()
                .iter()
                .any(|row| row.method == method)
        })
    }
}

/// A call that reports progress shows it with a running clock, and Cancel
/// (the button, then ⌘.) stops it and tells the server.
pub fn cancel_and_progress_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("progress", json!({"steps": 200, "delay_ms": 40}));
    live.wait("progress is reported", |s| {
        s.response_waiting().is_some_and(|w| w.progress.is_some())
    });
    live.ui(|window, _| {
        assert!(window.try_find("call-progress").is_some(), "progress shown");
        assert!(window.try_find("cancel-call").is_some(), "a way out");
    });
    snap(&mut live.cx, live.handle, "35-call-progress");
    live.ui(|window, cx| window.click("cancel-call", cx));
    assert_eq!(live.answered().0, ResponseStatus::Cancelled);
    live.wait("the cancellation is logged", |s| {
        s.servers[0]
            .log()
            .iter()
            .any(|row| row.method == "notifications/cancelled")
    });

    // ⌘. does the same while the next call waits.
    live.call("sleep", json!({"millis": 5000}));
    live.wait("the call is sent", |s| s.response_pending());
    live.ui(|window, cx| window.press("cmd-.", cx));
    assert_eq!(live.answered().0, ResponseStatus::Cancelled);
}

/// A result whose `structuredContent` breaks the tool's declared output
/// schema is flagged above the result; a conforming one is not.
pub fn output_schema_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("add", json!({"a": 2, "b": 3}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.ui(|window, _| assert!(window.try_find("output-issues").is_none()));
    live.call("mistyped", json!({}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    let issues = live
        .cx
        .update(|cx| live.state.read(cx).response().unwrap().issues.clone());
    assert!(
        issues.iter().any(|issue| issue.starts_with("$.value")),
        "{issues:?}"
    );
    live.ui(|window, _| assert!(window.try_find("output-issues").is_some()));
    snap(&mut live.cx, live.handle, "36-output-schema");
}

/// A server that announces a changed list has it read again in place: the
/// session stays, and the new item is marked until it is selected.
pub fn list_changed_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("add_resource", json!({"name": "notes"}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.wait("the new resource is listed", |s| {
        s.snapshot()
            .is_some_and(|snap| snap.resource("mock://extra/notes").is_some())
    });
    live.show(Mode::Resources);
    let ix = item_index(&mut live.cx, &live.state, "mock://extra/notes");
    live.ui(|window, _| {
        assert!(window.try_find(("changed", ix)).is_some(), "marked");
    });
    snap(&mut live.cx, live.handle, "37-list-changed");
    live.ui(|window, cx| window.click(("item", ix), cx));
    live.ui(|window, _| {
        assert!(window.try_find(("changed", ix)).is_none(), "seen");
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert_eq!(s.servers[0].status, Status::Connected, "no reconnect");
    });
}

/// Subscribing to a resource marks its row, and a change the server
/// reports reads it again under the response already shown.
pub fn subscription_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.select(Mode::Resources, "mock://counter");
    live.ui(|window, cx| window.click("subscribe", cx));
    live.wait("the subscription is taken", |s| {
        s.servers[0].is_subscribed("mock://counter")
    });
    live.ui(|window, cx| window.click("call", cx));
    assert_eq!(live.text(), "0");
    live.call("bump", json!({}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.select(Mode::Resources, "mock://counter");
    live.wait("the change is read again", |s| {
        s.response()
            .is_some_and(|r| r.raw["contents"][0]["text"] == "1")
    });
    live.ui(|window, _| {
        assert!(window.try_find("resource-updated").is_some());
    });
    snap(&mut live.cx, live.handle, "38-subscribed");
    live.ui(|window, cx| window.click("subscribe", cx));
    live.wait("the subscription ends", |s| {
        !s.servers[0].is_subscribed("mock://counter")
    });
}

/// The drawer sets the server's log level, and a server error it logs is
/// shown, and filtered, as an error.
pub fn log_level_flow() {
    use coco_mcp::state::LogFilter;
    let mut live = live(&["--schema", "v1"], None);
    live.ui(|window, cx| window.click("log-header", cx));
    live.ui(|window, _| assert!(window.try_find("log-level").is_some()));
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.choose_log_level(Some("error"), cx))
    });
    live.wait("the level is set", |s| {
        s.servers[0].log_level.as_deref() == Some("error")
    });
    live.call("log", json!({"level": "info", "message": "quiet"}));
    assert_eq!(live.text(), "logged=false");
    live.call("log", json!({"level": "error", "message": "disk full"}));
    assert_eq!(live.text(), "logged=true");
    live.wait("the error is logged", |s| {
        s.servers[0]
            .log()
            .iter()
            .any(|r| r.level.as_deref() == Some("error") && r.is_error)
    });
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.set_log_filter(LogFilter::Errors, cx))
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        let log = s.servers[0].log();
        assert!(
            s.visible_log()
                .iter()
                .any(|&ix| log[ix].body.contains("disk full")),
            "the errors filter shows it"
        );
    });
    snap(&mut live.cx, live.handle, "39-log-level");
}

/// Prompt arguments and template variables are completed by the server as
/// they are typed; a suggestion fills the input.
pub fn completion_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.select(Mode::Prompts, "greet");
    live.ui(|window, cx| {
        window.click("arg-name", cx);
        window.input("G", cx);
    });
    let workspace = live.workspace.clone();
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(25));
        live.cx.run_until_parked();
        let found = live.cx.update(|cx| {
            workspace
                .read(cx)
                .selection
                .suggestions
                .get("name")
                .cloned()
        });
        if found.is_some() {
            break;
        }
    }
    live.cx.update(|cx| {
        let suggestions = workspace
            .read(cx)
            .selection
            .suggestions
            .get("name")
            .cloned();
        assert_eq!(
            suggestions,
            Some(vec!["Grace".to_owned(), "Guido".to_owned()])
        );
    });
    snap(&mut live.cx, live.handle, "40-completions");
    live.ui(|window, cx| window.click(SharedString::from("suggest-name-1"), cx));
    live.ui(|window, _| {
        assert_eq!(window.find("arg-name").value(), Some("Guido"));
    });

    live.select(Mode::Resources, "mock://item/{id}");
    live.ui(|window, cx| {
        window.click("var-id", cx);
        window.input("4", cx);
    });
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(25));
        live.cx.run_until_parked();
        let found = live
            .cx
            .update(|cx| workspace.read(cx).selection.suggestions.get("id").cloned());
        if found.is_some() {
            break;
        }
    }
    live.cx.update(|cx| {
        let suggestions = workspace.read(cx).selection.suggestions.get("id").cloned();
        assert_eq!(suggestions, Some(vec!["42".to_owned()]));
    });
}

/// A server that stops answering while its transport stays open is noticed
/// by the keepalive and shown as failed, instead of reading as connected.
pub fn keepalive_flow() {
    let mut live = live(
        &["--schema", "v1", "--ignore-pings"],
        Some(Duration::from_millis(200)),
    );
    live.wait(
        "the silent server is noticed",
        |s| matches!(&s.servers[0].status, Status::Error(e) if e.contains("stopped answering")),
    );
    live.ui(|window, _| {
        assert!(window.try_find("connect-server").is_some(), "Retry offered");
    });
}

/// The Server view shows what the server said about itself and which lists
/// came back; roots saved there are stored, announced to the server and
/// offered when it asks.
pub fn server_view_flow() {
    let mut live = live(&["--schema", "v1", "--no-templates"], None);
    live.show(Mode::Server);
    live.ui(|window, _| {
        assert!(window.try_find("server-instructions").is_some());
        for ix in 0..4usize {
            assert!(window.try_find(("list-status", ix)).is_some());
        }
    });
    snap(&mut live.cx, live.handle, "41-server-view");
    live.select(Mode::Server, "Roots");
    live.ui(|window, cx| {
        window.click("server-roots", cx);
        window.input("file:///tmp/project", cx);
        window.click("save-roots", cx);
    });
    let id = live
        .cx
        .update(|cx| live.state.read(cx).servers[0].record.id.clone());
    live.wait("the server is told", |s| {
        s.servers[0]
            .log()
            .iter()
            .any(|row| row.method == "notifications/roots/list_changed")
    });
    let stored: Option<Vec<mcp_core::Root>> =
        live.store.get_setting(&format!("roots.{id}")).unwrap();
    assert_eq!(
        stored.map(|roots| roots.into_iter().map(|r| r.uri).collect::<Vec<_>>()),
        Some(vec!["file:///tmp/project".to_owned()])
    );

    // When the server asks, the dialog offers the saved roots.
    live.call("roots", json!({}));
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.ui(|window, cx| window.click("req-accept", cx));
    assert!(live.text().contains("file:///tmp/project"));
    assert!(live.logged("roots/list"));
}

/// A request the server withdraws closes its dialog without an answer.
pub fn withdrawn_request_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call(
        "elicit",
        json!({"question": "still there?", "timeout_ms": 400}),
    );
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.wait("the dialog closes on its own", |s| {
        s.current_request().is_none()
    });
    live.ui(|window, _| {
        assert!(window.try_find("request-dialog").is_none());
    });
    assert_eq!(live.text(), "withdrawn");
}

/// A server that turns a connect away for want of authorization says so,
/// and Authorize gets credentials: the edit form for a server without OAuth;
/// for one with it, the browser flow again, stored credentials dropped.
pub fn authorize_flow() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let bridge = coco_mcp::bridge::Bridge::new().unwrap();
    let server = futures::executor::block_on(bridge.run(mcp_mockserver::http::serve_http(
        mcp_mockserver::Schema::V1,
        mcp_mockserver::http::HttpAuth::OAuth { expires_in: 3600 },
    )))
    .unwrap()
    .unwrap();
    let opened = Arc::new(AtomicU32::new(0));
    let counter = opened.clone();
    let mut model = AppState::new(Some(bridge), None);
    // Stands in for the browser: follows the authorization URL, which the
    // mock authorization server answers by redirecting to the app's listener.
    model.oauth_open = Some(Arc::new(move |url: String| {
        counter.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let _ = reqwest::Client::new().get(url).send().await;
        });
    }));
    let http = |auth| ServerSpec::Http {
        url: server.url.clone(),
        headers: Default::default(),
        auth,
    };
    model.add_demo_server("plain", http(mcp_core::AuthRef::None), Status::Off);
    let oauth = mcp_core::AuthRef::OAuth {
        keyring_id: "remote-oauth".into(),
    };
    model.add_demo_server("oauth", http(oauth), Status::Off);
    let mut live = open(model, mcp_store::Store::open_in_memory().unwrap());

    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.connect(0, cx)));
    live.wait("the server turns the connect away", |s| {
        s.servers[0].unauthorized && matches!(s.servers[0].status, Status::Error(_))
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert!(
            s.servers[0].auth_challenge.is_some(),
            "its challenge is kept"
        );
    });
    live.ui(|window, _| assert!(window.try_find("authorize").is_some()));
    snap(&mut live.cx, live.handle, "42-authorize");
    live.ui(|window, cx| window.click("authorize", cx));
    live.cx.update(|cx| {
        assert_eq!(
            live.state.read(cx).screen,
            Screen::AddServer,
            "credentials are set in the edit form"
        );
    });

    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            s.cancel_add_server(cx);
            s.connect(1, cx);
        })
    });
    live.wait("the OAuth server connects", |s| {
        s.servers[1].status == Status::Connected
    });
    assert_eq!(opened.load(Ordering::Relaxed), 1, "one browser round trip");
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.authorize(1, cx)));
    live.wait("it authorizes again and connects", |s| {
        s.servers[1].status == Status::Connected && s.servers[1].session.is_some()
    });
    for _ in 0..200 {
        if opened.load(Ordering::Relaxed) >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
        live.cx.run_until_parked();
    }
    assert_eq!(
        opened.load(Ordering::Relaxed),
        2,
        "the stored credentials were dropped and the flow ran again"
    );
    server.shutdown();
}
