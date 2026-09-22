//! Headless flows for how the window answers the keyboard and says what it
//! cannot show: dialogs that take Enter and Esc, shortcuts a dialog holds
//! back, empty lists and logs, display titles, the declared-schema tab and a
//! replay that needs a session, and the log drawer zoomed and restored.

use coco_mcp::calls::ResponseStatus;
use coco_mcp::state::{AppState, Confirm, LogFilter, Mode, Screen, Status};
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;

use crate::macos::{item_index, snap, wait_for_request};
use crate::protocol::live;

/// A confirmation takes Esc and Enter, a server request takes Enter, and the
/// window's shortcuts wait while either is open.
pub fn dialog_keyboard_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("echo", json!({"text": "to be cleared"}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.show(Mode::History);
    live.wait("the call is listed", |s| s.count(Mode::History) == Some(1));
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.request_clear_history(cx)));
    live.ui(|window, _| assert!(window.try_find("confirm-dialog").is_some()));
    // ⌘E would open the edit form behind the dialog.
    live.ui(|window, cx| window.press("cmd-e", cx));
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert_ne!(s.screen, Screen::AddServer, "held back by the dialog");
        assert_eq!(s.confirm, Some(Confirm::ClearHistory(0)));
    });
    live.ui(|window, cx| window.press("escape", cx));
    live.wait("Esc cancels", |s| s.confirm.is_none());
    assert_eq!(
        live.cx
            .update(|cx| live.state.read(cx).count(Mode::History)),
        Some(1)
    );

    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.request_clear_history(cx)));
    live.ui(|window, cx| window.press("enter", cx));
    live.wait("Enter confirms", |s| {
        s.confirm.is_none() && s.count(Mode::History) == Some(0)
    });

    // A server request is answered with Enter.
    live.call("roots", json!({}));
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.ui(|window, cx| window.press("enter", cx));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.ui(|window, _| assert!(window.try_find("request-dialog").is_none()));

    // With no dialog open, ⌘E edits the selected server and Esc leaves.
    live.ui(|window, cx| window.press("cmd-e", cx));
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert_eq!(s.screen, Screen::AddServer);
        assert_eq!(s.editing, Some(0));
    });
    live.ui(|window, cx| window.press("escape", cx));
    live.wait("the form closes", |s| s.screen != Screen::AddServer);
}

/// An empty list, a filter that hides everything and an empty log each say
/// so rather than show a blank.
pub fn empty_states_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.show(Mode::Tools);
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.set_filter("no such tool".into(), cx))
    });
    live.ui(|window, _| {
        assert!(window.try_find("list-empty").is_some(), "filtered out");
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert!(s.items().is_empty());
        assert!(s.items_total() > 0, "the tools are still there");
    });
    snap(&mut live.cx, live.handle, "46-nothing-matches");
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.set_filter(String::new(), cx))
    });
    live.ui(|window, _| assert!(window.try_find("list-empty").is_none()));

    live.show(Mode::History);
    live.wait("the history is read", |s| s.count(Mode::History) == Some(0));
    live.ui(|window, _| {
        assert!(window.try_find("list-empty").is_some(), "no calls yet");
    });

    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            if !s.drawer_open {
                s.toggle_drawer(cx);
            }
            s.set_log_filter(LogFilter::Errors, cx);
        })
    });
    live.ui(|window, _| {
        assert!(window.try_find("log-empty").is_some(), "no errors logged");
    });
    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            s.set_log_filter(LogFilter::All, cx);
        })
    });
    live.ui(|window, _| assert!(window.try_find("log-empty").is_none()));
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.clear_log(cx)));
    live.ui(|window, _| {
        assert!(window.try_find("log-empty").is_some(), "cleared");
    });
}

