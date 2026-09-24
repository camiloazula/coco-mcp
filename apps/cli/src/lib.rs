//! The command-line mode of Coco MCP, `coco-mcp --cli …`, built on the same
//! core crates as the desktop mode. Everything after `--cli` is the command
//! line, as it is.
//!
//! Exit codes: 0 ok, 1 breaking change (`diff`), 2 operational failure
//! (cannot connect, JSON-RPC error, tool returned `isError`, a list `diff`
//! could not compare because one side failed to read it).

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, clippy::print_stderr)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod import;
mod saved;
mod source;
mod style;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, CommandFactory, Parser, Subcommand};
use mcp_core::{
    ElicitationPolicy, EventCategory, EventSink, ProtocolMode, RootsPolicy, SamplingPolicy,
    ServerRequestPolicy, ServerSpec, Session, SessionOptions, Snapshot,
};
use mcp_store::{CallKind, CallStatus, NewCall, Store};
use serde_json::{Map, Value, json};

use crate::source::Source;

const EXIT_OK: u8 = 0;
const EXIT_BREAKING: u8 = 1;
const EXIT_FAILURE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "coco-mcp",
    bin_name = "coco-mcp --cli",
    version,
    about = "Coco MCP: debug MCP servers in depth from the command line",
    long_about = "Coco MCP: debug MCP servers in depth from the command line. Every command here follows `coco-mcp --cli`; `coco-mcp --desktop` opens the window instead.",
    propagate_version = true,
    styles = style::CLAP
)]
struct Cli {
    /// Record snapshots and calls in this SQLite database (created if missing).
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Per-request timeout in seconds.
    #[arg(long, global = true, default_value_t = 60, value_name = "SECS")]
    timeout: u64,
    /// Print the event log (wire traffic, notifications, stderr) to stderr.
    #[arg(long, global = true)]
    events: bool,
    /// Single-line JSON output.
    #[arg(long, global = true)]
    compact: bool,
    /// When to colour the output.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto",
          value_parser = ["auto", "always", "never"])]
    color: String,
    /// `None` when invoked bare: print the help and exit 0 rather than
    /// failing, so `coco` alone is a usable way to discover the commands.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Args)]
struct SourceArgs {
    /// `stdio:<command>`, `stdio -- <command> [args]` or an http(s) URL; `snapshot` also
    /// takes a snapshot file or `saved:<name>[~N]` (needs --db).
    source: String,
    /// Extra HTTP header (`Name: value`); repeatable.
    #[arg(long = "header", short = 'H', value_name = "HEADER")]
    headers: Vec<String>,
    /// Send `Authorization: Bearer <token>` with the token read from this environment variable.
    #[arg(long, value_name = "VAR")]
    bearer_env: Option<String>,
    /// Authorize with OAuth 2.1 (opens the browser once; tokens are kept in the OS keyring).
    #[arg(long)]
    oauth: bool,
    /// Protocol era: `legacy` (the initialize handshake), `auto` (2026-07-28 when the
    /// server has it, else the handshake) or `modern` (2026-07-28 only). A server saved
    /// with --db uses the mode it was saved with unless this is given.
    #[arg(long, value_name = "MODE", value_parser = ["legacy", "auto", "modern"])]
    protocol: Option<String>,
}

