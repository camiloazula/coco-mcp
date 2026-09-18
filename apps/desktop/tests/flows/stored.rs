//! Headless flows for what the app keeps about a server: clearing its call
//! history, forgetting its credentials, and reading its stored snapshots
//! back, compared with each other or with a snapshot file.

use coco_mcp::calls::ResponseStatus;
use coco_mcp::state::{AppState, Confirm, Mode, Status};
use gpui_kit::test::TestWindowExt as _;
use mcp_core::ServerSpec;
use serde_json::json;

use crate::macos::snap;
use crate::protocol::{live, open};

/// The bin on a call deletes that call alone, at once; the bin above the
/// list asks first, then empties the list and the database.
pub fn clear_history_flow() {
    let mut live = live(&["--schema", "v1"], None);
    live.call("echo", json!({"text": "deleted alone"}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.call("echo", json!({"text": "kept until cleared"}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    let id = live
        .cx
        .update(|cx| live.state.read(cx).servers[0].record.id.clone());
    assert_eq!(live.store.list_calls(Some(&id), 10).unwrap().len(), 2);
    live.show(Mode::History);
    live.wait("both calls are listed", |s| {
        s.count(Mode::History) == Some(2)
    });
    // The older call is the second row.
    live.ui(|window, cx| window.click(("item", 1usize), cx));
    live.wait("the older call is selected", |s| {
        s.selected_call()
            .is_some_and(|c| c.args["text"] == "deleted alone")
    });
    live.ui(|window, cx| window.click("delete-call", cx));
    live.wait("only the newer call is left", |s| {
        s.count(Mode::History) == Some(1) && s.selected_call().is_none()
    });
    let left = live.store.list_calls(Some(&id), 10).unwrap();
    assert_eq!(left.len(), 1, "its row is gone from the database");
    assert_eq!(left[0].args["text"], "kept until cleared");
    live.select(Mode::History, "echo");
    live.ui(|window, cx| window.click("clear-history", cx));
    live.ui(|window, _| {
        assert!(window.try_find("confirm-dialog").is_some(), "asks first");
    });
    snap(&mut live.cx, live.handle, "43-clear-history");
    live.ui(|window, cx| window.click("confirm-accept", cx));
    live.wait("the history is empty", |s| {
        s.count(Mode::History) == Some(0) && s.confirm.is_none()
    });
    assert!(
        live.store.list_calls(Some(&id), 10).unwrap().is_empty(),
        "and its rows are gone"
    );
}

/// Forget Credentials asks first, removes the stored token and keeps the
/// server.
pub fn forget_credentials_flow() {
    use mcp_auth::SecretStore as _;
    let secrets = std::sync::Arc::new(mcp_auth::MemoryStore::new());
    secrets.set("remote-token", "s3cret").unwrap();
    let mut model = AppState::new(None, None);
    model.secrets = secrets.clone();
    let spec = ServerSpec::Http {
        url: "https://remote.test/mcp".into(),
        headers: Default::default(),
        auth: mcp_core::AuthRef::Bearer {
            keyring_id: "remote-token".into(),
        },
    };
    model.add_demo_server("remote", spec, Status::Off);
    let mut live = open(model, mcp_store::Store::open_in_memory().unwrap());
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.request_forget_credentials(cx))
    });
    live.ui(|window, _| assert!(window.try_find("confirm-dialog").is_some()));
    live.cx.update(|cx| {
        assert_eq!(
            live.state.read(cx).confirm,
            Some(Confirm::ForgetCredentials(0))
        );
    });
    live.ui(|window, cx| window.click("confirm-accept", cx));
    live.wait("the dialog closes", |s| s.confirm.is_none());
    assert_eq!(secrets.get("remote-token").unwrap(), None, "token removed");
    live.cx.update(|cx| {
        assert_eq!(live.state.read(cx).servers.len(), 1, "the server stays");
    });
}

/// The Server view lists the stored snapshots, compares the two newest, and
/// compares whichever two are picked.
pub fn snapshot_history_flow() {
    let mut live = live(&["--schema", "v1"], None);
    let (id, mut newer) = live.cx.update(|cx| {
        let entry = &live.state.read(cx).servers[0];
        (entry.record.id.clone(), entry.snapshot().cloned().unwrap())
    });
    // The connect stored the first snapshot; a later one lost `echo`.
    newer.tools.retain(|tool| tool.name != "echo");
    newer.taken_at += time::Duration::minutes(1);
    live.store.add_snapshot(&id, &newer).unwrap();
    live.show(Mode::Server);
    live.wait("the two newest are compared", |s| {
        matches!(&s.servers[0].snapshot_diff, Some(Ok(_)))
    });
    live.cx.update(|cx| {
        let entry = &live.state.read(cx).servers[0];
        assert_eq!(entry.snapshots.as_ref().map(Vec::len), Some(2));
        let diff = entry.snapshot_diff.clone().unwrap().unwrap();
        assert!(diff.breaking >= 1, "{}", diff.summary());
    });
    // The section alone, wherever the sections above it end.
    live.select(Mode::Server, "Snapshots");
    live.ui(|window, _| {
        assert!(window.try_find(("snapshot", 1usize)).is_some());
        assert!(window.try_find("snapshot-summary").is_some());
    });
    snap(&mut live.cx, live.handle, "44-snapshot-history");
    // The newest picked on both sides compares it with itself.
    live.ui(|window, cx| window.click(("snap-from", 0usize), cx));
    live.wait(
        "a snapshot matches itself",
        |s| matches!(&s.servers[0].snapshot_diff, Some(Ok(diff)) if diff.changes().is_empty()),
    );
}

/// A snapshot file used as the baseline shows its diff in the banner, named
/// after the file.
pub fn compare_file_flow() {
    let mut live = live(&["--schema", "v1"], None);
    let mut baseline = live
        .cx
        .update(|cx| live.state.read(cx).servers[0].snapshot().cloned().unwrap());
    baseline.tools.retain(|tool| tool.name != "add");
    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            s.compare_with_snapshot(&baseline, "baseline.json".into(), cx);
        })
    });
    live.ui(|window, _| {
        assert!(
            window.try_find("diff-details").is_some(),
            "the banner shows"
        );
    });
    live.cx.update(|cx| {
        let s = live.state.read(cx);
        assert_eq!(s.servers[0].diff_baseline.as_deref(), Some("baseline.json"));
        assert!(s.visible_diff().is_some_and(|diff| diff.compatible >= 1));
    });
    snap(&mut live.cx, live.handle, "45-compare-file");
}
