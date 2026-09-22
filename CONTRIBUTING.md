# Contributing

Coco MCP: a Rust tool that debugs MCP (Model Context Protocol) servers in
depth, from the command line or a desktop window drawn by GPUI. One binary,
two modes. These rules keep it that way.

## Hard constraints

1. **Rust only.** No JavaScript, TypeScript, web view, Node or Electron.
2. **No web engine.** The UI is GPUI. `wry`, `tao`, `webview2-com` and any
   gpui-kit WebView feature are banned in `deny.toml`.
3. **Fully open source stack.** Every dependency is MIT, Apache-2.0, BSD,
   MPL-2.0, ISC or Zlib (plus the permissive dedications listed in
   `deny.toml`). No GPL/AGPL/LGPL, no source-available. Our code is
   MIT OR Apache-2.0. `cargo deny check licenses` enforces this locally and
   in CI. Any per-crate exception goes in `deny.toml` with a justification.
   The same applies to assets: the two bundled fonts are SIL OFL 1.1 and the
   app never asks the OS for a proprietary face.
4. **No code from the Zed repository.** Only the published `gpui-pre` crate
   (Apache-2.0) and gpui-kit (Apache-2.0).
5. **Reuse crates before writing anything.** Hand-write only what has no
   adequate crate, and say why in a comment at the top of that module.
6. **The core is UI-free.** Nothing under `crates/` depends on `gpui` or
   `gpui-kit`; everything the app does is reachable from a test without a
   window.
7. **One binary, plainly built.** `cargo build --release` of the package
   `coco-mcp` is the whole build. The release workflow runs exactly that on
   each platform and attaches the result to the GitHub release; there is
   no installer, code signing, notarization or store pipeline, and no
   second binary: the command line is `coco-mcp --cli`.

## Working rules

- Read the relevant gpui-kit example or test in the pinned crate source
  (`~/.cargo/registry/src/*/gpui-kit-0.6.1`, `gpui-component-0.6.1`) before
  writing GPUI code; trust the pinned source over memory.
- Views stay under ~300 lines, tests aside. A view that outgrows that moves
  a part into a child module (`views/detail/declared.rs`), which starts with
  `use super::*;` and so sees the parent's private items; what the parent
  calls from it is `pub(super)`.
- Every crate has unit tests; `mcp-core` also has integration tests against
  `mcp-mockserver`, and `mcp-schema-form` against real-world schema fixtures
  in `tests/fixtures/`.
- The command line is the `coco-cli` library (`apps/cli`), UI-free and
  with no dependency on `mcp-mockserver`; the binary hands it the words
  after `--cli`. Its end-to-end tests live in `apps/desktop/tests/cli.rs`,
  run the one binary with `--cli`, and locate the standalone
  `mcp-mockserver` binary next to it in the shared target directory rather
  than importing the crate, so `just test-app` (and CI) build the mock
  server first; `just mock` runs it directly.
- No `unwrap()`/`expect()` outside tests (clippy denies them). No `unsafe`.
- State the licence of any new dependency in the commit message.
- Never build with `--workspace`; select packages with `-p` (the `just`
  recipes do). The GPUI app has heavy native deps.
- Design values live in code: `apps/desktop/src/theme.rs` (tokens, fonts)
  and the view constants (sidebar 200, list 280, rows 28, hairline 1, title
  bar 36, status bar 24, drawer 260/28, radius 3, type 15/13/12/11).
  `docs/screenshots/` holds the design screens the README and this file
  refer to, plus the reference renders of the main states; `just screenshots`
  renders every flow into `target/screenshots/` and assembles `tour.gif`,
  the README's animated tour, from the renders `TOUR` in
  `tests/screenshots.rs` names. Copy the renders and the tour into `docs/`
  when a screen changes. The README logo is `docs/logo-{dark,light}.svg`,
  the mark of `icons/coco-mark.svg` with the eyes, coloured for each theme.
  Do not invent visual values.
