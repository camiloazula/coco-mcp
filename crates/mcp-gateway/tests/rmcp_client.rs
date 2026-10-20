//! rmcp's streamable HTTP client reaches the gateway: what an agent built on
//! it does, with the token alone in `auth_header` (rmcp adds `Bearer `
//! itself), at the root and on a server's path, and what it gets without
//! the token or with the prefix given twice.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp_gateway::{
    Connector, Gateway, Opened, RelayServer, Silent, StderrSink, UpstreamError, stdio_over_streams,
};
use mcp_mockserver::{MockServer, Schema};
use rmcp::ServiceExt as _;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

const TOKEN: &str = "coco_test_token";

/// The mock server, spoken to over a pair of streams as a process is.
struct MockOverStreams(Schema);

#[async_trait]
impl Connector for MockOverStreams {
    async fn open(&self, _: StderrSink) -> Result<Opened, UpstreamError> {
        let (reader, writer) = tokio::io::split(MockServer::serve_duplex(self.0));
        Ok(stdio_over_streams(reader, writer, None))
    }
}

async fn gateway() -> Gateway {
    let servers = vec![RelayServer {
        namespace: "mock".into(),
        connector: Arc::new(MockOverStreams(Schema::V1)),
    }];
    Gateway::start(0, TOKEN.into(), servers, Arc::new(Silent))
        .await
        .unwrap()
}

/// What an agent does: initialize, list every tool, end the session. The
/// error is the text of whichever step failed.
async fn tool_names(url: &str, auth_header: Option<&str>) -> Result<Vec<String>, String> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
    if let Some(token) = auth_header {
        config = config.auth_header(token.to_owned());
    }
    let transport = StreamableHttpClientTransport::from_config(config);
    let attempt = async {
        let client = ().serve(transport).await.map_err(|e| format!("serve: {e}"))?;
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| format!("list tools: {e}"))?;
        client.cancel().await.map_err(|e| format!("cancel: {e}"))?;
        Ok(tools.into_iter().map(|t| t.name.to_string()).collect())
    };
    tokio::time::timeout(Duration::from_secs(15), attempt)
        .await
        .map_err(|_| format!("timed out against {url}"))?
}

#[tokio::test]
async fn with_the_token_alone_it_lists_the_tools_at_the_root_without_a_slash() {
    let g = gateway().await;
    let url = format!("http://localhost:{}", g.addr().port());
    let names = tool_names(&url, Some(TOKEN)).await.unwrap();
    assert!(!names.is_empty());
    assert!(names.contains(&"mock__echo".to_owned()), "{names:?}");
    assert!(names.iter().all(|n| n.starts_with("mock__")), "{names:?}");
    g.stop().await;
}

#[tokio::test]
async fn with_the_token_alone_it_lists_the_tools_at_the_root_with_a_slash() {
    let g = gateway().await;
    let url = format!("http://127.0.0.1:{}/", g.addr().port());
    let names = tool_names(&url, Some(TOKEN)).await.unwrap();
    assert!(!names.is_empty());
    assert!(names.contains(&"mock__echo".to_owned()), "{names:?}");
    g.stop().await;
}

#[tokio::test]
async fn with_the_token_alone_it_lists_the_servers_own_tools_on_its_path() {
    let g = gateway().await;
    let url = format!("http://localhost:{}/mock", g.addr().port());
    let names = tool_names(&url, Some(TOKEN)).await.unwrap();
    assert!(names.contains(&"echo".to_owned()), "{names:?}");
    assert!(names.iter().all(|n| !n.starts_with("mock__")), "{names:?}");
    g.stop().await;
}

#[tokio::test]
async fn the_root_and_the_path_offer_the_same_tools() {
    let g = gateway().await;
    let port = g.addr().port();
    let root = tool_names(&format!("http://localhost:{port}"), Some(TOKEN))
        .await
        .unwrap();
    let path = tool_names(&format!("http://localhost:{port}/mock"), Some(TOKEN))
        .await
        .unwrap();
    let mut prefixed: Vec<String> = path.iter().map(|n| format!("mock__{n}")).collect();
    let mut root = root;
    prefixed.sort();
    root.sort();
    assert_eq!(root, prefixed);
    g.stop().await;
}

#[tokio::test]
async fn without_the_token_it_is_turned_away() {
    let g = gateway().await;
    let port = g.addr().port();
    for url in [
        format!("http://localhost:{port}"),
        format!("http://localhost:{port}/mock"),
    ] {
        let error = tool_names(&url, None).await.unwrap_err();
        assert!(error.contains("Auth required"), "{url}: {error}");
    }
    g.stop().await;
}

/// `auth_header` takes the bare token and adds `Bearer ` itself, so passing
/// it with the prefix sends `Bearer Bearer …`: what an agent got wrong.
#[tokio::test]
async fn the_prefix_given_twice_is_turned_away_too() {
    let g = gateway().await;
    let url = format!("http://localhost:{}", g.addr().port());
    let error = tool_names(&url, Some(&format!("Bearer {TOKEN}")))
        .await
        .unwrap_err();
    assert!(error.contains("Auth required"), "{error}");
    g.stop().await;
}
