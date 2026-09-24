//! Hermetic MCP server for the integration tests and the headless flows,
//! over stdio or streamable HTTP. Nothing shipped links it.
//!
//! Exposes tools, resources (text, JSON, Markdown, PNG, a subscribable
//! counter, a URI template, resources added while it runs), prompts,
//! completions, logging at a level the client sets, progress, and
//! server-initiated requests (elicitation, sampling, roots). It speaks both
//! protocol eras: a 2026-07-28 client is asked for elicitation, sampling and
//! roots inside a round trip, hears changes on its `subscriptions/listen`
//! stream, and sets the log level in each request. Two [`Schema`]s are served: `v1` and the
//! later `v2`, two releases of the same fictional server, so `mcp-diff` can
//! be tested against real server output:
//!
//! | change in `v2` relative to `v1`          | expected classification |
//! |------------------------------------------|-------------------------|
//! | `add` gains a required `precision` field | breaking                |
//! | `echo` is removed                        | breaking                |
//! | `multiply` is added                      | compatible              |
//! | `sleep` description changes              | cosmetic                |
//!
//! [`Faults`] make the server misbehave the way half-implemented or stuck
//! servers do, so a client can prove it keeps a session whose tools still
//! work, and notices one that stopped answering.
//!
//! UI-free.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Sampling and roots are deprecated by SEP-2577; the client must still exercise them.
#![allow(deprecated)]

pub mod http;
pub mod legacy;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::{InputResponses, RequestState};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    CallToolResponse, CallToolResult, CompleteRequestParams, CompleteResult, CompletionInfo,
    ContentBlock, CreateMessageRequestParams, DiscoverRequestMethod, DiscoverResult, ErrorCode,
    ErrorData, Implementation, InputRequest, InputRequests, InputRequiredResult,
    ListResourceTemplatesResult, ListResourcesResult, LoggingLevel,
    LoggingMessageNotificationParam, PaginatedRequestParams, ProgressNotificationParam,
    PromptMessage, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Reference, Resource, ResourceContents, ResourceTemplate,
    ResourceUpdatedNotificationParam, Role, SamplingMessage, ServerCapabilities, ServerInfo,
    SetLevelRequestParams, SubscribeRequestParams, SubscriptionFilter, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, SubscriptionContext, SubscriptionSink};
use rmcp::transport::IntoTransport;
use rmcp::{
    Peer, RoleServer, ServerHandler, ServiceExt, prompt, prompt_handler, prompt_router, tool,
    tool_handler, tool_router,
};

use crate::legacy::RefuseDiscover;
use serde::{Deserialize, Serialize};

/// Server name reported in `initialize`.
pub const SERVER_NAME: &str = "mcp-mockserver";

/// URI of the subscribable counter resource.
pub const COUNTER_URI: &str = "mock://counter";

/// URI prefix of the resources `add_resource` adds.
pub const EXTRA_PREFIX: &str = "mock://extra/";

/// Resources per `resources/list` page with the `paged-resources` fault.
pub const PAGE_SIZE: usize = 2;

/// The keys the round-trip tools name their input requests under.
const ELICIT_KEY: &str = "answer";
const SAMPLE_KEY: &str = "sample";
const ROOTS_KEY: &str = "roots";

/// Which tool list to serve: two versions of the same fictional server, so
/// `mcp-diff` can be exercised against a real upgrade (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Schema {
    /// The server as first published.
    #[default]
    V1,
    /// A later release that breaks, extends and reworks parts of `V1`.
    V2,
}

impl Schema {
    /// Its name on the command line and in `--help`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }

    /// Parse `v1`/`v2`, with a bare `1`/`2` accepted (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "v1" | "1" => Some(Self::V1),
            "v2" | "2" => Some(Self::V2),
            _ => None,
        }
    }

    /// Read `MCP_MOCK_SCHEMA`, defaulting to [`Schema::V1`].
    pub fn from_env() -> Self {
        std::env::var("MCP_MOCK_SCHEMA")
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Tool parameter types
// ---------------------------------------------------------------------------

/// Arguments for `echo`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EchoArgs {
    /// Text to echo back.
    pub text: String,
}

/// Arguments for `add` (schema `v1`).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    /// Left operand.
    pub a: f64,
    /// Right operand.
    pub b: f64,
}

/// Arguments for `add` (schema `v2`): `precision` is new and required.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddArgsV2 {
    /// Left operand.
    pub a: f64,
    /// Right operand.
    pub b: f64,
    /// Decimal places to round the sum to.
    pub precision: u32,
}

/// Structured result of `add` and `multiply`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ArithmeticResult {
    /// The computed value.
    pub value: f64,
}

/// Arguments for `fail`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FailArgs {
    /// Error text to return.
    pub message: String,
}

/// Arguments for `sleep`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SleepArgs {
    /// Milliseconds to wait before answering.
    pub millis: u64,
}

/// A user's role.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// Full access.
    Admin,
    /// Regular access.
    Member,
    /// Read-only.
    Guest,
}

/// A postal address.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Address {
    /// Street and number.
    pub street: String,
    /// City.
    pub city: String,
}

/// A user record.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct User {
    /// Display name.
    pub name: String,
    /// Age in years.
    pub age: Option<u32>,
    /// Role.
    pub role: UserRole,
}

/// Arguments for `complex`: nested objects, arrays, enums, optionals and free JSON.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ComplexArgs {
    /// The user.
    pub user: User,
    /// Free-form tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional address.
    pub address: Option<Address>,
    /// Arbitrary JSON.
    pub metadata: Option<serde_json::Value>,
}

/// A string or a number, whichever the caller sends.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum StringOrNumber {
    /// A string.
    Text(String),
    /// A number.
    Number(i64),
}

/// A shape, told apart by its `kind`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Shape {
    /// A circle.
    Circle {
        /// Its radius.
        radius: f64,
    },
    /// A rectangle.
    Rect {
        /// Its width.
        width: f64,
        /// Its height.
        height: f64,
    },
}

/// Arguments for `kinds`: one field of every type the form draws, and the
/// mixed cases a schema can state.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KindsArgs {
    /// A string.
    pub text: String,
    /// An integer or null, and required: the caller says which.
    #[schemars(required, schema_with = "nullable_integer")]
    pub score: Option<i32>,
    /// A string or null, and null unless set.
    #[serde(default)]
    pub nickname: Option<String>,
    /// A string or an integer.
    pub id: StringOrNumber,
    /// One of several object shapes, told apart by a tag.
    pub shape: Shape,
    /// A short lowercase word: a length, a pattern and a default.
    #[serde(default = "default_word")]
    #[schemars(length(min = 2, max = 8), regex(pattern = r"^[a-z]+$"))]
    pub word: String,
    /// An email address: a format.
    #[schemars(email)]
    pub email: String,
    /// A URL: another format.
    #[schemars(url)]
    pub link: String,
    /// An integer within bounds.
    #[schemars(range(min = 1, max = 10))]
    pub count: u8,
    /// A number.
    pub ratio: f64,
    /// A boolean.
    pub flag: bool,
    /// A choice among constants.
    pub role: UserRole,
    /// A list of strings.
    pub tags: Vec<String>,
    /// A list of objects.
    pub people: Vec<User>,
    /// A map from names to numbers.
    pub scores: HashMap<String, i32>,
    /// A pair: a fixed-length array of mixed types, with a default.
    #[serde(default = "default_pair")]
    pub pair: (String, i32),
    /// A nested object.
    pub address: Address,
    /// Anything: free JSON.
    pub extra: serde_json::Value,
}

