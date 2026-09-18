//! Runs the hermetic mock MCP server, over stdio or streamable HTTP.
//!
//! In stdio mode nothing is written to stdout except JSON-RPC; in HTTP mode
//! the endpoint URL is printed to stderr.

#![forbid(unsafe_code)]
#![allow(clippy::print_stderr, clippy::print_stdout)]

use mcp_mockserver::{Faults, MockServer, Schema};

const USAGE: &str = "\
Hermetic mock MCP server, for testing clients.

Usage: mcp-mockserver [OPTIONS]

Transport:
      --stdio            Serve over stdio (default)
      --http <ADDR>      Serve streamable HTTP on ADDR, e.g. 127.0.0.1:3555

HTTP authorization (with --http):
      --bearer <TOKEN>   Require this bearer token
      --oauth            Require OAuth 2.1, with a mock authorization server

Faults:
      --no-templates     Answer resources/templates/list with method-not-found
      --broken-resources Answer resources/list with an internal error
      --stalled-resources
                         Never answer resources/list
      --ignore-pings     Never answer ping, as a server that stopped
                         responding with its transport still open
      --paged-resources  Answer resources/list two at a time with a cursor
      --legacy-only      Refuse server/discover and 2026-07-28, as a server
                         that only knows the initialize handshake
      --modern-only      Support only 2026-07-28, refusing the handshake
      --legacy-versions  Answer server/discover, supporting only versions up
                         to 2025-11-25
                         Also read from MCP_MOCK_FAULTS, comma-separated,
                         e.g. no-templates,broken-resources,paged-resources.

Options:
  -s, --schema <SCHEMA>  Which release of the fictional server to serve:
                         v1 (default) or v2, a later release whose tools
                         differ so snapshot diffs have something to classify.
                         Also read from MCP_MOCK_SCHEMA.
  -h, --help             Print this message
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut schema = Schema::from_env();
    let mut http: Option<String> = None;
    let mut auth = mcp_mockserver::http::HttpAuth::None;
    let mut faults = Faults::from_env();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--stdio" => http = None,
            "--http" => {
                http = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--http needs an address"))?,
                );
            }
            "--bearer" => {
                auth = mcp_mockserver::http::HttpAuth::Bearer(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--bearer needs a token"))?,
                );
            }
            "--oauth" => auth = mcp_mockserver::http::HttpAuth::OAuth { expires_in: 3600 },
            "--no-templates" => faults.no_templates = true,
            "--broken-resources" => faults.broken_resources = true,
            "--stalled-resources" => faults.stalled_resources = true,
            "--ignore-pings" => faults.ignore_pings = true,
            "--paged-resources" => faults.paged_resources = true,
            "--legacy-only" => faults.legacy_only = true,
            "--modern-only" => faults.modern_only = true,
            "--legacy-versions" => faults.legacy_versions = true,
            "--schema" | "-s" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--schema needs a value"))?;
                schema = parse_schema(&value)?;
            }
            other => match other.strip_prefix("--schema=") {
                Some(value) => schema = parse_schema(value)?,
                None => anyhow::bail!("unknown argument `{other}`; try --help"),
            },
        }
    }
    match http {
        Some(addr) => {
            let server = mcp_mockserver::http::serve_http_with(schema, faults, auth, &addr).await?;
            eprintln!("mcp-mockserver listening on {}", server.url);
            tokio::signal::ctrl_c().await?;
            server.shutdown();
            Ok(())
        }
        None => MockServer::serve_stdio(schema, faults).await,
    }
}

fn parse_schema(value: &str) -> anyhow::Result<Schema> {
    Schema::parse(value)
        .ok_or_else(|| anyhow::anyhow!("unknown schema `{value}` (expected v1 or v2)"))
}