- The native menu bar is built in `apps/desktop/src/menus.rs`; menu items
  dispatch the same actions as the key bindings, and `About` / `Quit` have
  global handlers so they work from any window. The binary is run as the
  release archive or Cargo leaves it; no platform packaging is assembled.
- Brand assets live in `apps/desktop/assets/`: `icon.svg`, the mark, and
  the four `icons/coco-*.svg` masks the About window draws. GPUI tints an
  SVG with the element's text colour, so the outline and the eyes are
  separate files and each takes a theme token.
- Every display of structured data is foldable. A tree node's key is its
  tree `prefix` plus its JSON path, and lives in `Workspace.collapsed`; a
  prefix must be unique among the trees that can be on screen together and
  stable across renders, and must include `server_scope` when it is only
  unique within one server (log row ids, tool names). `json.rs` owns the
  chevron and the `{ … N keys }` / `[ … N items ]` summaries; the generated
  form reuses both through `views::fold_icon`. There is no non-interactive
  tree renderer: do not add one. A tree is a list of its lines: the value is
  walked once into a plan of lines under the folds in force (`json/lines.rs`,
  one small entry per line, what a line says being read from the value when
  it is drawn), kept until the value or `Workspace.collapse_rev` changes,
  and a uniform list builds only the lines in view from it. Every container
  starts open; only `Workspace.collapsed` folds one. A tree takes its rows
  up to `json::MAX_TREE_ROWS` and scrolls inside past that (`Fit::Rows`),
  or, as the one thing in a panel (a response that is one tree, the schema
  tab, a recorded call's arguments), the panel's height (`Fit::Fill`).
  Nothing is held back for its size: a result is drawn as it arrives, and a
  hundred thousand rows are parsed and planned in one frame.
- A selected response is drawn on every frame, so nothing it shows may cost
  per frame what it can cost once: copy buttons build their text when
  pressed; decoded images, text blocks parsed and copied once, and the
  `Rc` a tree's value is cloned into once per answer stay in
  `Workspace.decoded` until a frame no longer draws them; and a tree's line
  plan is kept in `views::json` until its value or the folds change.
- Everything on screen has a way out, and a client configuration has a way
  in; `mcp-exchange` owns every shared format, in both directions, so the app
  and the CLI cannot disagree. A JSON tree line carries a right-click
  copy menu keyed by *segments*, not by the printed path, because a key may
  contain `.` or `[`; a named block of structured data is drawn with
  `views::tree_section`, which puts the copy button in the same place every
  time. New copy or export payloads belong in `views/copy.rs` as plain
  functions of the model, so the menu bar, the palette and a button cannot
  produce different bytes, and so a test can build them without a save panel.
- A bearer token is redacted in everything that leaves the app: `$MCP_TOKEN`
  in a shell command, `Bearer <token>` in a config. No `mcp-exchange` format
  can carry the real value; it leaves only through "Copy Bearer Token",
  which reads it from the keyring and copies it alone.
- Overlays are hand-written: the request dialog, the confirm dialog, the
  palette card and the copy menu. gpui-component's `ContextMenu` keeps its
  popup entity in a shared cell that its own dismiss subscription captures,
  so every open leaks one entity and the headless tests refuse to exit.
- Every glyph drawn must exist in the bundled fonts (Inter covers the
  keyboard symbols; Geist Mono does not, so key hints use `kbd`, not `mono`).
  Symbols missing from both, such as chevrons and the search glyph, are
  Lucide icons from gpui-kit. A missing glyph would be filled from a
  proprietary system font.

## Stack (pinned)

| Concern | Crate | Version | Licence | Notes |
|---|---|---|---|---|
| MCP protocol, transports | `rmcp` | 3.3 | Apache-2.0 | Client (`mcp-core`): `client`, `transport-child-process`, `transport-streamable-http-client-reqwest`, `reqwest`, `auth`, `elicitation`; `mcp-auth` uses `client`, `auth`, `reqwest`, `transport-streamable-http-client-reqwest`. Mock server: `server`, `macros`, `schemars`, `transport-io`, `elicitation`, `transport-streamable-http-server`. |
| UI engine | `gpui` = `gpui-pre` | **=0.3.2** | Apache-2.0 | The revision gpui-kit 0.6.1 resolves. Never float. |
| Widgets | `gpui-kit` | **=0.6.1** | Apache-2.0 | Features `component`, `assets`, `tree-sitter`. `gpui_kit::*` is GPUI; `gpui_kit::component::*` the widget layer. |
| Async runtime | `tokio` | 1.53 | MIT | Multi-thread runtime on a background thread; bridged to GPUI via channels. |
| Serialization | `serde`, `serde_json` | 1 | MIT/Apache-2.0 | `preserve_order` so unknown spec fields survive. |
| JSON Schema | `jsonschema` | 0.56 | MIT | Validation only. `schemars` 1.2 for our own types. |
| Storage | `rusqlite` (`bundled`) | 0.40 | MIT | `rusqlite_migration` 2.6. |
| Secrets | `keyring` | 4.2 | MIT/Apache-2.0 | Never write secrets to SQLite or logs. |
| OAuth 2.1 | via `rmcp` `auth` | | | `axum` 0.8 loopback listener, `open` 5.4 opens the browser. |
| HTTP client | `reqwest` | 0.13 | MIT/Apache-2.0 | rustls only; `openssl-sys`/`native-tls` banned. |
| Paths, config | `directories` 6, `toml` 1.1 | | MIT/Apache-2.0 | |
| Logging | `tracing*` | | MIT | The log drawer is fed from an in-memory broadcast, not `tracing`. |
| CLI | `clap` | 4.6 | MIT/Apache-2.0 | |
| CLI output style | `anstyle`, `anstream` | 1.0 | MIT/Apache-2.0 | Cargo's palette and terminal-aware streams; both arrive with clap. `apps/cli/src/style.rs` holds the palette and the status-line helpers. |
| Command lines | `shlex` | 2.0 | MIT/Apache-2.0 | POSIX quoting for a stdio server's command line (`mcp_core::command_line`, `split_command_line`): the server form, `ServerSpec::label`, and `coco`'s `stdio:` sources and saved-server names; already in the graph through `cc`. |
| URLs | `url` | 2 | MIT/Apache-2.0 | HTTP endpoints and OAuth in `mcp-core`; URL elicitations in the mock server. |
| Errors | `thiserror` 2 / `anyhow` 1 | | MIT/Apache-2.0 | |
| Time, ids, digest | `time`, `uuid` v7, `sha2`, `hex` | | MIT/Apache-2.0 | |
| Licence audit | `cargo-deny` | any current | | `just deny` locally; `EmbarkStudios/cargo-deny-action@v2` in CI. |
| Fonts | Inter 4.0, Geist Mono 1.5 | | OFL-1.1 | Embedded from `apps/desktop/assets/fonts/`, registered in `theme::install`. |

## Layout

```
crates/mcp-core          rmcp client wrapper: ServerSpec, Session, Snapshot, events
crates/mcp-schema-form   JSON Schema -> FormModel -> JSON value; no UI
crates/mcp-store         SQLite: servers, snapshots, calls, settings
crates/mcp-auth          keyring SecretStore, rmcp CredentialStore, loopback, OAuth orchestration
crates/mcp-diff          Snapshot -> classified diff (breaking / compatible / cosmetic)
crates/mcp-exchange      shared formats both ways: JSON-RPC, curl, client config, JSONL, Markdown
crates/mcp-mockserver    hermetic test server: two schemas, faults, both eras, stdio and HTTP
apps/cli                 coco-cli library: snapshot, call, read, prompt, history, diff, import/export-config
apps/desktop             the one binary (package coco-mcp): --desktop draws the GPUI views, --cli hands over to coco-cli
```

## Commands

```bash
just              # list every recipe
just check        # fmt-check + clippy + tests for every package + cargo-deny
just test-app     # app unit tests + headless flows, mock server built first
just clippy-app   # clippy for the GPUI app (slow)
just run          # the window
just coco <args>  # the command-line mode
just screenshots  # headless UI flows + PNGs (macOS)
```

## Releasing

A release is a tag `vX.Y.Z` on `main` and the GitHub release that carries
it. Tags never move: a wrong release gets the next patch version.

1. Merge a PR that sets `version` in the workspace `Cargo.toml`.
2. Check that every PR merged since the last tag carries one label:
   `enhancement`, `bug` or `documentation`. The generated notes list the
   merged PRs by title under a section per label, in the order
   `.github/release.yml` gives; an unlabelled PR lands under "Other", and
   `skip-changelog` leaves one out. There is no changelog file: the
   Releases page is the changelog, one line per PR, so a PR title says
   what changed for a user.
3. From an up-to-date `main`, create the tag and the release in one step:

   ```bash
   gh release create vX.Y.Z --title "Coco MCP X.Y.Z" --generate-notes
   ```

   `--draft` keeps it unpublished until you press the button on the site;
   `--notes-file` replaces the generated notes. A bare
   `git push origin vX.Y.Z` also works, and the workflow then creates the
   release itself with generated notes. Never `git push --tags`.
4. The tag starts `.github/workflows/release.yml`, which refuses a tag that
   does not match `Cargo.toml`, builds the binary for macOS (arm64 and
   x86_64), Linux (x86_64) and Windows (x86_64), and attaches the archives
   and a `SHA256SUMS` file to the release.
5. Bump the Homebrew formula in `camiloazula/homebrew-coco` to the new
   tag's archive and checksum, for example with
   `brew bump-formula-pr --tag vX.Y.Z camiloazula/coco/coco-mcp`.

To rehearse the matrix without a tag, run the workflow by hand from the
Actions tab: it builds every target and keeps the archives as workflow
artifacts, publishing nothing.

## Engineering notes

### rmcp 3.3

- `serve_client_with_lifecycle_and_ct(handler, transport, lifecycle, token)`
  starts a session and returns a `RunningService`;
  `service.peer().clone()` is the cloneable `Peer<RoleClient>`. The lifecycle
  comes from the server's `ProtocolMode` (`mcp_core::lifecycle`):
  `Initialize` for Legacy, `Discover` with 2026-07-28 for Modern, `Auto` for
  Auto. `peer_info()` is a `ServerPeerInfo` in either era, so
  `Session::server_info` and the snapshot read one shape, and the session's
  `Era` comes from the agreed version.
- Auto discovers with 2026-07-28 alone: a discover lifecycle at a legacy
  version belongs to neither era. rmcp falls back to the handshake by itself
  when a server does not answer `server/discover`, but returns
  `NoCompatibleProtocolVersion` when it answers without that version; the
  session then opens a second transport (over stdio, a second process) and
  starts with the handshake. Tests pass the opener to
  `Session::connect_with_transports`.
- Once rmcp's stdio server has seen a first request other than `initialize`,
  it keeps requiring per-request metadata, so refusing discovery in the
  handler would refuse the handshake that follows too. The mock server's
  `--legacy-only` answers `server/discover` in a transport wrapper
  (`mcp_mockserver::legacy::RefuseDiscover`) before the server sees it; over
  HTTP, where every modern request stands alone, its handler refuses it.
- A 2026-07-28 server answers `input_required` when it needs sampling,
  elicitation or roots. rmcp's helpers that drive the rounds carry no
  `RequestControl`, so `Session::request` runs them: each input request is
  read from its JSON into a `ServerRequestKind` and answered by
  `CocoClient::answer`, the same policy and dialog a legacy server's own
  request goes through; the request is then sent again with a new id, the
  answers and the `requestState` echoed as sent. At most `MAX_INPUT_ROUNDS`
  (10); the request timeout applies to each round, and cancel and progress
  follow the latest. `ElicitationMode::Url` carries `elicitationId` as
  optional, which 2026-07-28 drops, but rmcp 3.3 still requires it when it
  parses a URL elicitation, so a server that omits it fails before the
  session sees the request.
- Notifications on a `subscriptions/listen` stream go to its `Subscription`
  alone, never to the `ClientHandler`. `mcp-core`'s private `listen` module keeps one stream
  per modern session, for the list changes the server announces and the
  resources subscribed to; subscribing or unsubscribing opens it again with
  the new set and answers once the server acknowledged it. Its notifications
  become the usual `ListChanged` and `ResourceUpdated` events. An abrupt end
  opens it again after a doubling wait; a graceful one waits for the next
  change. What it does on its own is logged as `EventKind::Note` rows.
- rmcp's list and read helpers answer from a cache kept by `ttlMs` and hand
  back a stale copy when the server fails, so lists, reads and completions go
  out through `send_cancellable_request` and never through them.
- rmcp's HTTP client removes a tool with invalid `x-mcp-header` annotations
  from `tools/list` inside its own transport, before `TracedTransport` sees
  the message, and says so only through `tracing`: the app cannot show a
  tool dropped that way.
- There is no transport observer hook: `mcp_core::transport::TracedTransport`
  wraps any `Transport<RoleClient>` and classifies messages by JSON shape.
- Most model structs are `#[non_exhaustive]`: use constructors or
  `serde_json::from_value(json!({..}))`. Capability builders only exist with
  the `server`/`macros` features.
- Sampling and roots are `#[deprecated]` (SEP-2577) but still used by
  servers: `#![allow(deprecated)]` where they are touched.
- `Peer<RoleServer>::elicit::<T>` needs `rmcp::elicit_safe!(T)` and reports
  decline/cancel as `Err`.
- Tool argument type errors come back as `CallToolResult { isError: true }`,
  unknown tools and resources as JSON-RPC errors.
- `tokio::sync::watch::Sender::send` drops without receivers; use
  `send_replace`.
- rmcp's `auth` module covers discovery, RFC 8414 metadata, dynamic client
  registration, PKCE and refresh; `mcp-auth` adds the keyring store, the
  loopback listener and orchestration. `auth_header` takes the bare token.

### GPUI / gpui-kit

- Layout uses `h_resizable`/`v_resizable`, not `DockArea` (the dock skin
  wraps a lone panel in a tab bar). Toggling a panel's `visible` inside a
  group redistributes sizes; switch layouts instead.
- Theme: `theme::install` builds a `ThemeSet` JSON, loads it with
  `ThemeRegistry::load_themes_from_str`, then `Theme::change`.
  `focus_ring = false` gives the tinted-border focus.
- Edition 2024 `impl IntoElement` return values capture `&mut cx`; view
  functions return `AnyElement`.
- `.test_support()` wraps into `Observed<_>`: call it last; `Input` registers
  itself. Headless tests run on the main thread (`harness = false`) and pump
  tasks with `cx.run_until_parked()`.
- All `Session` calls go through `bridge::Bridge::run` (tokio) and come back
  over a `futures` oneshot awaited in `cx.spawn`. Never call a Session
  method on the GPUI thread.
- The command palette dispatches each `CommandItem`'s boxed action from its
  query input, so the root `on_action` handlers receive it; focus it in
  `window.defer` because its state is created during the same render.
- Server requests arrive as `EventKind::ServerRequest` and queue in
  `AppState.pending`; the dialog answers the head via `ServerRequest::respond`.
- On connect the app diffs the fresh snapshot against the last stored one and
  stores it only when the digest changed, off the GPUI thread and before the
  server shows as connected. `Store::add_snapshot` refuses a snapshot with a
  list that failed, for the app and `coco --db` alike, so a baseline always
  has every list. The diff skips a failed list, so it never decides what is
  stored.
- A list the server advertises but cannot deliver does not fail the connect.
  `Session::snapshot` leaves it empty and names it in
  `Snapshot::list_failures` (method-not-found is an empty list, not a
  failure) under one of the `mcp_core::list_method` names. Once a list times
  out, the lists after it are named without being requested, so a server
  that stopped answering costs one request timeout, not one per list.
  `mcp-diff` skips
  it and names it in `SnapshotDiff::skipped`, which every diff summary ends
  with; the app shows it in the list column, the detail pane of a mode it
  left with no items, the sidebar count and the status bar; `coco` warns,
  and `coco diff` exits 2 when a list was not compared and nothing that was
  compared is breaking.
- Connecting again never clears a server's log: the rows of the session that
  ended may be the only record of why it did. `connect` appends a `session`
  note row (`LogRow::session_break`) that every log filter shows and a log
  export writes; `clear_log` is the only way to empty the log, and the row
  cap still bounds it. The cap drops rows from the front, so a row's index
  shifts; every row carries an id its server hands out (`LogRow::row_id`),
  and folds, list measurements, the copy menu and the expanded row key on
  it, so neither the cap nor Clear moves them onto another row.
- A long session must not cost more per frame than a short one. A log row is
  built on the runtime (`Bridge::forward_events` with `state::incoming`),
  where a payload over `LOG_PAYLOAD_LIMIT` is replaced by a marker holding
  its head and `originalBytes`, so the row, its copy menu and a log export
  all say what was left out. A server's log is also drained from the front
  past `LOG_BYTE_BUDGET`, which counts rows as compact JSON: the parsed
  values they hold take a few times that, still bounded. The drawer takes
  the filtered row indices once per frame, borrows each row it draws, and
  builds a row's copy menu only when it opens. The middle list is built once
  per change of what it lists (the selected server, its snapshot or history,
  the mode, the filter) and shared by every read: `ServerEntry` keeps its
  snapshot and history behind setters that bump a revision, so a change
  cannot skip the rebuild. Its rows are drawn by `uniform_list`, which
  builds only the rows in view, and a key that moves the selection scrolls
  it into view. A burst of wire events is applied in one update and redraws
  once: the event task files everything already queued when it wakes
  (`bridge::drain_ready`). What the views keep beside the model goes with
  its subject: `AppState` emits `Gone` when rows leave a log, and the
  workspace drops their folds; deleting a server drops its responses, the
  requests it was waiting on, and every fold, reveal and measured size kept
  for it, found by the server id a server-scoped key embeds between colons
  (`{what}:{server}:…`).
- A database that could not be opened, or a database or keyring write that
  failed, sets `AppState.persistence`, which the status bar shows. Nothing
  claims to be saved without a database: adding, editing and importing
  servers are refused. Only a model built without one on purpose (demos,
  tests) keeps servers in memory.
- Adding, editing and importing servers write off the GPUI thread
  (`AppState::in_background`): the row first, then the bearer token, then
  the removal of a token the server no longer uses. The sidebar changes and
  the form closes only once the row is saved, so a save the database
  refuses leaves the keyring as it was.
- Startup reads only what the first frame draws: the server rows and the
  theme. A server's recorded calls are read the first time its History is
  shown, off the GPUI thread (`AppState::show_history`); until then its
  counts in the sidebar and the filter row are blank, the list says it is
  reading, no key selects a call and the detail pane shows none, and a
  history export waits for them. Calls recorded meanwhile are merged with
  the stored ones by id (`persistence::merge_history`), so none is lost or
  listed twice. The History filter matches a call's whole content: each
  row carries `Item::text`, built once per call by `state::search_text`
  (kind, name, outcome, error, arguments, result, lowercased, cut at
  `SEARCH_TEXT_LIMIT`) and kept in `ServerEntry::search_text` until the
  call leaves, so a keystroke costs one pass over strings already built. Keyring reads leave the GPUI thread too: the edit form fills
  in a stored bearer token when it arrives, keeps one typed meanwhile and
  refuses to save an empty one before then, and Copy Bearer Token copies
  once it is read.

- Every call, read and get goes out through `send_cancellable_request`
  with a `mcp_core::RequestControl`. Cancelling it, or running past the
  request timeout, sends `notifications/cancelled`; its progress token names
  the `notifications/progress` that belong to the request. The app keeps one
  per pending response (`calls::Waiting`) and shows the latest progress and a
  running clock under the Cancel button (⌘.). Reads do not use rmcp's
  `Peer::read_resource`, which caches: a subscribed resource must read as it
  is now. A request the server withdraws (`EventKind::RequestCancelled`)
  leaves the dialog queue unanswered.
- A tool result is checked against the tool's `outputSchema` on the runtime
  when it arrives (`Response.issues`), and the issues are shown above it.
- A `*/list_changed` notification reads that list again in place
  (`Session::relist`), keeping the session; what it added or altered is
  marked in the list until selected. A `resources/updated` for a subscribed
  resource reads it again under the response already shown, without a
  history row, since nobody asked for that read.
- `SessionOptions::keepalive` pings a server that has been quiet for that
  long with nothing of ours in its hands, and fails the session when the ping
  goes unanswered as long again. A slow call is not a silent server. A
  request given up on (timed out or cancelled) is struck from the transport's
  record (`Heard::forget`), or the keepalive would wait on it forever. A
  modern session never pings: 2026-07-28 has no `ping`, and a stdio server
  that exits still ends the session.
- What a server sends is bounded before it is kept: a list ends after
  `MAX_LIST_PAGES` or when a cursor repeats, a child's stderr line is cut at
  `STDERR_LINE_LIMIT`, and a URL a server asks the client to open must be
  `http` or `https` (`handler::web_url`; the OAuth authorization endpoint
  likewise, `oauth::browser_url`). The loopback listener turns away a
  redirect whose `state` is not the one this flow issued.
- Whether a feature can run is decided in one place, `features::Features`,
  from the connection state, the era of the agreed version and the declared
  capabilities. Views, menus and the palette ask it and never read
  capabilities themselves. A control that cannot run stays visible:
  `views::disabled_control` draws it muted, with the reason as its hover
  caption and in the status bar when pressed. What 2026-07-28 deprecates
  (log level, sampling, roots) carries `features::DEPRECATED` where it is
  used on a modern server.
- The log drawer's level dropdown hides server log messages below the
  chosen level, those already shown included, and asks a server that
  declares `logging` to send from that level (`logging/setLevel`; every
  level is `debug`), sent again after a reconnect. A modern session sends no
  `logging/setLevel`: the session keeps the level and puts it in the `_meta`
  of every request, and the server logs only while one runs. Rows without a
  level always pass. A server's `notifications/message` carries its level
  onto the row, and `error` and above are error rows.
- A connect the server turns away for want of authorization keeps its
  `WWW-Authenticate` challenge (`ServerEntry::auth_challenge`), and the next
  OAuth attempt starts from it (`AppState::oauth_options`). Authorize drops
  the stored OAuth credentials and runs the flow again; a server without
  OAuth opens the edit form instead.
- The roots offered to a server are a setting (`roots.{server id}`) read on
  connect. The roots dialog starts from them and answering keeps them; saving
  them in the Server view (⌘5) also sends `notifications/roots/list_changed`
  to a legacy session. A modern session has no such notice and gets them when
  it asks during a request.
- A prompt argument or template variable asks `completion/complete` on each
  change when the server declares `completions`; an answer to an older
  keystroke is dropped.
- Deleting a server, clearing its history and forgetting its credentials
  ask first, through one dialog (`state::Confirm`), whose index follows its
  server when another is deleted; deleting one recorded call
  (`AppState::delete_call`) does not, since the row names what goes. Each
  writes off the GPUI thread and changes what is shown only once the
  database or the keyring agreed.
- The Server view lists the snapshots stored for a server, read the first
  time it is shown, and compares any two in the change banner's terms; File >
  Compare with Snapshot File… shows the diff against a file in the banner.
- A dialog (the request dialog, the confirmation) takes focus while it is
  open, under the `Dialog` key context: Enter and `⌘⏎` accept, Esc cancels,
  and the window's shortcuts are bound to `NoAction` there, so they wait
  until it closes. Closing it gives focus back to the list.
- The detail pane is split once a selection has a response: the header,
  description and toolbar stay put, and the input (form, arguments or
  declaration) and the response scroll each on their own, with a draggable
  separator between them (`views/split.rs`). It starts halfway and is
  remembered per selection, like folds, so the split that suits one tool's
  form is not forced on another's; a deleted server or a cleared call takes
  its splits with it. Before the first call the input has the whole pane.
- A plain-text block of a response is a read-only text area
  (`views/plain.rs`, on gpui-kit's input engine): the text sits in a rope
  and only the lines in view are laid out, so a block of any length costs a
  frame the same, and it can be selected and copied. The text area is kept
  by the block's element id while the block is drawn and its text set again
  only when `kept::Decoded` stamps it as another text. A text that is the
  whole response fills the response panel; beside other blocks it takes its
  own rows, up to `plain::MAX_ROWS`, and scrolls inside. JSON text is still
  a tree.
- Markdown is gpui-kit's text view, which lays out every block of a
  document on every frame unless it scrolls, when it draws the blocks in
  view through a list. So a document scrolls: in the whole panel when it is
  the response, in a box of its own beside other blocks once it is longer
  than `SCROLLED_MARKDOWN_BYTES`, and at its own height only when short and
  beside others. The view parses a long document in the background, so it
  is blank for a moment after the answer.
- The log drawer opens and closes only by its header buttons, `⌘J` and
  `⌘⇧J`, the View menu and the palette, never by Esc: reading a log must not
  end by accident. Zoomed (`drawer_zoomed`), it takes the columns' place
  between the title bar and the status bar; the columns keep their sizes.
  Hiding the drawer drops the zoom, so `⌘J` always brings back the drawer at
  its height. The zoom and show/hide buttons stay in the collapsed header;
  Clear, the filters and the level only show with rows. Neither flag is
  persisted.
- The log drawer follows its newest row (`FollowMode::Tail`) until it is
  scrolled up. Rows open independently of each other (`expanded_log` is a
  set of row ids), so a request reads beside its response. The row a click
  toggles is scrolled to the top of the drawer, opened or closed
  (`LogList::sync`), within the same server's log only; several rows
  closing at once (Clear, the cap) scroll nothing. A headless flow
  addresses rows near the end, or filters the log first, rather than a
  fixed index.
- The menu bar is rebuilt only when what the selected server allows changes
  (`menus::Available`); an item that cannot run is disabled, not hidden.
- Calls are listed by `julianday(at)`, not by the text of `at`: RFC 3339
  drops trailing zeros from the fraction, so text sorts wrongly within one
  second.
- The client declares both elicitation modes. Accepting a URL elicitation
  opens the URL in the browser, so the headless flows decline one.
- The mock server's faults (`--no-templates`, `--broken-resources`,
  `--stalled-resources`, `--ignore-pings`, `--paged-resources`) and tools
  (`progress`, `stderr`, `exit`, `elicit` with a `url`) give each unhappy
  path a test. `exit` only ends a server serving its own process over stdio.
  `rows`, `text` and `markdown` return results as large as asked, for
  trying the window against a long answer of each kind.
- The mock server speaks both eras from one binary, over stdio and HTTP. On
  a 2026-07-28 request `elicit`, `sample` and `roots` answer `input_required`
  (`elicit` with `repeat` keeps asking, to reach the round limit), `bump` and
  `add_resource` notify the open `subscriptions/listen` streams, and `log`
  reads the level from the request. The era faults: `--legacy-only` (no
  discovery, no 2026-07-28), `--modern-only` (2026-07-28 only, no handshake)
  and `--legacy-versions` (discovery that offers only older versions, which
  sends Auto to a second transport). Its HTTP mode keeps one server behind
  every session, since a modern request has none.
  The headless flows fail when `target/debug/mcp-mockserver` is missing;
  `just test-app` builds it first.