/// The schema of [`KindsArgs::score`]: `Option<i32>` would be optional and
/// nullable, and `required` alone would drop the null; this keeps both.
fn nullable_integer(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": ["integer", "null"], "format": "int32"})
}

/// The `word` of [`KindsArgs`] when not given.
fn default_word() -> String {
    "hello".into()
}

/// The `pair` of [`KindsArgs`] when not given.
fn default_pair() -> (String, i32) {
    ("a".into(), 1)
}

/// Arguments for `log`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LogArgs {
    /// One of `debug`, `info`, `warning`, `error`.
    pub level: String,
    /// Message text.
    pub message: String,
    /// How many times to send it, one notification each (default 1, at most
    /// [`MAX_LOG_BURST`]), so a client can be tested against a chatty server.
    #[serde(default)]
    pub count: Option<u32>,
}

/// Most notifications one `log` call sends.
pub const MAX_LOG_BURST: u32 = 1_000;

/// Arguments for `rows`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RowsArgs {
    /// How many rows to return (at most [`MAX_ROWS`]).
    pub count: u32,
}

/// Most rows one `rows` call returns.
pub const MAX_ROWS: u32 = 100_000;

/// Arguments for `text` and `markdown`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProseArgs {
    /// About how many kilobytes to return (default 256, at most
    /// [`MAX_PROSE_KB`]).
    #[serde(default)]
    pub kilobytes: Option<u32>,
}

/// Most kilobytes one `text` or `markdown` call returns.
pub const MAX_PROSE_KB: u32 = 8 * 1024;

/// Kilobytes `text` and `markdown` return when not told how many.
pub const DEFAULT_PROSE_KB: u32 = 256;

/// URI prefix of the large resources: `mock://big/rows.json`,
/// `mock://big/prose.txt` and `mock://big/readme.md` at [`DEFAULT_PROSE_KB`],
/// and the template `mock://big/{kind}/{kilobytes}` for any size of `json`,
/// `text` or `markdown`.
pub const BIG_PREFIX: &str = "mock://big/";

/// The kinds a large answer comes in.
pub const BIG_KINDS: [&str; 3] = ["json", "text", "markdown"];

/// Arguments for the `long` prompt.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LongArgs {
    /// What the one message holds: `json`, `text` or `markdown`.
    pub kind: String,
    /// About how many kilobytes (default 256, at most [`MAX_PROSE_KB`]).
    #[serde(default)]
    pub kilobytes: Option<String>,
}

/// Arguments for `progress`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProgressArgs {
    /// How many steps to report (at most [`MAX_PROGRESS_STEPS`]).
    pub steps: u32,
    /// Milliseconds to wait before each step (default 50, at most ten seconds).
    #[serde(default)]
    pub delay_ms: Option<u64>,
}

/// Most steps one `progress` call reports.
pub const MAX_PROGRESS_STEPS: u32 = 1_000;

/// Arguments for `add_resource`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddResourceArgs {
    /// Name of the resource, listed as `mock://extra/{name}`.
    pub name: String,
}

/// Arguments for `elicit`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ElicitArgs {
    /// Question to ask the user.
    pub question: String,
    /// Milliseconds to wait for the answer before withdrawing the request
    /// with `notifications/cancelled`; waits as long as it takes when absent.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Ask the client to open this URL instead of filling a form.
    #[serde(default)]
    pub url: Option<String>,
    /// On a 2026-07-28 request, ask this many times before answering,
    /// counting in `requestState`, so a client's round limit can be tested.
    #[serde(default)]
    pub repeat: Option<u32>,
}

/// Arguments for `stderr`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StderrArgs {
    /// Lines to write.
    pub lines: Vec<String>,
}

/// Arguments for `exit`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExitArgs {
    /// Exit status of the process.
    pub code: i32,
    /// Milliseconds between the answer and the exit (default 50).
    #[serde(default)]
    pub delay_ms: Option<u64>,
}

/// Form the `elicit` tool asks the client to fill.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ElicitAnswer {
    /// The user's answer.
    pub answer: String,
}
rmcp::elicit_safe!(ElicitAnswer);

/// Arguments for `sample`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SampleArgs {
    /// Prompt sent to the client's model.
    pub prompt: String,
}

/// Arguments for `multiply` (schema `v2` only).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MultiplyArgs {
    /// Left factor.
    pub a: f64,
    /// Right factor.
    pub b: f64,
}

/// Arguments for the `greet` prompt.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GreetArgs {
    /// Who to greet.
    pub name: String,
}

/// Arguments for the `summarize` prompt.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SummarizeArgs {
    /// Text to summarize.
    pub text: String,
    /// Optional style hint (`brief`, `detailed`).
    pub style: Option<String>,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Ways the server misbehaves. The list faults leave the capability that
/// advertises the list declared; tools, prompts and resource reads are
/// unaffected by all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Faults {
    /// `resources/templates/list` answers method-not-found, as from a server
    /// that never implemented templates.
    pub no_templates: bool,
    /// `resources/list` answers an internal error, as from a server whose
    /// resource backend is down.
    pub broken_resources: bool,
    /// `resources/list` never answers, as from a server that hangs on a
    /// backend that stopped responding. `broken_resources` wins when both
    /// are set.
    pub stalled_resources: bool,
    /// `ping` never answers, as from a server that stopped responding while
    /// its transport stayed open. Every other request still answers.
    pub ignore_pings: bool,
    /// `resources/list` answers [`PAGE_SIZE`] resources at a time with a
    /// `nextCursor`, as a server with a long list does.
    pub paged_resources: bool,
    /// `server/discover` answers method-not-found and 2026-07-28 is not
    /// supported, as from a server that only knows the `initialize`
    /// handshake.
    pub legacy_only: bool,
    /// Only 2026-07-28 is supported, so the `initialize` handshake is
    /// refused.
    pub modern_only: bool,
    /// `server/discover` answers, but only versions up to 2025-11-25 are
    /// supported, as from a server that added discovery before the version
    /// that needs it.
    pub legacy_versions: bool,
}

impl Faults {
    /// Parse a comma-separated list of the fault flags without their dashes,
    /// e.g. `no-templates,broken-resources`. `None` for a name it does not
    /// know.
    pub fn parse(s: &str) -> Option<Self> {
        let mut faults = Self::default();
        for name in s.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            match name {
                "no-templates" => faults.no_templates = true,
                "broken-resources" => faults.broken_resources = true,
                "stalled-resources" => faults.stalled_resources = true,
                "ignore-pings" => faults.ignore_pings = true,
                "paged-resources" => faults.paged_resources = true,
                "legacy-only" => faults.legacy_only = true,
                "modern-only" => faults.modern_only = true,
                "legacy-versions" => faults.legacy_versions = true,
                _ => return None,
            }
        }
        Some(faults)
    }

    /// Read `MCP_MOCK_FAULTS`, defaulting to none. Lets a test break a list
    /// without changing the command line a client saves the server under.
    pub fn from_env() -> Self {
        std::env::var("MCP_MOCK_FAULTS")
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or_default()
    }
}