/// A resource with a display title is listed by it, found by its URI too,
/// and its declaration is one tab away.
pub fn titles_and_declaration_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.ui(|window, _| assert!(window.try_find("server-rows").is_some()));
    live.show(Mode::Resources);
    let ix = item_index(&mut live.cx, &live.state, "mock://text/hello");
    live.cx.update(|cx| {
        let items = live.state.read(cx).items();
        assert_eq!(items[ix].label, "Hello", "the title, not the URI");
    });
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.set_filter("text/hello".into(), cx))
    });
    assert_eq!(
        live.cx.update(|cx| live.state.read(cx).items().len()),
        1,
        "the filter matches the URI behind the title"
    );
    live.select(Mode::Resources, "mock://text/hello");
    live.ui(|window, _| {
        assert!(window.try_find("tab-detail").is_some());
        assert!(window.try_find("tab-schema").is_some());
    });
    live.ui(|window, cx| window.click("tab-schema", cx));
    live.cx.update(|cx| {
        assert!(live.workspace.read(cx).selection.schema_tab);
    });
    snap(&mut live.cx, live.handle, "47-resource-declaration");
    live.ui(|window, cx| window.click("tab-detail", cx));
    live.cx.update(|cx| {
        assert!(!live.workspace.read(cx).selection.schema_tab);
    });
}

/// A recorded call cannot be replayed without a session, and the row says
/// why instead of ignoring the press.
pub fn replay_needs_session_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("echo", json!({"text": "again"}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.show(Mode::History);
    live.wait("the call is listed", |s| s.count(Mode::History) == Some(1));
    live.select(Mode::History, "echo");
    live.ui(|window, _| assert!(window.try_find("replay-why").is_none()));
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.disconnect(0, cx)));
    live.wait("the session ends", |s| {
        s.servers[0].status != Status::Connected
    });
    live.ui(|window, cx| {
        assert!(window.try_find("replay-why").is_some(), "says why");
        window.click("replay", cx);
    });
    live.cx.update(|cx| {
        assert!(!live.state.read(cx).response_pending(), "nothing sent");
    });
    snap(&mut live.cx, live.handle, "48-replay-needs-session");
}

/// The drawer's level dropdown hides server log messages below the chosen
/// level, those already shown included, keeps rows without a level, and asks
/// the server to send from that level.
pub fn log_level_filter_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("log", json!({"level": "info", "message": "chatty"}));
    assert_eq!(live.text(), "logged=true");
    live.call("log", json!({"level": "error", "message": "disk full"}));
    assert_eq!(live.text(), "logged=true");
    // A log message the server sent: the call that asked for it carries the
    // same text but has no level.
    let shown = |s: &AppState, text: &str| {
        let log = s.servers[0].log();
        s.visible_log()
            .iter()
            .any(|&ix| log[ix].level.is_some() && log[ix].body.contains(text))
    };
    live.wait("both messages are logged", move |s| {
        shown(s, "chatty") && shown(s, "disk full")
    });
    live.ui(|window, cx| window.click("log-header", cx));
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.choose_log_level(Some("error"), cx))
    });
    live.wait("the server is asked", |s| {
        s.servers[0].log_level.as_deref() == Some("error")
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert!(shown(s, "disk full"), "at the level");
        assert!(!shown(s, "chatty"), "below it, although already logged");
        let log = s.servers[0].log();
        assert!(
            s.visible_log()
                .iter()
                .any(|&ix| log[ix].method == "tools/call"),
            "rows without a level stay"
        );
    });
    assert!(live.logged("logging/setLevel"));
    snap(&mut live.cx, live.handle, "53-log-level-filter");
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.choose_log_level(None, cx)));
    live.wait("every level again", |s| {
        s.log_min_level.is_none() && s.servers[0].log_level.as_deref() == Some("debug")
    });
    live.cx
        .update(|cx| assert!(shown(live.state.read(cx), "chatty")));

    // The dropdown opens from the drawer header and closes again.
    live.ui(|window, cx| window.click("log-level", cx));
    // Past the menu's fade-in, so the picture shows it as it settles.
    std::thread::sleep(std::time::Duration::from_millis(300));
    live.ui(|_, _| {});
    snap(&mut live.cx, live.handle, "55-log-level-menu");
    live.ui(|window, cx| window.click("log-level", cx));
}

