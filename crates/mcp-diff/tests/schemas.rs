//! The mock server's two schemas exercise every severity, as documented in
//! `mcp-mockserver`'s module docs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use mcp_core::{ServerSpec, Session, SessionOptions};
use mcp_diff::{ItemKind, Outcome, Severity, diff};
use mcp_mockserver::{MockServer, Schema};

async fn snapshot(schema: Schema) -> mcp_core::Snapshot {
    let spec = ServerSpec::Stdio {
        command: "in-process".into(),
        args: vec![],
        env: Default::default(),
        cwd: None,
    };
    let session = Session::connect_with_transport(
        spec,
        MockServer::serve_duplex(schema),
        SessionOptions::default(),
    )
    .await
    .unwrap();
    session.snapshot().await.unwrap()
}

#[tokio::test]
async fn v2_changes_are_classified() {
    let a = snapshot(Schema::V1).await;
    let b = snapshot(Schema::V2).await;
    let d = diff(Some(&a), &b);
    assert!(matches!(d.outcome, Outcome::Changed { .. }));
    let find = |name: &str, path: &str| {
        d.changes()
            .iter()
            .find(|c| c.name == name && c.path == path)
            .unwrap_or_else(|| panic!("no change for {name} {path}: {:?}", d.changes()))
    };
    assert_eq!(find("echo", "").severity, Severity::Breaking);
    assert_eq!(find("multiply", "").severity, Severity::Compatible);
    assert_eq!(
        find("add", "inputSchema.properties.precision").severity,
        Severity::Breaking
    );
    assert_eq!(find("sleep", "description").severity, Severity::Cosmetic);
    assert!(
        d.changes()
            .iter()
            .all(|c| c.kind == ItemKind::Tool || c.kind == ItemKind::Server)
    );
    assert!(d.has_breaking());
    assert!(d.summary().starts_with("2 breaking"), "{}", d.summary());

    // Same schema twice is unchanged; first contact is First.
    assert_eq!(
        diff(Some(&a), &snapshot(Schema::V1).await).outcome,
        Outcome::Unchanged
    );
    assert_eq!(diff(None, &a).outcome, Outcome::First);

    // Going back from B to A: precision removed is compatible for callers (add
    // still accepts the old shape), multiply removed is breaking, echo added is compatible.
    let back = diff(Some(&b), &a);
    let find_back = |name: &str, path: &str| {
        back.changes()
            .iter()
            .find(|c| c.name == name && c.path == path)
            .unwrap()
    };
    assert_eq!(find_back("multiply", "").severity, Severity::Breaking);
    assert_eq!(find_back("echo", "").severity, Severity::Compatible);
    assert_eq!(
        find_back("add", "inputSchema.properties.precision").severity,
        Severity::Compatible
    );
}