/// The mock server handler.
#[derive(Debug, Clone)]
pub struct MockServer {
    schema: Schema,
    faults: Faults,
    counter: Arc<AtomicU64>,
    subscriptions: Arc<Mutex<BTreeSet<String>>>,
    /// Names `add_resource` added, in order.
    extra_resources: Arc<Mutex<Vec<String>>>,
    /// Rank (see [`level_rank`]) of the lowest level `log` sends to a legacy
    /// client.
    log_floor: Arc<AtomicU64>,
    /// The `subscriptions/listen` streams open on the server, by a number
    /// of their own.
    listeners: Arc<Mutex<Vec<(u64, SubscriptionSink)>>>,
    next_listener: Arc<AtomicU64>,
    /// Serving its own process over stdio, so `exit` may end it.
    standalone: bool,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl MockServer {
    /// Build a server for `schema`.
    pub fn new(schema: Schema) -> Self {
        Self::with_faults(schema, Faults::default())
    }

    /// Build a server for `schema` whose lists misbehave as `faults` says.
    pub fn with_faults(schema: Schema, faults: Faults) -> Self {
        let tool_router = match schema {
            Schema::V1 => Self::common_tools() + Self::v1_tools(),
            Schema::V2 => Self::common_tools() + Self::v2_tools(),
        };
        Self {
            schema,
            faults,
            counter: Arc::new(AtomicU64::new(0)),
            subscriptions: Arc::new(Mutex::new(BTreeSet::new())),
            extra_resources: Arc::new(Mutex::new(Vec::new())),
            log_floor: Arc::new(AtomicU64::new(0)),
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener: Arc::new(AtomicU64::new(0)),
            standalone: false,
            tool_router,
            prompt_router: Self::prompt_router(),
        }
    }

    /// Serve over the process's stdin/stdout until the client disconnects.
    pub async fn serve_stdio(schema: Schema, faults: Faults) -> anyhow::Result<()> {
        let server = Self {
            standalone: true,
            ..Self::with_faults(schema, faults)
        };
        let service = if faults.legacy_only {
            let transport =
                IntoTransport::<RoleServer, _, _>::into_transport(rmcp::transport::stdio());
            server.serve(RefuseDiscover::new(transport)).await?
        } else {
            server.serve(rmcp::transport::stdio()).await?
        };
        service.waiting().await?;
        Ok(())
    }

    /// Serve over an in-process duplex stream; returns the client end.
    pub fn serve_duplex(schema: Schema) -> tokio::io::DuplexStream {
        Self::serve_duplex_with(schema, Faults::default())
    }

    /// [`Self::serve_duplex`] with lists that misbehave as `faults` says.
    pub fn serve_duplex_with(schema: Schema, faults: Faults) -> tokio::io::DuplexStream {
        let (server_side, client_side) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let server = Self::with_faults(schema, faults);
            let started = if faults.legacy_only {
                let transport = IntoTransport::<RoleServer, _, _>::into_transport(server_side);
                server.serve(RefuseDiscover::new(transport)).await
            } else {
                server.serve(server_side).await
            };
            match started {
                Ok(service) => {
                    let _ = service.waiting().await;
                }
                Err(e) => tracing::warn!("mock server failed to start: {e}"),
            }
        });
        client_side
    }

    /// The subscription streams open on the server.
    fn listening(&self) -> Vec<SubscriptionSink> {
        self.listeners
            .lock()
            .map(|open| open.iter().map(|(_, sink)| sink.clone()).collect())
            .unwrap_or_default()
    }

    fn resources(&self) -> Vec<Resource> {
        let mut resources = vec![
            Resource::new("mock://text/hello", "hello.txt")
                .with_title("Hello")
                .with_description("A plain text greeting")
                .with_mime_type("text/plain"),
            Resource::new("mock://json/config", "config.json")
                .with_description("A JSON document")
                .with_mime_type("application/json"),
            Resource::new("mock://md/readme", "README.md")
                .with_description("A Markdown document")
                .with_mime_type("text/markdown"),
            Resource::new("mock://png/pixel", "pixel.png")
                .with_description("A 16x16 checkerboard PNG")
                .with_mime_type("image/png"),
            Resource::new(COUNTER_URI, "counter")
                .with_description("Increments on every `bump` call; subscribable")
                .with_mime_type("text/plain"),
            Resource::new(format!("{BIG_PREFIX}rows.json"), "rows.json")
                .with_description("A large JSON document")
                .with_mime_type("application/json"),
            Resource::new(format!("{BIG_PREFIX}prose.txt"), "prose.txt")
                .with_description("A large plain text")
                .with_mime_type("text/plain"),
            Resource::new(format!("{BIG_PREFIX}readme.md"), "readme.md")
                .with_description("A large Markdown document")
                .with_mime_type("text/markdown"),
        ];
        if let Ok(extra) = self.extra_resources.lock() {
            resources.extend(extra.iter().map(|name| {
                Resource::new(format!("{EXTRA_PREFIX}{name}"), name.clone())
                    .with_description("Added by `add_resource`")
                    .with_mime_type("text/plain")
            }));
        }
        resources
    }

    fn read(&self, uri: &str) -> Option<ResourceContents> {
        // 16x16 checkerboard PNG (accent / background), visible in viewers.
        const PIXEL_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAJElEQVR42mOI378fjsTEJeEIlzjDINRAjCJk8cGoYTQeBoUGALYOEZAweb38AAAAAElFTkSuQmCC";
        match uri {
            "mock://text/hello" => {
                Some(ResourceContents::text("Hello from the mock server.\n", uri))
            }
            "mock://json/config" => Some(ResourceContents::TextResourceContents {
                uri: uri.to_owned(),
                mime_type: Some("application/json".into()),
                text: r#"{"name":"mock","features":["tools","resources","prompts"],"retries":3}"#
                    .into(),
                meta: None,
            }),
            "mock://md/readme" => Some(ResourceContents::TextResourceContents {
                uri: uri.to_owned(),
                mime_type: Some("text/markdown".into()),
                text: "# Mock server\n\nThis is **Markdown** with a list:\n\n- one\n- two\n".into(),
                meta: None,
            }),
            "mock://png/pixel" => Some(ResourceContents::BlobResourceContents {
                uri: uri.to_owned(),
                mime_type: Some("image/png".into()),
                blob: PIXEL_PNG_BASE64.into(),
                meta: None,
            }),
            COUNTER_URI => Some(ResourceContents::text(
                self.counter.load(Ordering::Relaxed).to_string(),
                uri,
            )),
            other => {
                if let Some(id) = other.strip_prefix("mock://item/") {
                    return Some(ResourceContents::text(format!("item {id}"), other));
                }
                if let Some(rest) = other.strip_prefix(BIG_PREFIX) {
                    return big_resource(other, rest);
                }
                let name = other.strip_prefix(EXTRA_PREFIX)?;
                let added = self
                    .extra_resources
                    .lock()
                    .is_ok_and(|extra| extra.iter().any(|n| n == name));
                added.then(|| ResourceContents::text(format!("extra {name}"), other))
            }
        }
    }
}

