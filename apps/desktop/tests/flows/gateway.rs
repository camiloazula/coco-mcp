//! Headless flow for the gateway: an agent reaches the mock server through
//! it, what it sends and gets back lands in the server's log under its name,
//! and what is changed in Settings reaches the running gateway.

use coco_mcp::bridge::Bridge;
use coco_mcp::gateway::Status as Gateway;
use coco_mcp::state::Status;
use serde_json::{Value, json};

use crate::protocol::{Live, live};

/// What an agent got back from the gateway.
struct Answer {
    status: u16,
    session: Option<String>,
    body: String,
}

/// One request from an agent, made on the runtime the gateway runs on.
fn call(
    bridge: &Bridge,
    port: u16,
    token: &str,
    method: reqwest::Method,
    path: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> Answer {
    let (token, path, session) = (
        token.to_owned(),
        path.to_owned(),
        session.map(str::to_owned),
    );
    futures::executor::block_on(bridge.run(async move {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut request = client
            .request(method, format!("http://127.0.0.1:{port}/{path}"))
            .bearer_auth(token)
            .header("accept", "application/json, text/event-stream");
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        Answer {
            status: response.status().as_u16(),
            session: response
                .headers()
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
            body: response.text().await.unwrap(),
        }
    }))
    .unwrap()
}

fn agent_rows(live: &mut Live) -> Vec<(String, String)> {
    live.cx.update(|cx| {
        live.state.read(cx).servers[0]
            .log()
            .iter()
            .filter_map(|row| Some((row.agent.clone()?, row.method.clone())))
            .collect()
    })
}

/// A request an agent makes, and what the log says about it.
pub fn gateway_flow() {
    let mut live = live(&["--schema", "v1"], None);
    // The agent's side of the conversation runs on a runtime of its own.
    let bridge = Bridge::new().unwrap();
    let port = {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        free.local_addr().unwrap().port()
    };
    let id = live
        .cx
        .update(|cx| live.state.read(cx).servers[0].record.id.clone());

    // A port below the unprivileged range is refused, and nothing starts.
    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            assert!(s.gateway_set_port(80, cx).is_err());
            assert_eq!(s.gateway.status, Gateway::Off);
            s.gateway_set_port(port, cx).unwrap();
            s.gateway_set_on(true, cx);
        })
    });
    live.wait("the gateway listens", |s| {
        matches!(s.gateway.status, Gateway::Running(_))
    });
    let token = live
        .cx
        .update(|cx| live.state.read(cx).gateway.token().map(str::to_owned))
        .expect("the gateway made its token");
    assert!(token.starts_with("coco_"));

    // Nobody gets in without it.
    let turned_away = call(
        &bridge,
        port,
        "wrong",
        reqwest::Method::POST,
        "mock",
        None,
        None,
    );
    assert_eq!(turned_away.status, 401);

    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "flow-agent", "version": "1.0"}}});
    let answer = call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "mock",
        None,
        Some(initialize),
    );
    assert_eq!(answer.status, 200);
    assert!(answer.body.contains("mcp-mockserver"), "{}", answer.body);
    let session = answer.session.expect("a session id");

    live.wait("the agent is known by name", |s| {
        s.gateway.agents.values().any(|a| a.name == "flow-agent")
    });
    let echo = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "echo", "arguments": {"text": "through the gateway"}}});
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let sent = call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "mock",
        Some(&session),
        Some(note),
    );
    assert_eq!(sent.status, 202);
    let echoed = call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "mock",
        Some(&session),
        Some(echo),
    );
    assert!(
        echoed.body.contains("through the gateway"),
        "{}",
        echoed.body
    );

    live.wait("the traffic is in the log", |s| {
        let rows = s.servers[0].log();
        rows.iter().any(|r| {
            r.agent.is_some() && r.method == "tools/call" && r.dir == coco_mcp::state::Dir::Out
        }) && rows
            .iter()
            .any(|r| r.agent.is_some() && r.body.contains("through the gateway"))
    });
    assert!(
        live.cx
            .update(|cx| live.state.read(cx).servers[0].log_has_agents())
    );
    // Coco's own connection to the server is untouched.
    assert_eq!(
        live.cx
            .update(|cx| live.state.read(cx).servers[0].status.clone()),
        Status::Connected
    );
    assert!(
        agent_rows(&mut live)
            .iter()
            .all(|(agent, _)| agent == "flow-agent")
    );

    // The call is in History under the agent's name, as the server was asked
    // it, and replaying it from there is Coco's own call.
    live.wait("the agent's call is in History", |s| {
        s.servers[0]
            .history()
            .iter()
            .any(|c| c.name == "echo" && c.agent.as_deref() == Some("flow-agent"))
    });
    let record = live.cx.update(|cx| {
        live.state.read(cx).servers[0]
            .history()
            .iter()
            .find(|c| c.agent.is_some())
            .cloned()
            .unwrap()
    });
    assert_eq!(record.args, json!({"text": "through the gateway"}));
    assert_eq!(record.status, mcp_store::CallStatus::Ok);
    assert!(
        record
            .result
            .as_ref()
            .is_some_and(|r| r.to_string().contains("through the gateway"))
    );
    let before = live
        .cx
        .update(|cx| live.state.read(cx).servers[0].history().len());
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.replay(&record, cx)));
    live.wait("the replay is recorded as Coco's own call", |s| {
        s.servers[0].history().len() == before + 1
            && s.servers[0].history()[0].agent.is_none()
            && s.servers[0].history()[0].name == "echo"
    });

    // A resource read, a prompt and a read the server refuses are calls too,
    // recorded as Coco records its own: the URI as a read's arguments, and a
    // refusal as a failure with its code and message.
    let read = json!({"jsonrpc": "2.0", "id": 10, "method": "resources/read",
        "params": {"uri": "mock://text/hello"}});
    call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "mock",
        Some(&session),
        Some(read),
    );
    let prompt = json!({"jsonrpc": "2.0", "id": 11, "method": "prompts/get",
        "params": {"name": "greet", "arguments": {"name": "Ada"}}});
    call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "mock",
        Some(&session),
        Some(prompt),
    );
    let missing = json!({"jsonrpc": "2.0", "id": 12, "method": "resources/read",
        "params": {"uri": "mock://nope"}});
    call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "mock",
        Some(&session),
        Some(missing),
    );
    live.wait("the read, the prompt and the refusal are in History", |s| {
        let by_agent = |name: &str| {
            s.servers[0]
                .history()
                .iter()
                .any(|c| c.name == name && c.agent.as_deref() == Some("flow-agent"))
        };
        by_agent("mock://text/hello") && by_agent("greet") && by_agent("mock://nope")
    });
    live.cx.update(|cx| {
        let state = live.state.read(cx);
        let history = state.servers[0].history();
        let find = |name: &str| {
            history
                .iter()
                .find(|c| c.name == name && c.agent.is_some())
                .unwrap()
        };
        let hello = find("mock://text/hello");
        assert_eq!(hello.kind, mcp_store::CallKind::Resource);
        assert_eq!(hello.args, json!("mock://text/hello"));
        assert_eq!(hello.status, mcp_store::CallStatus::Ok);
        let greet = find("greet");
        assert_eq!(greet.kind, mcp_store::CallKind::Prompt);
        assert_eq!(greet.args, json!({"name": "Ada"}));
        let refused = find("mock://nope");
        assert_eq!(refused.status, mcp_store::CallStatus::Failed);
        assert!(
            refused
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("server error -")),
            "{:?}",
            refused.error
        );
        // Not a call: the agent's handshake and its notifications.
        assert!(history.iter().all(|c| c.name != "initialize"));
    });

    // The same agent through the root: every server as one, names in front.
    let root_hello = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "root-agent", "version": "2.0"}}});
    let root = call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "",
        None,
        Some(root_hello),
    );
    assert_eq!(root.status, 200, "{}", root.body);
    assert!(root.body.contains("coco-mcp-gateway"), "{}", root.body);
    let root_session = root.session.expect("a root session");
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let tools = call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "",
        Some(&root_session),
        Some(list),
    );
    assert!(tools.body.contains("mock__echo"), "{}", tools.body);
    let named = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "mock__echo", "arguments": {"text": "through the root"}}});
    let echoed = call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "",
        Some(&root_session),
        Some(named),
    );
    assert!(echoed.body.contains("through the root"), "{}", echoed.body);
    live.wait("the root agent is in the log under its name", |s| {
        let rows = s.servers[0].log();
        rows.iter()
            .any(|r| r.agent.as_deref() == Some("root-agent") && r.method == "tools/call")
    });
    live.wait(
        "the root agent's call is in History as the server knows it",
        |s| {
            s.servers[0]
                .history()
                .iter()
                .any(|c| c.name == "echo" && c.agent.as_deref() == Some("root-agent"))
        },
    );
    // Two connections, whatever the number of servers each reaches.
    assert_eq!(
        live.cx
            .update(|cx| live.state.read(cx).gateway.agent_count()),
        2
    );
    let ended = call(
        &bridge,
        port,
        &token,
        reqwest::Method::DELETE,
        "",
        Some(&root_session),
        None,
    );
    assert_eq!(ended.status, 204);
    live.wait("the root agent is gone", |s| s.gateway.agent_count() == 1);

    // The agent leaves.
    let ended = call(
        &bridge,
        port,
        &token,
        reqwest::Method::DELETE,
        "mock",
        Some(&session),
        None,
    );
    assert_eq!(ended.status, 204);
    live.wait("the agent is gone", |s| s.gateway.agents.is_empty());
    assert!(
        agent_rows(&mut live)
            .iter()
            .any(|(_, method)| method == "relay disconnected")
    );

    // A path is renamed while the gateway runs: the old one stops answering.
    live.cx.update(|cx| {
        live.state.update(cx, |s, cx| {
            assert!(
                s.gateway_set_server(&id, None, Some("not a path!"), cx)
                    .is_err()
            );
            s.gateway_set_server(&id, None, Some("renamed"), cx)
                .unwrap();
        })
    });
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "flow-agent", "version": "1.0"}}});
    let mut renamed = 0;
    for _ in 0..80 {
        renamed = call(
            &bridge,
            port,
            &token,
            reqwest::Method::POST,
            "renamed",
            None,
            Some(ping.clone()),
        )
        .status;
        if renamed == 200 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(renamed, 200, "the new path answers");
    let old = call(
        &bridge,
        port,
        &token,
        reqwest::Method::POST,
        "mock",
        None,
        Some(ping),
    );
    assert_eq!(old.status, 404, "the old one does not");

    // The token is replaced while an agent is connected (the one that just
    // initialized on the new path): the gateway comes back on the same port,
    // and only the new token gets in.
    let old_token = token.clone();
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.gateway_regenerate_token(cx))
    });
    live.wait("the gateway restarts with a new token", |s| {
        matches!(s.gateway.status, Gateway::Running(_))
            && s.gateway.token().is_some_and(|t| t != old_token)
    });
    let new_token = live
        .cx
        .update(|cx| live.state.read(cx).gateway.token().map(str::to_owned))
        .unwrap();
    let hello = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "flow-agent", "version": "1.0"}}});
    let stale = call(
        &bridge,
        port,
        &old_token,
        reqwest::Method::POST,
        "renamed",
        None,
        Some(hello.clone()),
    );
    assert_eq!(stale.status, 401, "the old token is turned away");
    let fresh = call(
        &bridge,
        port,
        &new_token,
        reqwest::Method::POST,
        "renamed",
        None,
        Some(hello),
    );
    assert_eq!(fresh.status, 200, "{}", fresh.body);
    let status = live
        .cx
        .update(|cx| live.state.read(cx).gateway.status.clone());
    assert!(
        matches!(status, Gateway::Running(addr) if addr.port() == port),
        "{status:?}"
    );

    // Off, and nothing is listening.
    live.cx
        .update(|cx| live.state.update(cx, |s, cx| s.gateway_set_on(false, cx)));
    assert_eq!(
        live.cx
            .update(|cx| live.state.read(cx).gateway.status.clone()),
        Gateway::Off
    );
    let refused = futures::executor::block_on(bridge.run(async move {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for _ in 0..80 {
            if client
                .get(format!("http://127.0.0.1:{port}/mock"))
                .send()
                .await
                .is_err()
            {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        false
    }));
    assert_eq!(refused, Some(true), "the port is closed");
}