impl SourceArgs {
    /// Transport auth for an HTTP source.
    async fn transport(&self, url: &str) -> Result<mcp_core::transport::TransportOptions, String> {
        if (self.bearer_env.is_some() || self.oauth) && plain_http(url) {
            style::warn(format!(
                "{url} is plain http: the token is sent unencrypted"
            ));
        }
        if let Some(var) = &self.bearer_env {
            let token =
                std::env::var(var).map_err(|_| format!("--bearer-env: `{var}` is not set"))?;
            return Ok(mcp_core::transport::TransportOptions {
                bearer_token: Some(token.trim().to_owned()),
                oauth: None,
            });
        }
        if self.oauth {
            let secrets: std::sync::Arc<dyn mcp_auth::SecretStore> =
                std::sync::Arc::new(mcp_auth::KeyringStore::default());
            let options = mcp_auth::OAuthOptions {
                client_name: "coco-mcp".into(),
                ..mcp_auth::OAuthOptions::default()
            };
            style::status("Authorizing", format!("{url} (a browser window may open)"));
            let auth = mcp_core::AuthRef::OAuth {
                keyring_id: format!("oauth:{url}"),
            };
            return mcp_auth::resolve(&auth, url, secrets, &options)
                .await
                .map_err(|e| e.to_string());
        }
        Ok(mcp_core::transport::TransportOptions::default())
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Connect, list tools/resources/prompts and print the snapshot as JSON.
    Snapshot {
        #[command(flatten)]
        source: SourceArgs,
        /// Write the snapshot to this file instead of stdout.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Command and arguments for a `stdio` source (after `--`).
        #[arg(last = true)]
        trailing: Vec<String>,
    },
    /// Call a tool and print the result as JSON.
    Call {
        #[command(flatten)]
        source: SourceArgs,
        /// Tool name.
        tool: String,
        /// Arguments as a JSON object.
        #[arg(long, default_value = "{}", value_name = "JSON")]
        args: String,
        /// Command and arguments for a `stdio` source (after `--`).
        #[arg(last = true)]
        trailing: Vec<String>,
    },
    /// Read a resource and print the result as JSON.
    Read {
        #[command(flatten)]
        source: SourceArgs,
        /// Resource URI.
        uri: String,
        /// Command and arguments for a `stdio` source (after `--`).
        #[arg(last = true)]
        trailing: Vec<String>,
    },
    /// Get a prompt and print the result as JSON.
    Prompt {
        #[command(flatten)]
        source: SourceArgs,
        /// Prompt name.
        name: String,
        /// Arguments as a JSON object of strings.
        #[arg(long, default_value = "{}", value_name = "JSON")]
        args: String,
        /// Command and arguments for a `stdio` source (after `--`).
        #[arg(last = true)]
        trailing: Vec<String>,
    },
    /// Compare two snapshots and classify every change (exit 1 when any is breaking, 2 when a list could not be compared).
    Diff {
        /// Earlier side: a snapshot file, `stdio:<command>`, an http(s) URL, or `saved:<name>[~N]` (needs --db).
        a: String,
        /// Later side, same forms.
        b: String,
        /// Extra HTTP header for live http(s) sides (`Name: value`); repeatable.
        #[arg(long = "header", short = 'H', value_name = "HEADER")]
        headers: Vec<String>,
        /// Bearer token from this environment variable for live http(s) sides.
        #[arg(long, value_name = "VAR")]
        bearer_env: Option<String>,
        /// Authorize live http(s) sides with OAuth 2.1.
        #[arg(long)]
        oauth: bool,
        /// Protocol era for live sides: `legacy`, `auto` or `modern`.
        #[arg(long, value_name = "MODE", value_parser = ["legacy", "auto", "modern"])]
        protocol: Option<String>,
        /// Human-readable listing instead of JSON.
        #[arg(long)]
        text: bool,
    },
    /// Print the servers saved with --db as an `mcpServers` block for MCP clients.
    /// Secrets are never exported: bearer tokens become placeholders.
    ExportConfig {
        /// Only the server saved under this name.
        #[arg(long)]
        server: Option<String>,
    },
    /// Add servers to the database given with --db from an `mcpServers`
    /// file written by another MCP client or by `export-config`.
    /// A bearer token in the file is moved into the keyring.
    ImportConfig {
        /// The configuration file to read.
        path: PathBuf,
    },
    /// List recorded calls from the database given with --db.
    History {
        /// Only calls to the server saved under this name.
        #[arg(long)]
        server: Option<String>,
        /// Maximum rows.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

/// Run the command line in `args`, the words after `--cli`, and return the
/// process exit code. The words are parsed as they are; `--color` is read
/// from the process arguments first so even the help obeys it.
pub fn run_cli<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    style::apply_color_from_args();
    let argv = std::iter::once(OsString::from("coco-mcp"))
        .chain(args.into_iter().map(Into::into))
        .collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(error) => {
            // Help and version print and exit 0; a bad argument prints and
            // exits 2, the operational-failure code.
            let code = if error.use_stderr() {
                EXIT_FAILURE
            } else {
                EXIT_OK
            };
            let _ = error.print();
            return ExitCode::from(code);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            style::error(format!("cannot start the runtime: {error}"));
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            style::error(message);
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

struct Ctx {
    store: Option<Store>,
    timeout: Duration,
    events: bool,
    compact: bool,
}

impl Ctx {
    fn print(&self, value: &Value) -> Result<(), String> {
        let text = if self.compact {
            serde_json::to_string(value)
        } else {
            serde_json::to_string_pretty(value)
        }
        .map_err(|e| e.to_string())?;
        println!("{text}");
        Ok(())
    }

    async fn connect(&self, spec: ServerSpec, source: &SourceArgs) -> Result<Session, String> {
        let transport = match &spec {
            ServerSpec::Http { url, .. } => source.transport(url).await?,
            ServerSpec::Stdio { .. } => mcp_core::transport::TransportOptions::default(),
        };
        let sink = EventSink::new(4096);
        if self.events {
            let mut events = sink.subscribe();
            tokio::spawn(async move {
                while let Ok(event) = events.recv().await {
                    let (tag, painted) = match event.kind.category() {
                        EventCategory::Stderr => ("stderr", style::WARN),
                        EventCategory::State => ("state", style::NOTE),
                        _ => ("wire", style::MUTED),
                    };
                    let summary = style::plain(event.kind.summary());
                    anstream::eprintln!("{painted}[{tag}]{painted:#} {summary}");
                }
            });
        }
        // A terminal has no dialog to answer server requests, and with
        // `--events` the sink has a listener, so the asking default would
        // wait out its timeout: refuse, decline and advertise no roots.
        let policy = ServerRequestPolicy {
            sampling: SamplingPolicy::Reject,
            elicitation: ElicitationPolicy::Decline,
            roots: RootsPolicy::Fixed { roots: Vec::new() },
            ..ServerRequestPolicy::default()
        };
        // The same answers inside a 2026-07-28 round trip: the session asks
        // this policy for them either way.
        let options = SessionOptions {
            policy,
            protocol: self.protocol(&spec, source)?,
            request_timeout: Some(self.timeout),
            client_name: "coco-mcp".into(),
            sink: Some(sink),
            transport,
            ..SessionOptions::default()
        };
        style::status("Connecting", spec.label());
        Session::connect(spec, options)
            .await
            .map_err(|e| e.to_string())
    }

    /// Resolve a source to a snapshot: read a file, look one up in the
    /// database, or connect and take one (storing it when `--db` is set and
    /// every list was read).
    async fn load_snapshot(
        &self,
        source: &SourceArgs,
        trailing: &[String],
    ) -> Result<Snapshot, String> {
        match source::parse(&source.source, trailing, &source.headers)? {
            Source::File(path) => source::load_snapshot(&path),
            Source::Saved { name, back } => {
                let store = self
                    .store
                    .as_ref()
                    .ok_or("`saved:` sources need --db <path>")?;
                let server = saved::find(store, &name)?;
                let summaries = store
                    .list_snapshots(&server.id, back + 1)
                    .map_err(|e| e.to_string())?;
                let summary = summaries.get(back).ok_or_else(|| {
                    format!(
                        "`{name}` has {} stored snapshot(s), none at offset ~{back}",
                        summaries.len()
                    )
                })?;
                store
                    .get_snapshot(&summary.id)
                    .map_err(|e| e.to_string())?
                    .map(|r| r.snapshot)
                    .ok_or_else(|| format!("snapshot {} vanished", summary.id))
            }
            Source::Server(spec) => {
                let session = self.connect(spec.clone(), source).await?;
                let snapshot = session.snapshot().await.map_err(|e| e.to_string())?;
                // The other lists are still worth printing, but the store
                // keeps no snapshot with a list that failed (the app follows
                // the same rule), so a `saved:` baseline has every list.
                if let Some(id) = self.server_id(&spec, source)?
                    && let Some(store) = &self.store
                    && store
                        .add_snapshot(&id, &snapshot)
                        .map_err(|e| e.to_string())?
                        .is_none()
                {
                    style::warn(
                        "snapshot not stored: a snapshot missing a list is never a baseline",
                    );
                }
                session.close();
                Ok(snapshot)
            }
        }
    }

    /// The era to connect `spec` in: `--protocol` when given, else the mode
    /// the server is saved with in `--db`, else legacy.
    fn protocol(&self, spec: &ServerSpec, source: &SourceArgs) -> Result<ProtocolMode, String> {
        if let Some(mode) = &source.protocol {
            return ProtocolMode::parse(mode)
                .ok_or_else(|| format!("--protocol: unknown mode `{mode}`"));
        }
        let Some(store) = &self.store else {
            return Ok(ProtocolMode::default());
        };
        let saved = saved::existing_for(store, spec).map_err(|e| e.to_string())?;
        Ok(saved.map(|server| server.protocol).unwrap_or_default())
    }

    /// Persist the server under a name derived from its spec, in the mode it
    /// was connected in, and return its id.
    fn server_id(&self, spec: &ServerSpec, source: &SourceArgs) -> Result<Option<String>, String> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let protocol = self.protocol(spec, source)?;
        saved::server_for(store, spec, protocol)
            .map(|r| Some(r.id))
            .map_err(|e| e.to_string())
    }

    fn record(&self, call: NewCall) -> Result<(), String> {
        if let Some(store) = &self.store {
            store.record_call(call).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Warn about each list `snapshot` is missing, whichever kind of source it
/// came from, so a list left empty by a failure is not taken for an empty
/// one. `side` names the source when a command reads two.
fn warn_list_failures(snapshot: &Snapshot, side: Option<&str>) {
    let prefix = side.map(|side| format!("{side}: ")).unwrap_or_default();
    for failure in &snapshot.list_failures {
        style::warn(format!(
            "{prefix}`{}` failed: {}",
            failure.method, failure.error
        ));
    }
}

fn parse_args(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("--args: {e}"))?;
    if !value.is_object() {
        return Err("--args must be a JSON object".into());
    }
    Ok(value)
}

async fn run(cli: Cli) -> Result<u8, String> {
    let Some(command) = cli.command else {
        Cli::command().print_help().map_err(|e| e.to_string())?;
        println!();
        return Ok(EXIT_OK);
    };
    let store = match &cli.db {
        Some(path) => Some(Store::open(path).map_err(|e| format!("--db {}: {e}", path.display()))?),
        None => None,
    };
    let ctx = Ctx {
        store,
        timeout: Duration::from_secs(cli.timeout.max(1)),
        events: cli.events,
        compact: cli.compact,
    };

    match command {
        Command::History { server, limit } => {
            let store = ctx.store.as_ref().ok_or("history needs --db <path>")?;
            let server_id = match server {
                Some(name) => Some(saved::find(store, &name)?.id),
                None => None,
            };
            let calls = store
                .list_calls(server_id.as_deref(), limit)
                .map_err(|e| e.to_string())?;
            ctx.print(&serde_json::to_value(calls).map_err(|e| e.to_string())?)?;
            Ok(EXIT_OK)
        }
        Command::Snapshot {
            source,
            out,
            trailing,
        } => {
            let snapshot = ctx.load_snapshot(&source, &trailing).await?;
            warn_list_failures(&snapshot, None);
            emit_snapshot(&ctx, &snapshot, out.as_deref())?;
            Ok(EXIT_OK)
        }
        Command::Diff {
            a,
            b,
            headers,
            bearer_env,
            oauth,
            protocol,
            text,
        } => {
            let side = |source: &str| SourceArgs {
                source: source.to_owned(),
                headers: headers.clone(),
                bearer_env: bearer_env.clone(),
                oauth,
                protocol: protocol.clone(),
            };
            let before = ctx.load_snapshot(&side(&a), &[]).await?;
            let after = ctx.load_snapshot(&side(&b), &[]).await?;
            warn_list_failures(&before, Some(a.as_str()));
            warn_list_failures(&after, Some(b.as_str()));
            let diff = mcp_diff::diff(Some(&before), &after);
            if text {
                anstream::print!("{}", diff_text(&diff));
            } else {
                ctx.print(&serde_json::to_value(&diff).map_err(|e| e.to_string())?)?;
            }
            if diff.has_breaking() {
                return Ok(EXIT_BREAKING);
            }
            // A list that was not compared may hide a breaking change, so a
            // gate on the exit code must not pass it as unchanged.
            if let Some(note) = diff.not_compared() {
                style::error(note);
                return Ok(EXIT_FAILURE);
            }
            Ok(EXIT_OK)
        }
        Command::ImportConfig { path } => {
            let store = ctx
                .store
                .as_ref()
                .ok_or("import-config needs --db <path>")?;
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let config = mcp_exchange::read_client_config(&text)?;
            let added = import::import(store, &mcp_auth::KeyringStore::default(), config)?;
            ctx.print(&json!({ "imported": added }))?;
            Ok(EXIT_OK)
        }
        Command::ExportConfig { server } => {
            let store = ctx
                .store
                .as_ref()
                .ok_or("export-config needs --db <path>")?;
            let servers = match server {
                Some(name) => vec![saved::find(store, &name)?],
                None => store.list_servers().map_err(|e| e.to_string())?,
            };
            let config = mcp_exchange::client_config(&servers);
            for note in config.notes {
                style::warn(note);
            }
            ctx.print(&config.value)?;
            Ok(EXIT_OK)
        }
        Command::Call {
            source,
            tool,
            args,
            trailing,
        } => {
            let args = parse_args(&args)?;
            let spec = live_spec(&source, &trailing)?;
            let session = ctx.connect(spec.clone(), &source).await?;
            let server_id = ctx.server_id(&spec, &source)?;
            let started = std::time::Instant::now();
            let outcome = session.call_tool(&tool, args.clone()).await;
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            session.close();
            match outcome {
                Ok(outcome) => {
                    if let Some(server_id) = server_id {
                        ctx.record(NewCall {
                            server_id,
                            kind: CallKind::Tool,
                            name: tool.clone(),
                            args,
                            result: Some(outcome.raw.clone()),
                            status: if outcome.is_error {
                                CallStatus::ToolError
                            } else {
                                CallStatus::Ok
                            },
                            error: None,
                            elapsed_ms,
                        })?;
                    }
                    ctx.print(&with_elapsed(outcome.raw, outcome.elapsed))?;
                    if outcome.is_error {
                        style::error(format!("tool `{tool}` returned isError"));
                        Ok(EXIT_FAILURE)
                    } else {
                        Ok(EXIT_OK)
                    }
                }
                Err(e) => {
                    if let Some(server_id) = server_id {
                        ctx.record(NewCall {
                            server_id,
                            kind: CallKind::Tool,
                            name: tool.clone(),
                            args,
                            result: None,
                            status: CallStatus::Failed,
                            error: Some(e.to_string()),
                            elapsed_ms,
                        })?;
                    }
                    Err(format!("call `{tool}`: {e}"))
                }
            }
        }
        Command::Read {
            source,
            uri,
            trailing,
        } => {
            let spec = live_spec(&source, &trailing)?;
            let session = ctx.connect(spec.clone(), &source).await?;
            let server_id = ctx.server_id(&spec, &source)?;
            let started = std::time::Instant::now();
            let outcome = session.read_resource(&uri).await;
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            session.close();
            finish(
                &ctx,
                server_id,
                CallKind::Resource,
                &uri,
                Value::Null,
                elapsed_ms,
                outcome.map(|o| (o.raw, o.elapsed)),
            )
        }
        Command::Prompt {
            source,
            name,
            args,
            trailing,
        } => {
            let args = parse_args(&args)?;
            let spec = live_spec(&source, &trailing)?;
            let session = ctx.connect(spec.clone(), &source).await?;
            let server_id = ctx.server_id(&spec, &source)?;
            let map: Map<String, Value> = args.as_object().cloned().unwrap_or_default();
            let started = std::time::Instant::now();
            let outcome = session.get_prompt(&name, map).await;
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            session.close();
            finish(
                &ctx,
                server_id,
                CallKind::Prompt,
                &name,
                args,
                elapsed_ms,
                outcome.map(|o| (o.raw, o.elapsed)),
            )
        }
    }
}

/// Whether `url` is `http://` to a host other than this machine, where a
/// token would travel in clear text.
fn plain_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority);
    !(host.starts_with("127.") || host.starts_with("localhost") || host.starts_with("[::1]"))
}

fn live_spec(source: &SourceArgs, trailing: &[String]) -> Result<ServerSpec, String> {
    match source::parse(&source.source, trailing, &source.headers)? {
        Source::Server(spec) => Ok(spec),
        Source::File(_) | Source::Saved { .. } => {
            Err("this command needs a live server, not a snapshot".into())
        }
    }
}

/// Human-readable diff listing.
fn diff_text(diff: &mcp_diff::SnapshotDiff) -> String {
    let severity_style = |s: mcp_diff::Severity| match s {
        mcp_diff::Severity::Breaking => style::ERROR,
        mcp_diff::Severity::Compatible => style::GOOD,
        mcp_diff::Severity::Cosmetic => style::MUTED,
    };
    let counts: Vec<String> = [
        (diff.breaking, "breaking", mcp_diff::Severity::Breaking),
        (
            diff.compatible,
            "compatible",
            mcp_diff::Severity::Compatible,
        ),
        (diff.cosmetic, "cosmetic", mcp_diff::Severity::Cosmetic),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, label, severity)| style::paint(severity_style(severity), format!("{n} {label}")))
    .collect();
    let mut out = if counts.is_empty() {
        format!("{}\n", diff.summary())
    } else {
        let note = diff.not_compared().map(|note| format!(" · {note}"));
        format!("{}{}\n", counts.join(" · "), note.unwrap_or_default())
    };
    for change in diff.changes() {
        let kind = match change.kind {
            mcp_diff::ItemKind::Server => "server",
            mcp_diff::ItemKind::Tool => "tool",
            mcp_diff::ItemKind::Resource => "resource",
            mcp_diff::ItemKind::ResourceTemplate => "template",
            mcp_diff::ItemKind::Prompt => "prompt",
        };
        let path = if change.path.is_empty() {
            String::new()
        } else {
            format!(" {}", change.path)
        };
        // `server` is both the kind and the name of the initialize row.
        let subject = if change.kind == mcp_diff::ItemKind::Server {
            kind.to_owned()
        } else {
            format!("{kind} {}", change.name)
        };
        out.push_str(&format!(
            "{} {subject}{path}: {}\n",
            style::paint(
                severity_style(change.severity),
                format!("{:<11}", change.severity.label())
            ),
            change.summary
        ));
    }
    out
}

fn with_elapsed(mut raw: Value, elapsed: Duration) -> Value {
    if let Some(obj) = raw.as_object_mut() {
        obj.insert("_elapsedMs".into(), json!(elapsed.as_millis() as u64));
    }
    raw
}

/// Shared tail of `read` and `prompt`: record, print, map errors.
fn finish(
    ctx: &Ctx,
    server_id: Option<String>,
    kind: CallKind,
    name: &str,
    args: Value,
    elapsed_ms: u64,
    outcome: Result<(Value, Duration), mcp_core::Error>,
) -> Result<u8, String> {
    match outcome {
        Ok((raw, elapsed)) => {
            if let Some(server_id) = server_id {
                ctx.record(NewCall {
                    server_id,
                    kind,
                    name: name.to_owned(),
                    args,
                    result: Some(raw.clone()),
                    status: CallStatus::Ok,
                    error: None,
                    elapsed_ms,
                })?;
            }
            ctx.print(&with_elapsed(raw, elapsed))?;
            Ok(EXIT_OK)
        }
        Err(e) => {
            if let Some(server_id) = server_id {
                ctx.record(NewCall {
                    server_id,
                    kind,
                    name: name.to_owned(),
                    args,
                    result: None,
                    status: CallStatus::Failed,
                    error: Some(e.to_string()),
                    elapsed_ms,
                })?;
            }
            Err(format!("{} `{name}`: {e}", kind.as_str()))
        }
    }
}

fn emit_snapshot(
    ctx: &Ctx,
    snapshot: &Snapshot,
    out: Option<&std::path::Path>,
) -> Result<(), String> {
    let mut value = serde_json::to_value(snapshot).map_err(|e| e.to_string())?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("digest".into(), Value::String(snapshot.digest()));
    }
    match out {
        Some(path) => {
            let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
            std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            style::status(
                "Wrote",
                format!(
                    "snapshot of {} ({} tools, {} resources, {} prompts) to {}",
                    snapshot.server_name().unwrap_or("server"),
                    snapshot.tools.len(),
                    snapshot.resources.len(),
                    snapshot.prompts.len(),
                    path.display()
                ),
            );
            Ok(())
        }
        None => ctx.print(&value),
    }
}