#[tool_router(router = common_tools, vis = "pub")]
impl MockServer {
    /// Write each of `lines` to the process's stderr, as a server logging
    /// outside the protocol does.
    #[tool]
    fn stderr(&self, Parameters(args): Parameters<StderrArgs>) -> String {
        use std::io::Write as _;
        let mut err = std::io::stderr().lock();
        let written = args
            .lines
            .iter()
            .filter(|line| writeln!(err, "{line}").is_ok())
            .count();
        let _ = err.flush();
        format!("wrote {written} lines")
    }

    /// Answer, then end the process with `code`, as a server that crashes
    /// mid-session. Only a server serving its own process over stdio exits;
    /// one in a test's process refuses.
    #[tool]
    fn exit(&self, Parameters(args): Parameters<ExitArgs>) -> CallToolResult {
        if !self.standalone {
            return CallToolResult::error(vec![ContentBlock::text(
                "exit only ends a server serving its own process over stdio",
            )]);
        }
        let delay = std::time::Duration::from_millis(args.delay_ms.unwrap_or(50).min(10_000));
        let code = args.code;
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            std::process::exit(code);
        });
        CallToolResult::success(vec![ContentBlock::text(format!(
            "exiting with {code} in {}ms",
            delay.as_millis()
        ))])
    }

    /// Return an error result (`isError: true`) with the given message.
    #[tool]
    fn fail(&self, Parameters(args): Parameters<FailArgs>) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(args.message)])
    }

    /// Exercise nested objects, arrays, enums and optional fields.
    #[tool]
    fn complex(&self, Parameters(args): Parameters<ComplexArgs>) -> String {
        format!(
            "user={} role={:?} tags={} address={} metadata={}",
            args.user.name,
            args.user.role,
            args.tags.len(),
            args.address.is_some(),
            args.metadata.is_some()
        )
    }

    /// Take one argument of every type a form can draw, and the mixed cases:
    /// nullable, string or number, tagged shapes, lists, maps, pairs and
    /// free JSON. Answers with what it understood.
    #[tool]
    fn kinds(&self, Parameters(args): Parameters<KindsArgs>) -> String {
        format!(
            "text={} word={} email={} link={} count={} ratio={} flag={} role={:?} nickname={:?} \
             score={:?} id={:?} shape={:?} tags={} people={} scores={} pair=({}, {}) \
             address={} extra={}",
            args.text,
            args.word,
            args.email,
            args.link,
            args.count,
            args.ratio,
            args.flag,
            args.role,
            args.nickname,
            args.score,
            args.id,
            args.shape,
            args.tags.len(),
            args.people.len(),
            args.scores.len(),
            args.pair.0,
            args.pair.1,
            args.address.city,
            args.extra
        )
    }

    /// Increment the counter resource and tell whoever subscribed to it: the
    /// subscription streams open on the server, and a legacy client that
    /// asked with `resources/subscribe`.
    #[tool]
    async fn bump(&self, context: RequestContext<RoleServer>) -> String {
        let value = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        for stream in self.listening() {
            let _ = stream.notify_resource_updated(COUNTER_URI).await;
        }
        let subscribed = self
            .subscriptions
            .lock()
            .map(|s| s.contains(COUNTER_URI))
            .unwrap_or(false);
        if subscribed && !is_modern(&context) {
            let _ = context
                .peer
                .notify_resource_updated(ResourceUpdatedNotificationParam::new(COUNTER_URI))
                .await;
        }
        value.to_string()
    }

    /// Send a `notifications/message` log entry to the client, `count` times.
    #[tool]
    async fn log(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<LogArgs>,
    ) -> String {
        let level = match args.level.as_str() {
            "debug" => LoggingLevel::Debug,
            "warning" | "warn" => LoggingLevel::Warning,
            "error" => LoggingLevel::Error,
            _ => LoggingLevel::Info,
        };
        // Below the level the client asked for: not sent. A legacy client
        // sets it with `logging/setLevel`, a modern one in the request.
        let floor = if is_modern(&context) {
            context.meta.log_level().map_or(0, |l| level_rank(&l)) as u64
        } else {
            self.log_floor.load(Ordering::Relaxed)
        };
        if (level_rank(&level) as u64) < floor {
            return "logged=false".into();
        }
        let count = args.count.unwrap_or(1).clamp(1, MAX_LOG_BURST);
        let mut sent = true;
        for _ in 0..count {
            sent &= context
                .peer
                .notify_logging_message(
                    LoggingMessageNotificationParam::new(
                        level,
                        serde_json::Value::String(args.message.clone()),
                    )
                    .with_logger("mock"),
                )
                .await
                .is_ok();
        }
        format!("logged={sent}")
    }

    /// Return `count` rows as one JSON text block, so a client can be tested
    /// against a result too large to draw at once.
    #[tool]
    fn rows(&self, Parameters(args): Parameters<RowsArgs>) -> String {
        let rows = (0..args.count.min(MAX_ROWS))
            .map(|id| serde_json::json!({"id": id, "ok": true}))
            .collect();
        serde_json::Value::Array(rows).to_string()
    }

    /// Return about `kilobytes` of plain text as one text block, so a client
    /// can be tested against a long answer that is not JSON.
    #[tool]
    fn text(&self, Parameters(args): Parameters<ProseArgs>) -> String {
        prose(prose_bytes(&args))
    }

    /// Return about `kilobytes` of Markdown as an embedded `text/markdown`
    /// resource, so a client can be tested against a long rendered answer.
    #[tool]
    fn markdown(&self, Parameters(args): Parameters<ProseArgs>) -> CallToolResult {
        CallToolResult::success(vec![ContentBlock::resource(
            ResourceContents::TextResourceContents {
                uri: "mock://md/long".into(),
                mime_type: Some("text/markdown".into()),
                text: markdown(prose_bytes(&args)),
                meta: None,
            },
        )])
    }

    /// Report `steps` progress notifications against the request's progress
    /// token, one every `delay_ms`, then answer. Stops early when the client
    /// cancels the request.
    #[tool]
    async fn progress(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<ProgressArgs>,
    ) -> String {
        let steps = args.steps.min(MAX_PROGRESS_STEPS);
        let delay = std::time::Duration::from_millis(args.delay_ms.unwrap_or(50).min(10_000));
        let token = context.meta.get_progress_token();
        for step in 1..=steps {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = context.ct.cancelled() => return format!("cancelled after {} of {steps}", step - 1),
            }
            if let Some(token) = &token {
                let _ = context
                    .peer
                    .notify_progress(
                        ProgressNotificationParam::new(token.clone(), f64::from(step))
                            .with_total(f64::from(steps))
                            .with_message(format!("step {step} of {steps}")),
                    )
                    .await;
            }
        }
        format!("done {steps} steps")
    }

    /// Add a resource named `name` and tell the client its resource list
    /// changed.
    #[tool]
    async fn add_resource(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<AddResourceArgs>,
    ) -> String {
        if let Ok(mut extra) = self.extra_resources.lock()
            && !extra.contains(&args.name)
        {
            extra.push(args.name.clone());
        }
        for stream in self.listening() {
            let _ = stream.notify_resource_list_changed().await;
        }
        if !is_modern(&context) {
            let _ = context.peer.notify_resource_list_changed().await;
        }
        format!("{EXTRA_PREFIX}{}", args.name)
    }

    /// Return `structuredContent` whose `value` is text, although the tool
    /// declares a number, so a client can be tested on a result that breaks
    /// its own output schema.
    #[tool(output_schema = number_value_schema())]
    fn mistyped(&self) -> CallToolResult {
        CallToolResult::structured(serde_json::json!({"value": "three"}))
    }

    /// Declare everything a tool can beside its input: a long output
    /// schema, annotations and `_meta`, so a client's schema view is tested
    /// on several blocks at once.
    #[tool(
        output_schema = declared_schema(),
        annotations(title = "Declared", read_only_hint = true, idempotent_hint = true),
        meta = declared_meta()
    )]
    fn declared(&self) -> CallToolResult {
        CallToolResult::structured(serde_json::json!({
            "id": "declared-1",
            "name": "declared",
            "tags": ["mock"],
            "address": {"street": "1 Mock St", "city": "Mockville", "postcode": "00000", "country": "XX"},
            "items": [],
            "counts": {"read": 0, "written": 0, "failed": 0},
        }))
    }

    /// Ask the client to answer a question: with `elicitation/create` on a
    /// legacy request, as an input request of a round trip on a 2026-07-28
    /// one.
    #[tool]
    async fn elicit(
        &self,
        context: RequestContext<RoleServer>,
        InputResponses(responses): InputResponses,
        RequestState(state): RequestState,
        Parameters(args): Parameters<ElicitArgs>,
    ) -> CallToolResponse {
        if is_modern(&context) {
            return elicit_in_rounds(responses, state, &args);
        }
        text_result(elicit_over(&context.peer, args).await)
    }

    /// Ask the client to complete a prompt: with `sampling/createMessage` on
    /// a legacy request, as an input request on a 2026-07-28 one.
    #[tool]
    async fn sample(
        &self,
        context: RequestContext<RoleServer>,
        InputResponses(responses): InputResponses,
        Parameters(args): Parameters<SampleArgs>,
    ) -> CallToolResponse {
        if is_modern(&context) {
            return match responses.as_ref().and_then(|r| r.get(SAMPLE_KEY)) {
                Some(answer) => text_result(answer.to_string()),
                None => asking(
                    SAMPLE_KEY,
                    serde_json::json!({
                        "method": "sampling/createMessage",
                        "params": {
                            "messages": [{
                                "role": "user",
                                "content": {"type": "text", "text": args.prompt},
                            }],
                            "maxTokens": 64,
                        },
                    }),
                    None,
                ),
            };
        }
        let params =
            CreateMessageRequestParams::new(vec![SamplingMessage::user_text(args.prompt)], 64);
        text_result(match context.peer.create_message(params).await {
            Ok(result) => serde_json::to_string(&result).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("sampling error: {e}"),
        })
    }

    /// Ask the client for its roots and return them as JSON: with
    /// `roots/list` on a legacy request, as an input request on a 2026-07-28
    /// one.
    #[tool]
    async fn roots(
        &self,
        context: RequestContext<RoleServer>,
        InputResponses(responses): InputResponses,
    ) -> CallToolResponse {
        if is_modern(&context) {
            return match responses.as_ref().and_then(|r| r.get(ROOTS_KEY)) {
                Some(answer) => text_result(answer.to_string()),
                None => asking(ROOTS_KEY, serde_json::json!({"method": "roots/list"}), None),
            };
        }
        text_result(match context.peer.list_roots().await {
            Ok(result) => serde_json::to_string(&result).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("roots error: {e}"),
        })
    }
}