/// A plain-text result is a read-only text area, whatever its length, and
/// on its own it fills the response panel.
pub fn plain_text_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("text", json!({"kilobytes": 8}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    let lines = live.text().lines().count();
    assert!(
        lines > 40,
        "{lines} lines, enough to need a scroll of its own"
    );
    let area = live.cx.update(|cx| {
        let (server, mode, name) = live.state.read(cx).response_key().unwrap();
        format!("resp:{server}:{mode:?}:{name}c0txt")
    });
    live.ui(|window, _| {
        assert!(window.try_find(area.clone()).is_some(), "the text area");
        assert!(
            window.try_find("response-body").is_some(),
            "under the response header"
        );
    });
    snap(&mut live.cx, live.handle, "63-text-result");
}

/// The zoomed drawer takes the columns' place until it is restored; hiding
/// it drops the zoom; and Esc leaves the drawer as it is.
pub fn log_zoom_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("echo", json!({"text": "read me in full"}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);

    // ⌘⇧J on a hidden drawer shows it zoomed in one step.
    live.ui(|window, cx| window.press("cmd-shift-j", cx));
    live.wait("zoomed", |s| s.drawer_open && s.drawer_zoomed);
    live.ui(|window, _| {
        assert!(window.try_find("log-zoomed").is_some());
        assert!(
            window.try_find("add-server").is_none(),
            "the columns are gone"
        );
    });
    snap(&mut live.cx, live.handle, "61-log-zoomed");

    // Esc closes nothing here.
    live.ui(|window, cx| window.press("escape", cx));
    live.ui(|window, _| assert!(window.try_find("log-zoomed").is_some()));
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert!(s.drawer_open && s.drawer_zoomed, "Esc leaves the drawer");
    });

    // The header's button restores the columns and keeps the drawer open.
    live.ui(|window, cx| window.click("zoom-log", cx));
    live.wait("restored", |s| s.drawer_open && !s.drawer_zoomed);
    live.ui(|window, _| {
        assert!(window.try_find("add-server").is_some());
        assert!(window.try_find("clear-log").is_some(), "still open");
    });

    // Zoomed again, hiding the drawer drops the zoom, so ⌘J brings back the
    // drawer at its height, not the whole window.
    live.ui(|window, cx| window.click("zoom-log", cx));
    live.wait("zoomed again", |s| s.drawer_zoomed);
    live.ui(|window, cx| window.click("toggle-log", cx));
    live.wait("hidden", |s| !s.drawer_open && !s.drawer_zoomed);
    // Collapsed, the header keeps its two buttons: the toggle shows the
    // drawer again at its height.
    live.ui(|window, _| {
        assert!(window.try_find("zoom-log").is_some());
        assert!(window.try_find("toggle-log").is_some());
        assert!(window.try_find("clear-log").is_none(), "Clear needs rows");
    });
    live.ui(|window, cx| window.click("toggle-log", cx));
    live.wait("shown at its height", |s| s.drawer_open && !s.drawer_zoomed);
    live.ui(|window, _| assert!(window.try_find("add-server").is_some()));
}

/// Double-clicking a server disconnects it when connected and connects it
/// again when off.
pub fn double_click_server_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.ui(|window, cx| window.double_click(("server", 0usize), cx));
    live.wait("it disconnects", |s| s.servers[0].status == Status::Off);
    live.cx.update(|cx| {
        assert_eq!(
            live.state.read(cx).selected_server,
            Some(0),
            "and stays selected"
        );
    });
    live.ui(|window, cx| window.double_click(("server", 0usize), cx));
    live.wait("it connects again", |s| {
        s.servers[0].status == Status::Connected
    });
    // A single click only selects.
    live.ui(|window, cx| window.click(("server", 0usize), cx));
    live.cx.update(|cx| {
        assert_eq!(live.state.read(cx).servers[0].status, Status::Connected);
    });
}