#[tool_router(router = v1_tools, vis = "pub")]
impl MockServer {
    /// Echo the text back.
    #[tool]
    fn echo(&self, Parameters(args): Parameters<EchoArgs>) -> String {
        args.text
    }

    /// Add two numbers.
    #[tool]
    fn add(&self, Parameters(args): Parameters<AddArgs>) -> Json<ArithmeticResult> {
        Json(ArithmeticResult {
            value: args.a + args.b,
        })
    }

    /// Wait for the given number of milliseconds.
    #[tool]
    async fn sleep(&self, Parameters(args): Parameters<SleepArgs>) -> String {
        tokio::time::sleep(std::time::Duration::from_millis(args.millis.min(10_000))).await;
        format!("slept {}ms", args.millis)
    }
}

#[tool_router(router = v2_tools, vis = "pub")]
impl MockServer {
    /// Add two numbers, rounded to `precision` decimals.
    #[tool(name = "add")]
    fn add_v2(&self, Parameters(args): Parameters<AddArgsV2>) -> Json<ArithmeticResult> {
        let factor = 10f64.powi(i32::try_from(args.precision).unwrap_or(i32::MAX));
        Json(ArithmeticResult {
            value: ((args.a + args.b) * factor).round() / factor,
        })
    }

    /// Multiply two numbers.
    #[tool]
    fn multiply(&self, Parameters(args): Parameters<MultiplyArgs>) -> Json<ArithmeticResult> {
        Json(ArithmeticResult {
            value: args.a * args.b,
        })
    }

    /// Pause for the given number of milliseconds (capped at ten seconds).
    #[tool(name = "sleep")]
    async fn sleep_v2(&self, Parameters(args): Parameters<SleepArgs>) -> String {
        tokio::time::sleep(std::time::Duration::from_millis(args.millis.min(10_000))).await;
        format!("slept {}ms", args.millis)
    }
}

#[prompt_router]
impl MockServer {
    /// Greet someone by name.
    #[prompt]
    fn greet(&self, Parameters(args): Parameters<GreetArgs>) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!("Please greet {} warmly.", args.name),
        )]
    }

    /// Summarize a piece of text.
    #[prompt]
    fn summarize(&self, Parameters(args): Parameters<SummarizeArgs>) -> Vec<PromptMessage> {
        let style = args.style.unwrap_or_else(|| "brief".into());
        vec![
            PromptMessage::new_text(
                Role::User,
                format!("Summarize the following text ({style}):"),
            ),
            PromptMessage::new_text(Role::User, args.text),
        ]
    }

    /// One message of about `kilobytes` of `kind` (`json`, `text` or
    /// `markdown`), to try a large prompt.
    #[prompt]
    fn long(&self, Parameters(args): Parameters<LongArgs>) -> Vec<PromptMessage> {
        let kilobytes = args
            .kilobytes
            .as_deref()
            .and_then(|kb| kb.trim().parse().ok());
        let bytes = prose_bytes(&ProseArgs { kilobytes });
        let block = match big_block(&args.kind, bytes, &format!("{BIG_PREFIX}long")) {
            Some(block) => block,
            None => ContentBlock::text(format!(
                "unknown kind `{}`: one of {}",
                args.kind,
                BIG_KINDS.join(", ")
            )),
        };
        vec![PromptMessage::new(Role::User, block)]
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for MockServer {
    fn get_info(&self) -> ServerInfo {
        let schema = self.schema.as_str();
        // The version a handshake falls back to must be one the server
        // supports, or `modern_only` would still agree to a handshake.
        let version = if self.faults.modern_only {
            ProtocolVersion::V_2026_07_28
        } else {
            ProtocolVersion::V_2025_11_25
        };
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_resources_list_changed()
                .enable_prompts()
                .enable_logging()
                .enable_completions()
                .build(),
        )
        .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
        .with_instructions(format!(
            "Hermetic mock server (schema {schema}) for testing MCP clients."
        ))
        .with_protocol_version(version)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        if self.faults.modern_only {
            Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
        } else if self.faults.legacy_only || self.faults.legacy_versions {
            Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25))
        } else {
            Cow::Borrowed(ProtocolVersion::KNOWN_VERSIONS)
        }
    }

    /// Discovery as `rmcp` answers it, refused under `legacy_only`. Over
    /// stdio [`RefuseDiscover`] answers first, so this matters to HTTP.
    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, ErrorData> {
        if self.faults.legacy_only {
            return Err(ErrorData::method_not_found::<DiscoverRequestMethod>());
        }
        Ok(DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        ))
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        if self.faults.broken_resources {
            return Err(ErrorData::internal_error("resources are unavailable", None));
        }
        if self.faults.stalled_resources {
            std::future::pending::<()>().await;
        }
        let all = self.resources();
        if !self.faults.paged_resources {
            return Ok(ListResourcesResult::with_all_items(all));
        }
        let start = match request.and_then(|r| r.cursor) {
            None => 0,
            Some(cursor) => cursor.parse::<usize>().map_err(|_| {
                ErrorData::invalid_params(format!("unknown cursor `{cursor}`"), None)
            })?,
        };
        let end = start.saturating_add(PAGE_SIZE).min(all.len());
        let page = all
            .get(start..end)
            .map(<[Resource]>::to_vec)
            .unwrap_or_default();
        let mut result = ListResourcesResult::with_all_items(page);
        result.next_cursor = (end < all.len()).then(|| end.to_string());
        Ok(result)
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        if self.faults.no_templates {
            return Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "resources/templates/list is not implemented",
                None,
            ));
        }
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new("mock://item/{id}", "item")
                .with_description("An item by id")
                .with_mime_type("text/plain"),
            ResourceTemplate::new(format!("{BIG_PREFIX}{{kind}}/{{kilobytes}}"), "big")
                .with_description(
                    "About `kilobytes` of `json`, `text` or `markdown`, to try a large read",
                ),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        match self.read(&request.uri) {
            Some(contents) => Ok(ReadResourceResult::new(vec![contents]).into()),
            // `rmcp` turns the code into -32602 for a 2026-07-28 client, which
            // finds the URI in the data.
            None => Err(ErrorData::resource_not_found(
                format!("unknown resource `{}`", request.uri),
                Some(serde_json::json!({ "uri": request.uri })),
            )),
        }
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if let Ok(mut subs) = self.subscriptions.lock() {
            subs.insert(request.uri);
        }
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if let Ok(mut subs) = self.subscriptions.lock() {
            subs.remove(&request.uri);
        }
        Ok(())
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(requested.clone())
    }

    /// Keep the stream's sink while it is open, for `bump` and
    /// `add_resource` to notify through.
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let id = self.next_listener.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut open) = self.listeners.lock() {
            open.push((id, context.sink().clone()));
        }
        context.cancelled().await;
        if let Ok(mut open) = self.listeners.lock() {
            open.retain(|(listening, _)| *listening != id);
        }
        Ok(())
    }

    async fn ping(&self, _context: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        if self.faults.ignore_pings {
            std::future::pending::<()>().await;
        }
        Ok(())
    }

    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.log_floor
            .store(level_rank(&request.level) as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Suggestions for the `greet` prompt's `name`, the `summarize` prompt's
    /// `style`, the item template's `id` and the kind of a large answer,
    /// filtered by what is typed.
    async fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let candidates: &[&str] = match (&request.r#ref, request.argument.name.as_str()) {
            (Reference::Prompt(prompt), "name") if prompt.name == "greet" => {
                &["Ada", "Alan", "Grace", "Guido"]
            }
            (Reference::Prompt(prompt), "style") if prompt.name == "summarize" => {
                &["brief", "detailed"]
            }
            (Reference::Resource(template), "id") if template.uri == "mock://item/{id}" => {
                &["1", "2", "42"]
            }
            (Reference::Prompt(prompt), "kind") if prompt.name == "long" => &BIG_KINDS,
            (Reference::Resource(template), "kind") if template.uri.starts_with(BIG_PREFIX) => {
                &BIG_KINDS
            }
            _ => &[],
        };
        let typed = request.argument.value.to_lowercase();
        let values = candidates
            .iter()
            .filter(|c| c.to_lowercase().starts_with(&typed))
            .map(|c| (*c).to_owned())
            .collect();
        let info = CompletionInfo::new(values).map_err(|e| ErrorData::internal_error(e, None))?;
        Ok(CompleteResult::new(info))
    }
}

/// `{"value": <number>}`: the output schema `mistyped` declares and breaks.
fn number_value_schema() -> Arc<rmcp::model::JsonObject> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"value": {"type": "number"}},
        "required": ["value"],
    });
    Arc::new(schema.as_object().cloned().unwrap_or_default())
}

/// The output schema `declared` declares: a record with nested objects and
/// an array of objects, long enough to need a scroll of its own.
fn declared_schema() -> Arc<rmcp::model::JsonObject> {
    let schema = serde_json::json!({
        "type": "object",
        "title": "Declared record",
        "description": "Everything a record can carry.",
        "properties": {
            "id": {"type": "string", "description": "Unique id."},
            "name": {"type": "string", "description": "Display name."},
            "tags": {"type": "array", "items": {"type": "string"}, "description": "Free-form labels."},
            "address": {
                "type": "object",
                "properties": {
                    "street": {"type": "string"},
                    "city": {"type": "string"},
                    "postcode": {"type": "string", "pattern": "^[0-9]{5}$"},
                    "country": {"type": "string", "minLength": 2, "maxLength": 2}
                },
                "required": ["street", "city", "country"]
            },
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "sku": {"type": "string"},
                        "quantity": {"type": "integer", "minimum": 0},
                        "price": {"type": "number", "minimum": 0},
                        "currency": {"type": "string", "enum": ["EUR", "USD", "GBP"]},
                        "dimensions": {
                            "type": "object",
                            "properties": {
                                "width": {"type": "number"},
                                "height": {"type": "number"},
                                "depth": {"type": "number"},
                                "unit": {"type": "string", "enum": ["mm", "cm", "m"]}
                            }
                        }
                    },
                    "required": ["sku", "quantity"]
                }
            },
            "counts": {
                "type": "object",
                "properties": {
                    "read": {"type": "integer"},
                    "written": {"type": "integer"},
                    "failed": {"type": "integer"}
                },
                "required": ["read", "written", "failed"]
            }
        },
        "required": ["id", "name"],
    });
    Arc::new(schema.as_object().cloned().unwrap_or_default())
}

/// The `_meta` `declared` carries.
fn declared_meta() -> rmcp::model::MetaObject {
    let meta = serde_json::json!({
        "vendor": SERVER_NAME,
        "since": "2026-09-24",
        "stable": true,
    });
    rmcp::model::MetaObject(meta.as_object().cloned().unwrap_or_default())
}

/// Whether `context` is a 2026-07-28 request, which is asked for input inside
/// its round trip and hears changes on a subscription stream, rather than
/// through requests and notifications of the server's own.
fn is_modern(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str())
}

/// The bytes `text` and `markdown` were asked for.
fn prose_bytes(args: &ProseArgs) -> usize {
    let kilobytes = args.kilobytes.unwrap_or(DEFAULT_PROSE_KB).min(MAX_PROSE_KB);
    usize::try_from(kilobytes)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024)
}

/// At least `bytes` of JSON: an object with a count and its records.
fn big_json(bytes: usize) -> String {
    let mut text = String::with_capacity(bytes + 256);
    text.push_str("{\"records\":[");
    let mut id = 0;
    while text.len() < bytes {
        if id > 0 {
            text.push(',');
        }
        text.push_str(&format!(
            "{{\"id\":{id},\"name\":\"record {id}\",\"tags\":[\"a\",\"b\"],\"score\":{}.5,\"ok\":true}}",
            id % 100
        ));
        id += 1;
    }
    text.push_str(&format!("],\"count\":{id}}}"));
    text
}

/// About `bytes` of `kind` as a content block: JSON and text as text
/// blocks, Markdown as an embedded `text/markdown` resource at `uri`.
fn big_block(kind: &str, bytes: usize, uri: &str) -> Option<ContentBlock> {
    Some(match kind {
        "json" => ContentBlock::text(big_json(bytes)),
        "text" => ContentBlock::text(prose(bytes)),
        "markdown" => ContentBlock::resource(ResourceContents::TextResourceContents {
            uri: uri.to_owned(),
            mime_type: Some("text/markdown".into()),
            text: markdown(bytes),
            meta: None,
        }),
        _ => return None,
    })
}

/// The large resource `uri`, whose path under [`BIG_PREFIX`] is `rest`: one
/// of the three listed at the default size, or `{kind}/{kilobytes}`.
fn big_resource(uri: &str, rest: &str) -> Option<ResourceContents> {
    let (kind, kilobytes) = match rest {
        "rows.json" => ("json", None),
        "prose.txt" => ("text", None),
        "readme.md" => ("markdown", None),
        _ => {
            let (kind, kb) = rest.split_once('/')?;
            (kind, Some(kb.parse::<u32>().ok()?))
        }
    };
    let bytes = prose_bytes(&ProseArgs { kilobytes });
    let (mime, text) = match kind {
        "json" => ("application/json", big_json(bytes)),
        "text" => ("text/plain", prose(bytes)),
        "markdown" => ("text/markdown", markdown(bytes)),
        _ => return None,
    };
    Some(ResourceContents::TextResourceContents {
        uri: uri.to_owned(),
        mime_type: Some(mime.into()),
        text,
        meta: None,
    })
}

/// At least `bytes` of numbered lines of plain text.
fn prose(bytes: usize) -> String {
    let mut text = String::with_capacity(bytes + 128);
    let mut line = 1;
    while text.len() < bytes {
        text.push_str(&format!(
            "Line {line}: the quick brown fox jumps over the lazy dog, \
             then reads the log, checks the schema and calls the tool again.\n"
        ));
        line += 1;
    }
    text
}

/// At least `bytes` of Markdown: numbered sections with a paragraph, a
/// list, a code block and a table each.
fn markdown(bytes: usize) -> String {
    let mut text = String::with_capacity(bytes + 512);
    text.push_str("# A long document\n\n");
    let mut section = 1;
    while text.len() < bytes {
        text.push_str(&format!(
            "## Section {section}\n\n\
             This is paragraph {section}, with **bold**, _italic_ and `code` in it, \
             long enough to wrap in a column of ordinary width.\n\n\
             - first point of section {section}\n\
             - second point\n\
             - third point, with a [link](https://example.com/{section})\n\n\
             ```json\n{{\"section\": {section}, \"ok\": true}}\n```\n\n\
             | key | value |\n|---|---|\n| section | {section} |\n| ok | true |\n\n"
        ));
        section += 1;
    }
    text
}

fn text_result(text: impl Into<String>) -> CallToolResponse {
    CallToolResult::success(vec![ContentBlock::text(text.into())]).into()
}

/// A result asking for `request` under `key`, carrying `state`.
fn asking(key: &str, request: serde_json::Value, state: Option<String>) -> CallToolResponse {
    match serde_json::from_value::<InputRequest>(request) {
        Ok(request) => {
            let requests = InputRequests::from([(key.to_owned(), request)]);
            InputRequiredResult::new(Some(requests), state).into()
        }
        Err(e) => text_result(format!("input request error: {e}")),
    }
}

/// What `elicit` returns to a legacy client, asked with `elicitation/create`.
async fn elicit_over(peer: &Peer<RoleServer>, args: ElicitArgs) -> String {
    let timeout = args.timeout_ms.map(std::time::Duration::from_millis);
    if let Some(url) = args.url {
        let url = match url::Url::parse(&url) {
            Ok(url) => url,
            Err(e) => return format!("elicitation error: {e}"),
        };
        return match peer
            .elicit_url_with_timeout(args.question, url, "mock-url", timeout)
            .await
        {
            Ok(rmcp::model::ElicitationAction::Accept) => "accepted".into(),
            Ok(rmcp::model::ElicitationAction::Decline) => "declined".into(),
            Ok(_) => "cancelled".into(),
            Err(e) if e.to_string().to_ascii_lowercase().contains("time") => "withdrawn".into(),
            Err(e) => format!("elicitation error: {e}"),
        };
    }
    match peer
        .elicit_with_timeout::<ElicitAnswer>(args.question, timeout)
        .await
    {
        Ok(Some(answer)) => format!("answer={}", answer.answer),
        Ok(None) => "declined".into(),
        Err(e) if e.to_string().to_ascii_lowercase().contains("time") => "withdrawn".into(),
        // rmcp reports decline/cancel as errors; keep the wire text simple.
        Err(e) if e.to_string().to_ascii_lowercase().contains("declined") => "declined".into(),
        Err(e) if e.to_string().to_ascii_lowercase().contains("cancel") => "cancelled".into(),
        Err(e) => format!("elicitation error: {e}"),
    }
}

/// What `elicit` returns to a 2026-07-28 client: the input request, until it
/// has been answered `repeat` times, counted in `requestState`; then the
/// answer, in the words the legacy path uses.
fn elicit_in_rounds(
    responses: Option<rmcp::model::InputResponses>,
    state: Option<String>,
    args: &ElicitArgs,
) -> CallToolResponse {
    let answer = responses.as_ref().and_then(|r| r.get(ELICIT_KEY));
    let answered = state
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
        + u32::from(answer.is_some());
    if let Some(answer) = answer
        && answered >= args.repeat.unwrap_or(1)
    {
        return text_result(elicitation_outcome(answer, args.url.is_some()));
    }
    let params = match &args.url {
        Some(url) => serde_json::json!({
            "mode": "url",
            "message": args.question,
            "url": url,
            "elicitationId": "mock-url",
        }),
        None => serde_json::json!({
            "mode": "form",
            "message": args.question,
            "requestedSchema": {
                "type": "object",
                "properties": {"answer": {"type": "string", "description": "The user's answer."}},
                "required": ["answer"],
            },
        }),
    };
    asking(
        ELICIT_KEY,
        serde_json::json!({"method": "elicitation/create", "params": params}),
        Some(answered.to_string()),
    )
}

/// An elicitation answer in the words the legacy path returns.
fn elicitation_outcome(answer: &serde_json::Value, url: bool) -> String {
    match answer.get("action").and_then(serde_json::Value::as_str) {
        Some("accept") if url => "accepted".into(),
        Some("accept") => match answer
            .pointer("/content/answer")
            .and_then(serde_json::Value::as_str)
        {
            Some(text) => format!("answer={text}"),
            None => "declined".into(),
        },
        Some("decline") => "declined".into(),
        _ => "cancelled".into(),
    }
}

/// Position of `level` in the protocol's order, `debug` first.
fn level_rank(level: &LoggingLevel) -> usize {
    const ORDER: [&str; 8] = [
        "debug",
        "info",
        "notice",
        "warning",
        "error",
        "critical",
        "alert",
        "emergency",
    ];
    serde_json::to_value(level)
        .ok()
        .and_then(|v| v.as_str().and_then(|s| ORDER.iter().position(|o| *o == s)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_parses() {
        assert_eq!(Schema::parse("V1"), Some(Schema::V1));
        assert_eq!(Schema::parse(" v2 "), Some(Schema::V2));
        assert_eq!(Schema::parse("1"), Some(Schema::V1));
        assert_eq!(Schema::parse("2"), Some(Schema::V2));
        assert_eq!(Schema::default(), Schema::V1);
        assert_eq!(Schema::V2.as_str(), "v2");
        // The old `a`/`b` names are gone rather than silently accepted.
        assert_eq!(Schema::parse("a"), None);
        assert_eq!(Schema::parse("b"), None);
    }

    #[test]
    fn tool_lists_differ_between_schemas() {
        let a: Vec<String> = MockServer::new(Schema::V1)
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        let b: Vec<String> = MockServer::new(Schema::V2)
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert!(a.contains(&"echo".to_string()));
        assert!(!b.contains(&"echo".to_string()));
        assert!(b.contains(&"multiply".to_string()));
        assert!(a.contains(&"add".to_string()) && b.contains(&"add".to_string()));
    }

    #[test]
    fn reads_known_resources() {
        let server = MockServer::new(Schema::V1);
        for r in server.resources() {
            assert!(server.read(&r.uri).is_some(), "{}", r.uri);
        }
        assert!(server.read("mock://item/42").is_some());
        assert!(server.read("mock://nope").is_none());
        // The large ones at any size, of the three kinds.
        for (uri, mime) in [
            ("mock://big/json/8", "application/json"),
            ("mock://big/text/8", "text/plain"),
            ("mock://big/markdown/8", "text/markdown"),
        ] {
            match server.read(uri) {
                Some(ResourceContents::TextResourceContents {
                    mime_type, text, ..
                }) => {
                    assert_eq!(mime_type.as_deref(), Some(mime), "{uri}");
                    assert!(text.len() >= 8 * 1024, "{uri}");
                }
                other => panic!("{uri}: {other:?}"),
            }
        }
        assert!(server.read("mock://big/csv/8").is_none(), "no such kind");
        assert!(server.read("mock://big/json/lots").is_none(), "not a size");
        assert!(big_block("csv", 8, "mock://big/long").is_none());
    }

    #[test]
    fn faults_leave_tools_and_reads_alone() {
        let faults = Faults {
            no_templates: true,
            broken_resources: true,
            stalled_resources: true,
            ignore_pings: true,
            paged_resources: true,
            legacy_only: true,
            modern_only: true,
            legacy_versions: true,
        };
        let server = MockServer::with_faults(Schema::V1, faults);
        assert_eq!(server.faults, faults);
        assert_eq!(MockServer::new(Schema::V1).faults, Faults::default());
        let tools = server.tool_router.list_all();
        assert!(tools.iter().any(|t| t.name == "echo"));
        assert!(server.read("mock://text/hello").is_some());
        assert!(server.read("mock://item/42").is_some());
    }

    #[test]
    fn prose_and_markdown_reach_the_size_asked_for() {
        let asked = 64 * 1024;
        let plain = prose(asked);
        assert!(
            plain.len() >= asked && plain.len() < asked + 256,
            "{}",
            plain.len()
        );
        assert!(plain.starts_with("Line 1: "));
        let md = markdown(asked);
        assert!(md.len() >= asked && md.len() < asked + 1024, "{}", md.len());
        assert!(md.starts_with("# A long document"));
        assert!(md.contains("## Section 2"));
        let json = big_json(asked);
        assert!(
            json.len() >= asked && json.len() < asked + 256,
            "{}",
            json.len()
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["records"].as_array().map(Vec::len),
            parsed["count"].as_u64().map(|n| n as usize),
            "counted and well formed"
        );
        let default = ProseArgs { kilobytes: None };
        assert_eq!(prose_bytes(&default), 256 * 1024);
        let capped = ProseArgs {
            kilobytes: Some(u32::MAX),
        };
        assert_eq!(prose_bytes(&capped), 8 * 1024 * 1024);
    }

    #[test]
    fn levels_rank_in_protocol_order() {
        assert_eq!(level_rank(&LoggingLevel::Debug), 0);
        assert!(level_rank(&LoggingLevel::Info) < level_rank(&LoggingLevel::Warning));
        assert_eq!(level_rank(&LoggingLevel::Emergency), 7);
    }

    #[test]
    fn an_added_resource_is_listed_and_read() {
        let server = MockServer::new(Schema::V1);
        let before = server.resources().len();
        assert!(server.read("mock://extra/notes").is_none());
        server.extra_resources.lock().unwrap().push("notes".into());
        assert_eq!(server.resources().len(), before + 1);
        assert!(server.read("mock://extra/notes").is_some());
    }

    #[test]
    fn faults_parse_from_their_flag_names() {
        assert_eq!(Faults::parse(""), Some(Faults::default()));
        assert_eq!(
            Faults::parse("broken-resources"),
            Some(Faults {
                broken_resources: true,
                ..Faults::default()
            })
        );
        assert_eq!(
            Faults::parse(" no-templates, broken-resources "),
            Some(Faults {
                no_templates: true,
                broken_resources: true,
                ..Faults::default()
            })
        );
        assert_eq!(
            Faults::parse("stalled-resources"),
            Some(Faults {
                stalled_resources: true,
                ..Faults::default()
            })
        );
        assert_eq!(
            Faults::parse("ignore-pings"),
            Some(Faults {
                ignore_pings: true,
                ..Faults::default()
            })
        );
        assert_eq!(
            Faults::parse("paged-resources"),
            Some(Faults {
                paged_resources: true,
                ..Faults::default()
            })
        );
        assert_eq!(
            Faults::parse("legacy-only,modern-only,legacy-versions"),
            Some(Faults {
                legacy_only: true,
                modern_only: true,
                legacy_versions: true,
                ..Faults::default()
            })
        );
        assert_eq!(Faults::parse("no-tools"), None);
    }
}
