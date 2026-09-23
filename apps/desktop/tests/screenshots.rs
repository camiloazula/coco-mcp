//! Renders the design screens headlessly (macOS/Metal) and writes
//! PNGs to `target/screenshots/` for design review. Also asserts the frame
//! renders and that key elements exist. Skipped on other platforms.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

#[cfg(target_os = "macos")]
#[path = "flows/protocol.rs"]
mod protocol;

#[cfg(target_os = "macos")]
#[path = "flows/stored.rs"]
mod stored;

#[cfg(target_os = "macos")]
#[path = "flows/interaction.rs"]
mod interaction;

#[cfg(target_os = "macos")]
#[path = "flows/unhappy.rs"]
mod unhappy;

#[cfg(target_os = "macos")]
#[path = "flows/eras.rs"]
mod eras;

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;

    use coco_mcp::persistence::Persistence;
    use coco_mcp::state::{AppState, LogRow, Screen, Status};

    /// The row the app files for `event`, from a borrowed event.
    fn log_row(event: &mcp_core::Event) -> Option<LogRow> {
        coco_mcp::state::incoming(event.clone()).row
    }
    use coco_mcp::views::Workspace;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, HeadlessAppContext, point, px, size};
    use mcp_core::{Direction, EventKind, EventSink, ServerSpec};
    use serde_json::json;

    pub(crate) fn context() -> HeadlessAppContext {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            std::sync::Arc::new(coco_mcp::assets::AppAssets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            coco_mcp::theme::install(cx).unwrap();
            coco_mcp::actions::bind(cx);
        });
        cx
    }

    fn out_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn demo_state(connected: bool) -> AppState {
        let mut state = AppState::new(None, None);
        if !connected {
            return state;
        }
        for (name, status) in [
            ("weather", Status::Connected),
            ("files", Status::Off),
            ("github", Status::Error("spawn failed".into())),
        ] {
            let spec = ServerSpec::Stdio {
                command: name.into(),
                args: vec![],
                env: Default::default(),
                cwd: None,
            };
            state.add_demo_server(name, spec, status);
        }
        let weather = &mut state.servers[0];
        weather.set_snapshot(Some(
            serde_json::from_value(json!({
                "protocolVersion": "2025-06-18",
                "serverInfo": {"name": "weather", "version": "0.4.1"},
                "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                "tools": [
                    {"name": "get_weather", "description": "Returns current conditions for a city. Temperature, humidity, wind and a one-word conditions summary; optionally a 24-hour hourly breakdown.",
                     "inputSchema": {"type": "object", "properties": {"city": {"type": "string"}, "units": {"type": "string", "enum": ["metric", "imperial"]}, "include_hourly": {"type": "boolean", "default": false}}, "required": ["city"]}},
                    {"name": "get_forecast", "inputSchema": {"type": "object"}},
                    {"name": "list_stations", "inputSchema": {"type": "object"}},
                    {"name": "search_city", "inputSchema": {"type": "object"}},
                    {"name": "get_alerts", "inputSchema": {"type": "object"}},
                    {"name": "get_air_quality", "inputSchema": {"type": "object"}}
                ],
                "resources": [
                    {"uri": "weather://stations", "name": "stations", "mimeType": "application/json"},
                    {"uri": "weather://readme", "name": "readme", "mimeType": "text/markdown"}
                ],
                "prompts": [{"name": "brief", "arguments": [{"name": "city", "required": true}]}],
                "takenAt": "2026-09-11T12:00:00Z"
            }))
            .unwrap(),
        ));
        weather.last_call_ms = Some(142);
        let sink = EventSink::new(64);
        let events = [
            EventKind::Request {
                direction: Direction::Outbound,
                id: json!(1),
                method: "initialize".into(),
                params: Some(json!({"protocolVersion": "2025-06-18", "capabilities": {}})),
            },
            EventKind::Response {
                direction: Direction::Inbound,
                id: json!(1),
                method: Some("initialize".into()),
                result: json!({"serverInfo": {"name": "weather", "version": "0.4.1"}}),
                elapsed: None,
            },
            EventKind::Notification {
                direction: Direction::Outbound,
                method: "notifications/initialized".into(),
                params: None,
            },
            EventKind::Request {
                direction: Direction::Outbound,
                id: json!(2),
                method: "tools/list".into(),
                params: Some(json!({})),
            },
            EventKind::Response {
                direction: Direction::Inbound,
                id: json!(2),
                method: Some("tools/list".into()),
                result: json!({"tools": [1, 2, 3, 4, 5, 6]}),
                elapsed: None,
            },
            EventKind::Request {
                direction: Direction::Outbound,
                id: json!(3),
                method: "tools/call".into(),
                params: Some(
                    json!({"name": "get_weather", "arguments": {"city": "San Francisco", "units": "metric"}}),
                ),
            },
            EventKind::Response {
                direction: Direction::Inbound,
                id: json!(3),
                method: Some("tools/call".into()),
                result: json!({"content": [{"type": "text", "text": "{\"temp\":17.2}"}]}),
                elapsed: None,
            },
            EventKind::Notification {
                direction: Direction::Inbound,
                method: "notifications/message".into(),
                params: Some(json!({"level": "info", "data": "cache miss, fetched upstream"})),
            },
        ];
        for kind in events {
            let event = sink.emit(kind);
            weather.push_log(log_row(&event).unwrap());
        }
        state.selected_server = Some(0);
        state.selected_item = Some(0);
        state
    }

    fn capture(name: &str, state: AppState, dark: bool) {
        let mut cx = context();
        if !dark {
            cx.update(|cx| coco_mcp::theme::set_dark(false, None, cx));
        }
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let state = cx.new(|_| state);
                let workspace = cx.new(|cx| Workspace::new(state, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("workspace").is_some());
        })
        .unwrap();
        let image = cx
            .capture_screenshot(handle.into())
            .expect("Metal rendering");
        let path = out_dir().join(format!("{name}.png"));
        image.save(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }

    /// Drive the real UI: `+`, type a name and command, Connect, then watch
    /// the session reach `Connected` against the mock server binary (built by
    /// `just test`).
    fn add_server_flow() {
        let mock = mock_binary();
        let mut cx = context();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        // An in-memory database so calls are recorded for the history flow.
        let store = mcp_store::Store::open_in_memory().unwrap();
        let state = cx.update(|cx| cx.new(|_| AppState::new(Some(bridge), Some(store))));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("add-server", cx);
            window.render_frame(cx);
            window.click("name", cx);
            window.input("mock", cx);
            window.click("command", cx);
            window.input(&format!("{} --schema v1", mock.display()), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let image = cx
            .capture_screenshot(handle.into())
            .expect("Metal rendering");
        image
            .save(out_dir().join("04-add-server-typed.png"))
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        // The row is saved off the UI thread; the server appears once it is.
        for _ in 0..100 {
            if cx.update(|cx| !state.read(cx).servers.is_empty()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(("server", 0usize)).is_some(),
                "server row appears"
            );
        })
        .unwrap();
        let mut connected = false;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            let status = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    state.read(cx).servers[0].status.clone()
                })
                .unwrap();
            if status == Status::Connected {
                connected = true;
                break;
            }
        }
        let image = cx
            .capture_screenshot(handle.into())
            .expect("Metal rendering");
        image.save(out_dir().join("06-connected-live.png")).unwrap();
        assert!(connected, "mock server should connect through the bridge");
        drive_calls(&mut cx, handle, &state);
        server_requests_flow(&mut cx, handle, &state);
        history_flow(&mut cx, handle, &state);
        palette_flow(&mut cx, handle, &state);
        // Every call was recorded and both theme choices were written.
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
        }
        assert_eq!(
            cx.update(|cx| state.read(cx).persistence.clone()),
            Persistence::Saved
        );
    }

    /// ⌘K: open, filter, confirm (dispatches the item's action), Esc closes.
    fn palette_flow(
        cx: &mut HeadlessAppContext,
        handle: gpui_kit::WindowHandle<Root>,
        state: &gpui_kit::Entity<AppState>,
    ) {
        use coco_mcp::state::Mode;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("commands", cx);
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("command-palette").is_some(),
                "palette opens"
            );
        })
        .unwrap();
        cx.run_until_parked();
        snap(cx, handle, "21-command-palette");
        cx.update_window(handle.into(), |_, window, cx| {
            window.input("show reso", cx);
            window.render_frame(cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("command-palette").is_none(),
                "palette closes on confirm"
            );
        })
        .unwrap();
        assert_eq!(cx.update(|cx| state.read(cx).mode), Mode::Resources);

        // Esc closes without acting; the theme survives a round trip.
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("cmd-k", cx);
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.try_find("command-palette").is_some());
            window.input("dark", cx);
            window.render_frame(cx);
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(window.try_find("command-palette").is_none(), "esc closes");
            window.press("cmd-t", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(!cx.update(|cx| state.read(cx).dark), "cmd-t flips to light");
        snap(cx, handle, "22-light-live");
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("cmd-t", cx);
        })
        .unwrap();
    }

    /// Index of the list row whose label is `name` in the current mode.
    /// The mock server binary the live flows spawn. A flow that needs it
    /// fails when it was not built rather than passing without running.
    pub(crate) fn mock_binary() -> PathBuf {
        let mock =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/mcp-mockserver");
        assert!(
            mock.exists(),
            "{} is missing: run `cargo build -p mcp-mockserver` first",
            mock.display()
        );
        mock
    }

    pub(crate) fn item_index(
        cx: &mut HeadlessAppContext,
        state: &gpui_kit::Entity<AppState>,
        name: &str,
    ) -> usize {
        cx.update(|cx| {
            state
                .read(cx)
                .items()
                .iter()
                .position(|i| i.label == name || i.name == name)
                .unwrap_or_else(|| panic!("no row named {name}"))
        })
    }

    /// Wait until the server has asked something (a dialog is open).
    pub(crate) fn wait_for_request(
        cx: &mut HeadlessAppContext,
        handle: gpui_kit::WindowHandle<Root>,
        state: &gpui_kit::Entity<AppState>,
    ) {
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
            let pending = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    state.read(cx).current_request().is_some()
                })
                .unwrap();
            if pending {
                return;
            }
        }
        panic!("server request did not arrive");
    }

    fn response_text(cx: &mut HeadlessAppContext, state: &gpui_kit::Entity<AppState>) -> String {
        cx.update(|cx| {
            state.read(cx).response().unwrap().raw["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
    }

    /// The three server-initiated requests, each answered through its dialog.
    fn server_requests_flow(
        cx: &mut HeadlessAppContext,
        handle: gpui_kit::WindowHandle<Root>,
        state: &gpui_kit::Entity<AppState>,
    ) {
        use coco_mcp::calls::ResponseStatus;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("log-header", cx);
            window.click(("mode", 0usize), cx);
            window.render_frame(cx);
        })
        .unwrap();

        // elicitation/create: fill the requested form and accept.
        let elicit = item_index(cx, state, "elicit");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", elicit), cx);
            window.render_frame(cx);
            window.click("$.question", cx);
            window.input("Favourite number?", cx);
            window.click("call", cx);
        })
        .unwrap();
        wait_for_request(cx, handle, state);
        snap(cx, handle, "15-elicitation-dialog");
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.try_find("request-dialog").is_some());
            window.click("$.answer", cx);
            window.input("42", cx);
            window.click("req-accept", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        assert_eq!(response_text(cx, state), "answer=42");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("request-dialog").is_none(), "dialog closes");
        })
        .unwrap();

        // sampling/createMessage: type the assistant reply.
        let sample = item_index(cx, state, "sample");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", sample), cx);
            window.render_frame(cx);
            window.click("$.prompt", cx);
            window.input("Say hi", cx);
            window.click("call", cx);
        })
        .unwrap();
        wait_for_request(cx, handle, state);
        snap(cx, handle, "16-sampling-dialog");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("req-text", cx);
            window.input("Hello from Coco", cx);
            window.click("req-accept", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        let text = response_text(cx, state);
        assert!(text.contains("Hello from Coco"), "{text}");
        assert!(text.contains("coco-mcp"), "{text}");

        // roots/list: one URI per line.
        let roots = item_index(cx, state, "roots");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", roots), cx);
            window.render_frame(cx);
            window.click("call", cx);
        })
        .unwrap();
        wait_for_request(cx, handle, state);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("req-roots", cx);
            window.input("file:///tmp/project", cx);
            window.click("req-accept", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        let text = response_text(cx, state);
        assert!(text.contains("file:///tmp/project"), "{text}");

        // Declining an elicitation reaches the server as `declined`.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", elicit), cx);
            window.render_frame(cx);
            window.click("call", cx);
        })
        .unwrap();
        wait_for_request(cx, handle, state);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("req-decline", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        assert_eq!(response_text(cx, state), "declined");
    }

    /// History mode: rows, detail, replay and "Open in form".
    fn history_flow(
        cx: &mut HeadlessAppContext,
        handle: gpui_kit::WindowHandle<Root>,
        state: &gpui_kit::Entity<AppState>,
    ) {
        use coco_mcp::calls::ResponseStatus;
        use coco_mcp::state::Mode;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("mode", 3usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        // Listed at once for a server saved this session; stored calls of a
        // server saved earlier would arrive off the UI thread.
        for _ in 0..200 {
            if cx.update(|cx| state.read(cx).count(Mode::History).is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        }
        let count = cx.update(|cx| state.read(cx).count(Mode::History).unwrap());
        assert!(count >= 9, "calls were recorded: {count}");
        let newest = cx.update(|cx| state.read(cx).items()[0].label.clone());
        assert_eq!(newest, "elicit", "newest first");
        snap(cx, handle, "17-history");

        // Replay the `add` call: a fresh response appears under the row.
        let add = item_index(cx, state, "add");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", add), cx);
            window.render_frame(cx);
        })
        .unwrap();
        snap(cx, handle, "18-history-detail");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("replay", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        let value = cx.update(|cx| {
            state.read(cx).response().unwrap().raw["structuredContent"]["value"].clone()
        });
        assert_eq!(value, json!(5.5));
        let count_after = cx.update(|cx| state.read(cx).count(Mode::History).unwrap());
        assert_eq!(count_after, count + 1, "the replay is recorded too");

        // Open in form: back to Tools with `add` selected and its arguments loaded.
        let add = item_index(cx, state, "add");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", add), cx);
            window.render_frame(cx);
            window.click("open-call", cx);
            window.render_frame(cx);
        })
        .unwrap();
        let (mode, name) = cx.update(|cx| {
            let s = state.read(cx);
            (s.mode, s.selected_name())
        });
        assert_eq!(mode, Mode::Tools);
        assert_eq!(name.as_deref(), Some("add"));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        let value = cx.update(|cx| {
            state.read(cx).response().unwrap().raw["structuredContent"]["value"].clone()
        });
        assert_eq!(value, json!(5.5), "the loaded arguments were resent");
    }

    /// Connect to schema v1 of the mock with a stored v2 snapshot:
    /// the banner reports the classified changes.
    fn diff_banner_flow() {
        let mock = mock_binary();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let store = mcp_store::Store::open_in_memory().unwrap();
        let spec = ServerSpec::Stdio {
            command: mock.display().to_string(),
            args: vec!["--schema".into(), "v1".into()],
            env: Default::default(),
            cwd: None,
        };
        let record = store
            .add_server("mock", &spec, &mcp_core::ServerRequestPolicy::default())
            .unwrap();
        let previous = futures::executor::block_on(bridge.run(async {
            let spec = ServerSpec::Stdio {
                command: "in-process".into(),
                args: vec![],
                env: Default::default(),
                cwd: None,
            };
            let session = mcp_core::Session::connect_with_transport(
                spec,
                mcp_mockserver::MockServer::serve_duplex(mcp_mockserver::Schema::V2),
                mcp_core::SessionOptions::default(),
            )
            .await
            .unwrap();
            session.snapshot().await.unwrap()
        }))
        .unwrap();
        store.add_snapshot(&record.id, &previous).unwrap();

        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| AppState::new(Some(bridge), Some(store.clone()))));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.connect(0, cx)));
        let mut connected = false;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            let status = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    state.read(cx).servers[0].status.clone()
                })
                .unwrap();
            if status == Status::Connected {
                connected = true;
                break;
            }
            if let Status::Error(e) = status {
                panic!("connect failed: {e}");
            }
        }
        assert!(connected);
        let diff = cx.update(|cx| state.read(cx).visible_diff().cloned().unwrap());
        assert!(diff.has_breaking(), "{}", diff.summary());
        assert!(
            diff.changes()
                .iter()
                .any(|c| c.name == "multiply" && c.summary == "tool removed")
        );
        // The changed snapshot was stored; the store now has two.
        assert_eq!(store.list_snapshots(&record.id, 10).unwrap().len(), 2);
        snap(&mut cx, handle, "19-changed-banner");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("diff-details", cx);
        })
        .unwrap();
        snap(&mut cx, handle, "20-diff-details");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("diff-dismiss", cx);
            window.render_frame(cx);
            assert!(window.try_find("diff-details").is_none());
        })
        .unwrap();
        assert!(cx.update(|cx| state.read(cx).visible_diff().is_none()));

        // Disconnect: the pane shows the server's settings, whose Connect
        // button connects it again.
        let earlier = cx
            .update_window(handle.into(), |_, window, cx| {
                window.press("cmd-shift-r", cx);
                window.render_frame(cx);
                assert_eq!(state.read(cx).servers[0].status, Status::Off);
                assert!(window.try_find("connect").is_some(), "connect button shown");
                assert!(
                    window.try_find("cancel-add").is_none(),
                    "nothing to go back to"
                );
                log_lines(state.read(cx).servers[0].log())
            })
            .unwrap();
        assert!(!earlier.is_empty(), "the first session logged");
        snap(&mut cx, handle, "29-disconnected");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            let status = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    state.read(cx).servers[0].status.clone()
                })
                .unwrap();
            if status == Status::Connected {
                break;
            }
        }
        assert_eq!(
            cx.update(|cx| state.read(cx).servers[0].status.clone()),
            Status::Connected,
            "the button reconnects"
        );
        cx.update(|cx| {
            let log = state.read(cx).servers[0].log();
            assert!(log.len() > earlier.len());
            assert_eq!(
                log_lines(&log[..earlier.len()]),
                earlier,
                "the first session's rows are kept"
            );
            assert!(
                log[earlier.len()].session_break,
                "a separator starts the new session"
            );
        });

        // Edit the server: the form opens prefilled, saving renames it in the
        // sidebar and the store and reconnects.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("edit-server", cx);
            window.render_frame(cx);
            assert_eq!(state.read(cx).editing, Some(0));
            assert!(window.try_find("name").is_some(), "edit form opens");
            assert!(
                window.try_find("command").is_some(),
                "stdio transport prefilled"
            );
        })
        .unwrap();
        snap(&mut cx, handle, "26-edit-server");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("name", cx);
            window.press("cmd-a", cx);
            window.input("mock edited", cx);
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        // Still connected from before until the save lands and reconnects.
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            let (saved, status) = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    let s = state.read(cx);
                    (s.editing.is_none(), s.servers[0].status.clone())
                })
                .unwrap();
            if saved && status == Status::Connected {
                break;
            }
        }
        cx.update(|cx| {
            let s = state.read(cx);
            assert_eq!(
                s.servers[0].status,
                Status::Connected,
                "reconnected after save"
            );
            assert_eq!(s.servers[0].record.name, "mock edited");
            assert!(s.editing.is_none());
        });
        assert_eq!(
            store.get_server(&record.id).unwrap().unwrap().name,
            "mock edited",
            "the store row was updated"
        );

        // Delete the server: the bin opens a confirmation; Cancel keeps it,
        // Delete removes it from the sidebar and the store.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("delete-server", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("confirm-dialog").is_some(),
                "confirmation opens"
            );
        })
        .unwrap();
        snap(&mut cx, handle, "23-delete-confirm");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("confirm-cancel", cx);
            window.render_frame(cx);
            assert!(window.try_find("confirm-dialog").is_none(), "cancel closes");
            assert!(window.try_find(("server", 0usize)).is_some(), "server kept");
        })
        .unwrap();
        // Hover the pencil long enough for its caption to appear.
        cx.update_window(handle.into(), |_, window, cx| {
            window.hover("edit-server", cx)
        })
        .unwrap();
        for _ in 0..12 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            cx.run_until_parked();
            cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
        snap(&mut cx, handle, "27-hover-caption");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("delete-server", cx);
            window.render_frame(cx);
            window.click("confirm-accept", cx);
            window.render_frame(cx);
        })
        .unwrap();
        // The row is deleted off the UI thread; the server leaves the sidebar
        // once it is gone.
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            if cx.update(|cx| state.read(cx).servers.is_empty()) {
                break;
            }
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(("server", 0usize)).is_none(),
                "server removed"
            );
            assert!(
                window.try_find("delete-server").is_none(),
                "no bin without a server"
            );
        })
        .unwrap();
        assert!(cx.update(|cx| state.read(cx).servers.is_empty()));
        assert!(
            store.list_servers().unwrap().is_empty(),
            "store row deleted"
        );
        assert_eq!(
            cx.update(|cx| state.read(cx).persistence.clone()),
            Persistence::Saved,
            "every write of the flow reached the database"
        );
    }

    /// A database that could not be opened: the status bar says so from the
    /// first frame, and nothing is added that could not be kept, not even a
    /// token in the keyring.
    fn persistence_unavailable_flow() {
        let mut cx = context();
        let secrets = std::sync::Arc::new(mcp_auth::MemoryStore::new());
        let mut model = AppState::without_database(
            None,
            "cannot open /nowhere/coco.db: permission denied".into(),
        );
        model.secrets = secrets.clone();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let mut workspace = None;
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                workspace = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let workspace = workspace.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("persistence").is_some(),
                "the status bar says nothing is saved"
            );
            window.click("add-server", cx);
            window.render_frame(cx);
            window.click("name", cx);
            window.input("mock", cx);
            window.click("command", cx);
            window.input("mcp-mockserver --schema v1", cx);
            window.click("connect", cx);
            window.render_frame(cx);
            assert!(window.try_find("form-error").is_some(), "the form says why");
        })
        .unwrap();
        cx.update(|cx| {
            let s = state.read(cx);
            assert!(s.servers.is_empty(), "nothing looks saved");
            assert_eq!(s.screen, Screen::AddServer, "the form stays open");
        });
        snap(&mut cx, handle, "32-persistence-unavailable");

        // A bearer token typed into the form, and one carried by an imported
        // file, stay out of the keyring when their server cannot be saved.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("transport-http", cx);
            window.render_frame(cx);
            window.click("url", cx);
            window.input("https://paid.test/mcp", cx);
            window.click("auth-bearer", cx);
            window.render_frame(cx);
            window.click("token", cx);
            window.input("s3cret", cx);
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| import_small_config(&workspace, cx));
        cx.update(|cx| {
            let s = state.read(cx);
            assert!(s.servers.is_empty(), "an import is refused too");
            assert!(matches!(s.persistence, Persistence::Unavailable(_)));
        });
        assert!(secrets.is_empty(), "no token was left in the keyring");
    }

    /// The database is open but refuses a write: the row of a connected
    /// server went missing, so neither its snapshot nor a call to it can be
    /// stored, and the status bar names the last write that failed.
    fn failed_write_flow() {
        let mock = mock_binary();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let store = mcp_store::Store::open_in_memory().unwrap();
        let spec = ServerSpec::Stdio {
            command: mock.display().to_string(),
            args: vec!["--schema".into(), "v1".into()],
            env: Default::default(),
            cwd: None,
        };
        let record = store
            .add_server("mock", &spec, &mcp_core::ServerRequestPolicy::default())
            .unwrap();
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| AppState::new(Some(bridge), Some(store.clone()))));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("persistence").is_none(),
                "nothing to report while writes succeed"
            );
        })
        .unwrap();

        // The row disappears behind the app's back.
        assert!(store.delete_server(&record.id).unwrap());
        cx.update(|cx| state.update(cx, |s, cx| s.connect(0, cx)));
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            let status = cx.update(|cx| state.read(cx).servers[0].status.clone());
            if status == Status::Connected {
                break;
            }
            if let Status::Error(e) = status {
                panic!("connect failed: {e}");
            }
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let s = state.read(cx);
            assert_eq!(s.servers[0].status, Status::Connected);
            match &s.persistence {
                Persistence::Failed(reason) => assert!(
                    reason.starts_with("snapshot of `mock` was not stored"),
                    "{reason}"
                ),
                other => panic!("expected a failed write, got {other:?}"),
            }
            assert!(
                window.try_find("persistence").is_some(),
                "the status bar shows the failure"
            );
        })
        .unwrap();
        snap(&mut cx, handle, "33-persistence-failed");

        // The server still answers a call, but the call has no row to be
        // recorded against.
        let echo = item_index(&mut cx, &state, "echo");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", echo), cx);
            window.render_frame(cx);
            window.click("$.text", cx);
            window.input("unrecorded", cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(
            wait_for_response(&mut cx, handle, &state),
            coco_mcp::calls::ResponseStatus::Ok
        );
        cx.update(|cx| {
            let s = state.read(cx);
            match &s.persistence {
                Persistence::Failed(reason) => assert!(
                    reason.starts_with("call to `echo` was not recorded"),
                    "{reason}"
                ),
                other => panic!("expected a failed write, got {other:?}"),
            }
            assert!(s.servers[0].history().is_empty(), "nothing was recorded");
        });
    }

    /// An edit the database refuses (a name already taken) keeps the form
    /// open with the reason and leaves the server's bearer token as it was,
    /// although a new one was typed. The row is written off the UI thread, as
    /// in the app, so the form hears of the refusal after the click; with the
    /// name fixed, the same form saves, and headers it did not touch (one a
    /// `Name: value` line cannot hold) are saved as stored.
    fn refused_edit_keeps_its_token_flow() {
        use mcp_auth::SecretStore as _;
        let store = mcp_store::Store::open_in_memory().unwrap();
        let policy = mcp_core::ServerRequestPolicy::default();
        let weather = ServerSpec::Stdio {
            command: "weather".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        store.add_server("weather", &weather, &policy).unwrap();
        let remote = ServerSpec::Http {
            url: "https://remote.test/mcp".into(),
            headers: [("X-Note".to_owned(), " first\nSecond: line".to_owned())].into(),
            auth: mcp_core::AuthRef::Bearer {
                keyring_id: "remote-token".into(),
            },
        };
        let record = store.add_server("remote", &remote, &policy).unwrap();
        let secrets = std::sync::Arc::new(mcp_auth::MemoryStore::new());
        secrets.set("remote-token", "old").unwrap();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let mut model = AppState::new(Some(bridge), Some(store.clone()));
        model.secrets = secrets.clone();
        let remote_ix = model
            .servers
            .iter()
            .position(|s| s.record.id == record.id)
            .unwrap();
        model.selected_server = Some(remote_ix);
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("edit-server", cx);
            window.render_frame(cx);
        })
        .unwrap();
        // The stored token is read off the UI thread and filled in once it is.
        let mut prefilled = None;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
            prefilled = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    window.find("token").value().map(str::to_owned)
                })
                .unwrap();
            if prefilled.as_deref().is_some_and(|token| !token.is_empty()) {
                break;
            }
        }
        assert_eq!(prefilled.as_deref(), Some("old"), "the stored token");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("name", cx);
            window.press("cmd-a", cx);
            window.input("weather", cx);
            window.click("token", cx);
            window.press("cmd-a", cx);
            window.input("new", cx);
            window.click("connect", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("form-error").is_none(),
                "the database has not answered yet"
            );
        })
        .unwrap();
        let mut refused = false;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
            refused = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("form-error").is_some()
                })
                .unwrap();
            if refused {
                break;
            }
        }
        assert!(refused, "the form says why");
        cx.update(|cx| {
            let s = state.read(cx);
            assert_eq!(s.screen, Screen::AddServer, "the form stays open");
            assert_eq!(s.servers[remote_ix].record.name, "remote");
            assert_eq!(s.persistence, Persistence::Saved);
        });
        assert_eq!(
            secrets.get("remote-token").unwrap().as_deref(),
            Some("old"),
            "the server keeps the token it had"
        );
        assert_eq!(secrets.len(), 1);
        let stored = store.get_server(&record.id).unwrap().unwrap();
        assert_eq!(stored.name, "remote");

        // The refusal left the form free to save again.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("name", cx);
            window.press("cmd-a", cx);
            window.input("remote-api", cx);
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        for _ in 0..200 {
            if cx.update(|cx| state.read(cx).screen != Screen::AddServer) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
        }
        cx.update(|cx| {
            let s = state.read(cx);
            assert_eq!(s.screen, Screen::Detail, "the form closes once saved");
            assert!(s.editing.is_none());
            assert_eq!(s.servers[remote_ix].record.name, "remote-api");
            assert_eq!(s.persistence, Persistence::Saved);
        });
        let stored = store.get_server(&record.id).unwrap().unwrap();
        assert_eq!(stored.name, "remote-api");
        assert_eq!(stored.spec, remote, "only the name changed");
        assert_eq!(
            secrets.get("remote-token").unwrap().as_deref(),
            Some("new"),
            "the typed token is stored once the row is"
        );
        assert_eq!(secrets.len(), 1);
    }

    /// The stored bearer token of an edited server is read off the UI thread:
    /// until it arrives the form will not save an empty token, a token typed
    /// before it arrives is kept, and Copy Bearer Token copies it once read.
    fn stored_token_read_later_flow() {
        use mcp_auth::SecretStore as _;
        let store = mcp_store::Store::open_in_memory().unwrap();
        let remote = ServerSpec::Http {
            url: "https://remote.test/mcp".into(),
            headers: Default::default(),
            auth: mcp_core::AuthRef::Bearer {
                keyring_id: "remote-token".into(),
            },
        };
        let policy = mcp_core::ServerRequestPolicy::default();
        store.add_server("remote", &remote, &policy).unwrap();
        let secrets = std::sync::Arc::new(mcp_auth::MemoryStore::new());
        secrets.set("remote-token", "old").unwrap();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let mut model = AppState::new(Some(bridge), Some(store));
        model.secrets = secrets.clone();
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let mut workspace = None;
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                workspace = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let workspace = workspace.unwrap();
        let pump = |cx: &mut HeadlessAppContext| {
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                cx.run_until_parked();
            }
        };

        // Saved before the read lands: refused, and nothing is written.
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("edit-server", cx);
            window.render_frame(cx);
            assert!(
                window.find("token").value().unwrap_or_default().is_empty(),
                "not read on the UI thread"
            );
            window.click("connect", cx);
            window.render_frame(cx);
            assert!(window.try_find("form-error").is_some(), "the form says why");
        })
        .unwrap();
        pump(&mut cx);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("token").value(), Some("old"));
            assert!(
                window.try_find("form-error").is_none(),
                "the refusal goes once the token is read"
            );
            assert_eq!(state.read(cx).screen, Screen::AddServer, "still editing");
        })
        .unwrap();
        assert_eq!(secrets.get("remote-token").unwrap().as_deref(), Some("old"));

        // A token typed before the read lands is not overwritten by it.
        cx.update(|cx| state.update(cx, |s, cx| s.cancel_add_server(cx)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("edit-server", cx);
            window.render_frame(cx);
            window.click("token", cx);
            window.input("typed", cx);
            window.render_frame(cx);
        })
        .unwrap();
        pump(&mut cx);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("token").value(), Some("typed"));
        })
        .unwrap();

        cx.update(|cx| {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("before".into()));
            workspace.update(cx, |ws, cx| ws.copy_bearer_token(cx));
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("before"),
                "not read on the UI thread"
            );
        });
        pump(&mut cx);
        assert_eq!(clipboard(&mut cx).as_deref(), Some("old"));
    }

    /// A saved server's recorded calls are not read at startup. History says
    /// it is reading them, they arrive off the UI thread newest first, and an
    /// export asked for from another mode waits for them.
    fn lazy_history_flow() {
        use coco_mcp::state::{HistoryLoad, Mode};
        use coco_mcp::views::copy::Export;
        let store = mcp_store::Store::open_in_memory().unwrap();
        let policy = mcp_core::ServerRequestPolicy::default();
        for name in ["weather", "files"] {
            let spec = ServerSpec::Stdio {
                command: name.into(),
                args: vec![],
                env: Default::default(),
                cwd: None,
            };
            let record = store.add_server(name, &spec, &policy).unwrap();
            for call in ["first", "second", "third"] {
                store
                    .record_call(mcp_store::NewCall {
                        server_id: record.id.clone(),
                        kind: mcp_store::CallKind::Tool,
                        name: format!("{name}-{call}"),
                        args: json!({}),
                        result: None,
                        status: mcp_store::CallStatus::Ok,
                        error: None,
                        elapsed_ms: 1,
                    })
                    .unwrap();
            }
        }
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let model = AppState::new(Some(bridge), Some(store.clone()));
        let index = |name: &str| {
            model
                .servers
                .iter()
                .position(|s| s.record.name == name)
                .unwrap()
        };
        let (weather, files) = (index("weather"), index("files"));
        let newest_first: Vec<String> = store
            .list_calls(Some(&model.servers[weather].record.id), 10)
            .unwrap()
            .into_iter()
            .map(|call| call.id)
            .collect();
        for entry in &model.servers {
            assert!(entry.history().is_empty(), "nothing read at startup");
            assert_eq!(entry.history_load(), HistoryLoad::Unread);
        }
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let mut workspace = None;
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                workspace = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let workspace = workspace.unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.select_server(weather, cx)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                state.read(cx).servers[weather].history_load(),
                HistoryLoad::Unread,
                "selecting a server in Tools reads nothing"
            );
            window.click(("mode", 3usize), cx);
            window.render_frame(cx);
            assert!(window.try_find("history-loading").is_some(), "reading");
            let s = state.read(cx);
            assert_eq!(s.servers[weather].history_load(), HistoryLoad::Loading);
            assert_eq!(s.count(Mode::History), None);

            // A call recorded while the stored ones are read (here the newest
            // of them, which the read then lists once) stays out of sight:
            // no count, no key selects it, and no detail is drawn for it.
            let id = s.servers[weather].record.id.clone();
            let newest = store.list_calls(Some(&id), 1).unwrap().remove(0);
            state.update(cx, |s, cx| {
                s.servers[weather].push_call(newest);
                assert_eq!(s.list_count(), "", "no count while reading");
                s.move_item(1, cx);
                assert_eq!(s.selected_item, None, "no key selects an undrawn row");
                s.selected_item = Some(0);
                cx.notify();
            });
            window.render_frame(cx);
            assert!(
                window.try_find("history-loading").is_some(),
                "still reading"
            );
            assert!(window.try_find("open-call").is_none(), "no call detail");
        })
        .unwrap();
        let wait_until_read = |cx: &mut HeadlessAppContext, ix: usize| {
            for _ in 0..200 {
                if cx.update(|cx| state.read(cx).servers[ix].history_load() == HistoryLoad::Read) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                cx.run_until_parked();
            }
            panic!("history was not read");
        };
        wait_until_read(&mut cx, weather);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("history-loading").is_none());
            assert!(window.try_find(("item", 0usize)).is_some(), "rows drawn");
            assert!(
                window.try_find("open-call").is_some(),
                "the selected call is shown once read"
            );
            assert_eq!(
                state.read(cx).list_count(),
                format!("{0}/{0}", newest_first.len())
            );
        })
        .unwrap();
        let (listed, exported) = cx.update(|cx| {
            let names: Vec<String> = state
                .read(cx)
                .items()
                .iter()
                .map(|i| i.name.clone())
                .collect();
            (
                names,
                workspace.read_with(cx, |ws, cx| ws.export(Export::History, cx)),
            )
        });
        assert_eq!(listed, newest_first, "newest first");
        let (_, body) = exported.expect("the read history exports");
        assert_eq!(String::from_utf8(body).unwrap().lines().count(), 3);

        // From Tools, another saved server's history export starts reading
        // its calls and waits for them, and stays that server's export when
        // the selection moves on before they arrive.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("mode", 0usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.select_server(files, cx)));
        let mut export =
            cx.update(|cx| workspace.update(cx, |ws, cx| ws.export_ready(Export::History, cx)));
        assert_eq!(export.try_recv(), Ok(None), "waits for the read");
        cx.update(|cx| {
            assert_eq!(
                state.read(cx).servers[files].history_load(),
                HistoryLoad::Loading,
                "the export started the read"
            );
            state.update(cx, |s, cx| s.select_server(weather, cx));
        });
        wait_until_read(&mut cx, files);
        cx.run_until_parked();
        let (name, body) = export
            .try_recv()
            .unwrap()
            .expect("resolved once read")
            .expect("the files history exports");
        assert_eq!(name, "files-history.jsonl", "the server it was asked for");
        let body = String::from_utf8(body).unwrap();
        assert_eq!(body.lines().count(), 3);
        assert!(body.lines().all(|line| line.contains("files-")));
        let mut again = cx.update(|cx| state.update(cx, |s, cx| s.history_ready(files, cx)));
        assert_eq!(
            again.try_recv(),
            Ok(Some(())),
            "a read history is ready at once"
        );
    }

    /// A server whose resource list is broken and which never implemented
    /// templates: the session is kept, the resources view says why it is
    /// empty, the partial snapshot does not become the baseline, and tools
    /// still answer.
    fn tolerant_connect_flow() {
        use coco_mcp::calls::ResponseStatus;
        use coco_mcp::state::Mode;
        let mock = mock_binary();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let store = mcp_store::Store::open_in_memory().unwrap();
        let spec = ServerSpec::Stdio {
            command: mock.display().to_string(),
            args: ["--schema", "v1", "--no-templates", "--broken-resources"]
                .map(String::from)
                .to_vec(),
            env: Default::default(),
            cwd: None,
        };
        let record = store
            .add_server("mock", &spec, &mcp_core::ServerRequestPolicy::default())
            .unwrap();
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| AppState::new(Some(bridge), Some(store.clone()))));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.connect(0, cx)));
        let mut connected = false;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            let status = cx.update(|cx| state.read(cx).servers[0].status.clone());
            if status == Status::Connected {
                connected = true;
                break;
            }
            if let Status::Error(e) = status {
                panic!("a failing list failed the connect: {e}");
            }
        }
        assert!(connected, "the server connects");
        cx.update(|cx| {
            let s = state.read(cx);
            let failures = s.list_failures(Mode::Resources);
            assert_eq!(failures.len(), 1, "{failures:?}");
            assert_eq!(failures[0].method, "resources/list");
            assert!(s.list_failures(Mode::Tools).is_empty());
            let status = s.status_text();
            assert!(status.contains("resources/list failed"), "{status}");
            assert_eq!(s.persistence, Persistence::Saved);
        });
        assert!(
            store.list_snapshots(&record.id, 10).unwrap().is_empty(),
            "a partial snapshot is not stored"
        );
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("list-failure").is_none(), "tools were read");
            window.click(("mode", 1usize), cx);
            window.render_frame(cx);
            assert_eq!(state.read(cx).mode, Mode::Resources);
            assert!(
                window.try_find("list-failure").is_some(),
                "the list column says why it is empty"
            );
            assert!(
                window.try_find("list-failure-detail").is_some(),
                "so does the detail pane"
            );
        })
        .unwrap();
        snap(&mut cx, handle, "34-list-failed");

        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("mode", 0usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let echo = item_index(&mut cx, &state, "echo");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", echo), cx);
            window.render_frame(cx);
            window.click("$.text", cx);
            window.input("still here", cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(
            wait_for_response(&mut cx, handle, &state),
            ResponseStatus::Ok
        );
    }

    /// A mode with a failed list beside one that was read: the list column
    /// names the failure above the items that arrived, and the detail pane
    /// asks for a selection instead of saying nothing could be listed, also
    /// once the filter hides every row.
    fn failed_list_beside_items_flow() {
        use coco_mcp::state::Mode;
        let mut model = AppState::new(None, None);
        let spec = ServerSpec::Stdio {
            command: "files".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        let ix = model.add_demo_server("files", spec, Status::Connected);
        let snapshot = serde_json::from_value(json!({
            "protocolVersion": "2025-06-18",
            "serverInfo": {"name": "files", "version": "1.0.0"},
            "capabilities": {"resources": {}},
            "resourceTemplates": [{"uriTemplate": "file:///{path}", "name": "file"}],
            "listFailures": [{"method": "resources/list", "error": "internal error"}],
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap();
        model.servers[ix].set_snapshot(Some(snapshot));
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("mode", 1usize), cx);
            window.render_frame(cx);
            assert_eq!(state.read(cx).mode, Mode::Resources);
            assert!(
                window.try_find("list-failure").is_some(),
                "the list column names the failure"
            );
            assert!(
                window.try_find(("item", 0usize)).is_some(),
                "above the template that was read"
            );
            assert!(
                window.try_find("list-failure-detail").is_none(),
                "the detail pane asks for a selection"
            );
            window.click("filter", cx);
            window.input("no such resource", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(state.read(cx).filter, "no such resource");
            assert!(
                window.try_find(("item", 0usize)).is_none(),
                "the filter hides the template"
            );
            assert!(
                window.try_find("list-failure-detail").is_none(),
                "a hidden row is still there to select"
            );
        })
        .unwrap();
    }

    /// A server with three thousand tools: the list builds only the rows in
    /// view, a wheel brings later rows in, and a key that moves the selection
    /// past the bottom edge scrolls it back into view. A recorded call opened
    /// in the form selects its tool far down the list and scrolls it into view.
    fn long_list_flow() {
        use coco_mcp::state::Mode;

        let mut model = demo_state(true);
        let tools: Vec<_> = (0..3_000)
            .map(|i| json!({"name": format!("tool-{i:04}"), "inputSchema": {"type": "object"}}))
            .collect();
        let snapshot = serde_json::from_value(json!({
            "protocolVersion": "2025-06-18",
            "serverInfo": {"name": "weather", "version": "0.4.1"},
            "capabilities": {"tools": {}},
            "tools": tools,
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap();
        model.servers[0].set_snapshot(Some(snapshot));
        let server_id = model.servers[0].record.id.clone();
        model.servers[0].push_call(mcp_store::CallRecord {
            id: "call-far".into(),
            server_id,
            kind: mcp_store::CallKind::Tool,
            name: "tool-2500".into(),
            args: json!({}),
            result: Some(json!({"content": []})),
            status: mcp_store::CallStatus::Ok,
            error: None,
            elapsed_ms: 3,
            at: time::OffsetDateTime::UNIX_EPOCH,
        });
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("item", 0usize)).is_some());
            assert!(
                window.try_find(("item", 2999usize)).is_none(),
                "rows out of view are not built"
            );
            window.click(("item", 3usize), cx);
            assert_eq!(state.read(cx).selected_name().as_deref(), Some("tool-0003"));

            let down = gpui_kit::point(px(0.), px(-2800.));
            window.scroll("item-rows", gpui_kit::ScrollDelta::Pixels(down), cx);
            window.render_frame(cx);
            assert!(window.try_find(("item", 100usize)).is_some(), "scrolled");
            assert!(window.try_find(("item", 0usize)).is_none());

            window.click(("item", 100usize), cx);
            for _ in 0..40 {
                window.press("down", cx);
            }
            window.render_frame(cx);
            assert_eq!(state.read(cx).selected_item, Some(140));
            let row = window.find(("item", 140usize));
            assert!(row.visible(), "the selection is scrolled into view");

            state.update(cx, |s, cx| {
                s.set_mode(Mode::History, cx);
                let ix = s.items().iter().position(|i| i.name == "call-far");
                s.select_item(ix.unwrap(), cx);
            });
            window.render_frame(cx);
            window.click("open-call", cx);
            window.render_frame(cx);
            let s = state.read(cx);
            assert_eq!(s.mode, Mode::Tools);
            assert_eq!(s.selected_name().as_deref(), Some("tool-2500"));
            let ix = s.selected_item.unwrap();
            let row = window.find(("item", ix));
            assert!(
                row.visible(),
                "the tool opened by name is scrolled into view"
            );
        })
        .unwrap();
    }

    /// An imported stdio server saved through the edit form without a
    /// change keeps its arguments (one with a space, one with line breaks a
    /// single-line input cannot show), its working directory and its
    /// environment (a certificate whose lines a `KEY=value` line cannot hold,
    /// one of them ending in base64 padding, and a value with surrounding
    /// spaces). Retyped with shell quoting, each quoted argument stays whole
    /// and the untouched environment stays as stored; a line a shell could
    /// not split is refused.
    fn edit_form_round_trip_flow() {
        let store = mcp_store::Store::open_in_memory().unwrap();
        let stored_env: std::collections::BTreeMap<String, String> = [
            ("LOG", "debug"),
            (
                "CERT",
                "-----BEGIN CERTIFICATE-----\nMIIBAB==\n-----END CERTIFICATE-----",
            ),
            ("PADDED", "  spaced  "),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .into();
        let spec = ServerSpec::Stdio {
            command: "srv".into(),
            args: ["--verbose", "server name", "it's", "{\n  \"a\": 1\n}"]
                .map(String::from)
                .to_vec(),
            env: stored_env.clone(),
            cwd: Some("/tmp/work dir".into()),
        };
        let record = store
            .add_server("files", &spec, &mcp_core::ServerRequestPolicy::default())
            .unwrap();
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| AppState::new(None, Some(store.clone()))));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("edit-server", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("cwd").is_some(),
                "the working directory has a field"
            );
        })
        .unwrap();
        snap(&mut cx, handle, "35-edit-server-cwd");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.update(|cx| state.read(cx).editing.is_none()), "saved");
        assert_eq!(
            store.get_server(&record.id).unwrap().unwrap().spec,
            spec,
            "an untouched save changes nothing"
        );

        cx.update_window(handle.into(), |_, window, cx| {
            window.click("edit-server", cx);
            window.render_frame(cx);
            window.click("command", cx);
            window.press("cmd-a", cx);
            window.input(r#"srv --verbose "other name" 'it'\''s'"#, cx);
            window.click("cwd", cx);
            window.press("cmd-a", cx);
            window.input("/srv/other dir", cx);
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        match store.get_server(&record.id).unwrap().unwrap().spec {
            ServerSpec::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                assert_eq!(command, "srv");
                assert_eq!(args, ["--verbose", "other name", "it's"]);
                assert_eq!(env, stored_env, "the untouched environment stays as stored");
                assert_eq!(cwd, Some(PathBuf::from("/srv/other dir")));
            }
            other => panic!("expected a stdio spec, got {other:?}"),
        }

        cx.update_window(handle.into(), |_, window, cx| {
            window.click("add-server", cx);
            window.render_frame(cx);
            window.click("name", cx);
            window.input("broken", cx);
            window.click("command", cx);
            window.input(r#"srv "unclosed"#, cx);
            window.click("connect", cx);
            window.render_frame(cx);
            assert!(window.try_find("form-error").is_some(), "the form says why");
        })
        .unwrap();
        cx.update(|cx| {
            let s = state.read(cx);
            assert_eq!(s.servers.len(), 1, "nothing was added");
            assert_eq!(s.screen, Screen::AddServer, "the form stays open");
        });
        assert_eq!(store.list_servers().unwrap().len(), 1);
    }

    /// The form belongs to the server it was opened for. `+` while a server
    /// is being edited opens an empty form that adds one; switching from
    /// adding to editing prefills the form, working directory included.
    fn form_follows_its_target_flow() {
        let store = mcp_store::Store::open_in_memory().unwrap();
        let spec = ServerSpec::Stdio {
            command: "srv".into(),
            args: vec!["--verbose".into(), "server name".into()],
            env: Default::default(),
            cwd: Some("/tmp/work dir".into()),
        };
        let record = store
            .add_server("files", &spec, &mcp_core::ServerRequestPolicy::default())
            .unwrap();
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| AppState::new(None, Some(store.clone()))));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("server", 0usize), cx);
            window.render_frame(cx);
            window.click("edit-server", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("cwd").value(), Some("/tmp/work dir"));
            window.click("add-server", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(state.read(cx).editing, None);
            assert!(
                window.find("cwd").value().unwrap_or_default().is_empty(),
                "the form no longer shows the edited server"
            );
            window.click("name", cx);
            window.input("other", cx);
            window.click("command", cx);
            window.input("srv other", cx);
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        for _ in 0..100 {
            if cx.update(|cx| state.read(cx).servers.len() == 2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        }
        assert_eq!(
            store.list_servers().unwrap().len(),
            2,
            "Save added a server"
        );
        assert_eq!(
            store.get_server(&record.id).unwrap().unwrap().spec,
            spec,
            "the server that was being edited is unchanged"
        );

        // The pencil is hidden while the form is open, so the switch from
        // adding to editing comes from the model.
        let files = cx.update(|cx| {
            let s = state.read(cx);
            s.servers.iter().position(|e| e.record.id == record.id)
        });
        let files = files.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("server", files), cx);
            window.render_frame(cx);
            window.click("add-server", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("edit-server").is_none());
            assert!(window.find("cwd").value().unwrap_or_default().is_empty());
        })
        .unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.show_edit_selected(cx)));
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("cwd").value(),
                Some("/tmp/work dir"),
                "the form is prefilled"
            );
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        for _ in 0..100 {
            if cx.update(|cx| state.read(cx).editing.is_none()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        }
        cx.update(|cx| {
            let s = state.read(cx);
            assert_eq!(s.editing, None, "saved in place");
            assert_eq!(s.screen, Screen::Detail);
            assert_eq!(s.servers.len(), 2);
        });
        assert_eq!(store.list_servers().unwrap().len(), 2);
        assert_eq!(store.get_server(&record.id).unwrap().unwrap().spec, spec);
    }

    /// A server that exits while starting: its stderr says why, and Retry
    /// keeps that explanation above a separator instead of clearing it.
    fn failed_connect_keeps_its_log_flow() {
        use coco_mcp::state::LogFilter;
        let mock = mock_binary();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let mut model = AppState::new(Some(bridge), None);
        model.add_demo_server(
            "mock",
            ServerSpec::Stdio {
                command: mock.display().to_string(),
                args: vec!["--bogus".into()],
                env: Default::default(),
                cwd: None,
            },
            Status::Off,
        );
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.connect(0, cx)));
        let first = wait_for_failed_start(&mut cx, &state, 0);
        assert!(
            first.iter().all(|r| !r.session_break),
            "a first connect has nothing to separate"
        );

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("connect-server", cx);
            window.render_frame(cx);
        })
        .unwrap();
        let log = wait_for_failed_start(&mut cx, &state, first.len() + 1);
        assert_eq!(
            log_lines(&log[..first.len()]),
            log_lines(&first),
            "the failed session's stderr and failure are kept"
        );
        assert!(log[first.len()].session_break, "a separator follows them");

        let separator = ("log-row", first.len());
        // The drawer follows the newest rows; the Errors filter keeps both
        // sessions short enough for the separator to stay in view.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("log-header", cx);
            window.render_frame(cx);
            window.click(LogFilter::Errors.label(), cx);
            window.render_frame(cx);
            assert!(window.try_find(separator).is_some(), "the drawer draws it");
        })
        .unwrap();
        snap(&mut cx, handle, "36-log-session-break");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(LogFilter::Stderr.label(), cx);
            window.render_frame(cx);
            let state = state.read(cx);
            assert_eq!(state.log_filter, LogFilter::Stderr);
            assert!(
                state.visible_log().contains(&first.len()),
                "a filter does not hide where the sessions meet"
            );
        })
        .unwrap();
    }

    /// A server that sends a burst: everything already queued is filed in one
    /// change. Clear lets go of the folds of the rows it removed, and deleting
    /// the server lets go of everything kept for it.
    fn chatty_server_flow() {
        use coco_mcp::calls::ResponseStatus;
        use coco_mcp::state::{Changed, Dir, Mode};
        use coco_mcp::views::fold_prefix;
        use mcp_store::{CallKind, CallRecord, CallStatus};
        use std::cell::Cell;
        use std::rc::Rc;
        use std::time::Duration;

        let mock = mock_binary();
        let stdio = |command: String, args: Vec<String>| ServerSpec::Stdio {
            command,
            args,
            env: Default::default(),
            cwd: None,
        };
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let mut model = AppState::new(Some(bridge), None);
        let schema = vec!["--schema".into(), "v1".into()];
        model.add_demo_server(
            "chatty",
            stdio(mock.display().to_string(), schema),
            Status::Off,
        );
        model.add_demo_server("quiet", stdio("quiet".into(), vec![]), Status::Off);
        let mut cx = context();
        let mut workspace = None;
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                workspace = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let workspace = workspace.unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.connect(0, cx)));
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(50));
            cx.run_until_parked();
            if cx.update(|cx| state.read(cx).servers[0].status == Status::Connected) {
                break;
            }
        }
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(25));
            cx.run_until_parked();
        }
        let (id, quiet, logged) = cx.update(|cx| {
            let s = state.read(cx);
            assert_eq!(s.servers[0].status, Status::Connected);
            let ids = (
                s.servers[0].record.id.clone(),
                s.servers[1].record.id.clone(),
            );
            (ids.0, ids.1, s.servers[0].log().len())
        });

        // Fifty notifications and the call around them, queued before the app
        // gets a turn: the log takes them in one change, not fifty-two.
        let changes = Rc::new(Cell::new(0));
        let counted = changes.clone();
        let counting = cx.update(|cx| {
            cx.subscribe(&state, move |_, _: &Changed, _| {
                counted.set(counted.get() + 1)
            })
        });
        let key = (id.clone(), Mode::Tools, "log".to_owned());
        let burst = json!({"level": "info", "message": "tick", "count": 50});
        cx.update(|cx| state.update(cx, |s, cx| s.call_tool("log".into(), burst, cx)));
        std::thread::sleep(Duration::from_millis(300));
        let mut answered = false;
        for _ in 0..200 {
            cx.run_until_parked();
            let (status, rows) = cx.update(|cx| {
                let s = state.read(cx);
                let status = s.responses.get(&key).map(|r| r.status.clone());
                (status, s.servers[0].log().len())
            });
            if status == Some(ResponseStatus::Ok) && rows >= logged + 52 {
                answered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        drop(counting);
        assert!(answered, "the call and its fifty notifications were logged");
        assert!(
            changes.get() < 10,
            "a burst of 52 messages took {} changes",
            changes.get()
        );

        // A payload fold is kept by row id, and goes with the row on Clear.
        // The drawer follows the newest rows, and among the requests the call
        // is one of those.
        let request = cx.update(|cx| {
            state.read(cx).servers[0]
                .log()
                .iter()
                .rposition(|r| r.dir == Dir::Out && r.method == "tools/call")
                .unwrap()
        });
        let root = cx
            .update_window(handle.into(), |_, window, cx| {
                window.click("log-header", cx);
                window.render_frame(cx);
                window.click(coco_mcp::state::LogFilter::Requests.label(), cx);
                window.render_frame(cx);
                window.click(("log-row", request), cx);
                window.render_frame(cx);
                let row_id = state.read(cx).servers[0].log()[request].row_id;
                assert!(state.read(cx).expanded_log.contains(&row_id));
                let root = format!("{}$", fold_prefix(&id, row_id));
                window.click(root.clone(), cx);
                window.render_frame(cx);
                root
            })
            .unwrap();
        assert!(cx.update(|cx| workspace.read(cx).collapsed.contains(&root)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("clear-log", cx);
            window.render_frame(cx);
            window.click(coco_mcp::state::LogFilter::All.label(), cx);
            window.render_frame(cx);
        })
        .unwrap();
        // The workspace hears what left once the click's update has finished.
        cx.update(|cx| {
            assert!(
                !workspace.read(cx).collapsed.contains(&root),
                "Clear let go of its rows' folds"
            );
            assert!(state.read(cx).expanded_log.is_empty());
        });
        // Row ids are per server, so another server's log opens with none.
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.toggle_log_row(s.servers[0].next_log_id(), cx);
                s.select_server(1, cx);
            });
            assert!(state.read(cx).expanded_log.is_empty());
            state.update(cx, |s, cx| s.select_server(0, cx));
        });

        // What the workspace kept for both servers, and a recorded call.
        cx.update(|cx| {
            state.update(cx, |s, _| {
                s.servers[0].push_call(CallRecord {
                    id: "call-chatty".into(),
                    server_id: id.clone(),
                    kind: CallKind::Tool,
                    name: "log".into(),
                    args: json!({}),
                    result: None,
                    status: CallStatus::Ok,
                    error: None,
                    elapsed_ms: 1,
                    at: time::OffsetDateTime::UNIX_EPOCH,
                });
            });
            workspace.update(cx, |ws, _| {
                for key in [
                    format!("{}$", fold_prefix(&id, 60)),
                    format!("schema:{id}:log$"),
                    "hist:call-chatty$".to_owned(),
                    format!("schema:{quiet}:log$"),
                ] {
                    ws.collapsed.insert(key);
                }
            });
        });
        // A request the server is waiting on when it is deleted.
        let elicit = (id.clone(), Mode::Tools, "elicit".to_owned());
        let question = json!({"question": "Still there?"});
        cx.update(|cx| state.update(cx, |s, cx| s.call_tool("elicit".into(), question, cx)));
        wait_for_request(&mut cx, handle, &state);
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.request_delete_selected(cx);
                s.confirm(cx);
            })
        });
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(25));
            cx.run_until_parked();
            if cx.update(|cx| state.read(cx).servers.len() == 1) {
                break;
            }
        }
        // The cancelled call comes back too, and must not bring anything back.
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(25));
            cx.run_until_parked();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("request-dialog").is_none(),
                "the dialog left with its server"
            );
            let s = state.read(cx);
            assert_eq!(s.servers.len(), 1, "the server was deleted");
            assert!(s.pending.is_empty(), "its request was dropped");
            assert!(
                s.responses.get(&key).is_none(),
                "its responses were dropped"
            );
            assert!(s.responses.get(&elicit).is_none());
            assert!(!s.responses.is_pending(&elicit));
            let ws = workspace.read(cx);
            assert!(
                ws.collapsed
                    .iter()
                    .all(|k| !k.contains(&id) && !k.contains("call-chatty")),
                "{:?}",
                ws.collapsed
            );
            assert!(ws.collapsed.contains(&format!("schema:{quiet}:log$")));
        })
        .unwrap();
    }

    /// A large result is drawn as soon as it is selected: every container of
    /// its tree open, only the lines in view built, and the tree scrolls,
    /// folds and copies.
    fn large_result_flow() {
        use mcp_store::{CallKind, CallRecord, CallStatus};

        let mut cx = context();
        let mut state = demo_state(true);
        let server_id = state.servers[0].record.id.clone();
        let call = |id: &str, name: &str, result: serde_json::Value| CallRecord {
            id: id.into(),
            server_id: server_id.clone(),
            kind: CallKind::Tool,
            name: name.into(),
            args: json!({}),
            result: Some(result),
            status: CallStatus::Ok,
            error: None,
            elapsed_ms: 40,
            at: time::OffsetDateTime::UNIX_EPOCH,
        };
        // Twenty thousand rows, led by an index longer than a screen.
        let rows: Vec<_> = (0..20_000).map(|i| json!({"id": i, "ok": true})).collect();
        let large = json!({"ids": (0..2_100).collect::<Vec<_>>(), "rows": rows});
        // Three thousand numbers: small in bytes, long in lines.
        let small = json!({"values": (0..3_000).collect::<Vec<_>>()});
        state.servers[0].push_call(call("call-large", "list_rows", large));
        state.servers[0].push_call(call("call-small", "list_values", small));

        let state = cx.update(|cx| cx.new(|_| state));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("mode", 3usize), cx);
            window.render_frame(cx);
        })
        .unwrap();

        let root = "hist:call-large$";
        let large_row = item_index(&mut cx, &state, "list_rows");
        let row = |i: usize| format!("rowhist:call-large$.rows[{i}]");
        let ids = "hist:call-large$.ids";
        let first_id = "rowhist:call-large$.ids[0]";
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", large_row), cx);
            window.render_frame(cx);
            assert!(window.try_find(root).is_some(), "drawn as selected");
            // Every container starts open, and only the lines in view are
            // built: the index fills the panel, and the rows are below it.
            assert!(window.try_find(first_id).is_some());
            assert!(window.try_find(row(0)).is_none(), "out of view");
            assert!(window.try_find(row(19_999)).is_none());
            // Folding the index brings the rows up; opening it again does
            // the reverse.
            window.click(ids, cx);
            window.render_frame(cx);
            assert!(window.try_find(first_id).is_none(), "folded");
            assert!(
                window.try_find(row(0)).is_some(),
                "the rows follow the folded index"
            );
            assert!(window.try_find(row(19_999)).is_none(), "still out of view");
            window.click(ids, cx);
            window.render_frame(cx);
            assert!(window.try_find(first_id).is_some(), "open again");
            assert!(window.try_find(row(0)).is_none());
            window.click(ids, cx);
            window.render_frame(cx);
            window.right_click("rowhist:call-large$.rows", cx);
            window.render_frame(cx);
        })
        .unwrap();
        let entries = cx
            .update(|cx| coco_mcp::clip::menu(cx))
            .expect("a line has its copy menu")
            .0;
        let path = entries
            .iter()
            .find(|e| e.label == "Copy path")
            .map(|e| e.text.clone());
        assert_eq!(path.as_deref(), Some("$.rows"));
        cx.update(coco_mcp::clip::close_menu);

        // A wheel brings the end of the rows into view, and folding the rows
        // takes them all away.
        let rows = "hist:call-large$.rows";
        cx.update_window(handle.into(), |_, window, cx| {
            let bottom = gpui_kit::point(px(0.), px(-4_000_000.));
            window.scroll(row(0), gpui_kit::ScrollDelta::Pixels(bottom), cx);
            window.render_frame(cx);
            assert!(
                window.try_find(row(19_999)).is_some(),
                "scrolled to the end"
            );
            assert!(
                window.try_find(row(0)).is_none(),
                "the start is out of view"
            );
            let top = gpui_kit::point(px(0.), px(4_000_000.));
            window.scroll(row(19_999), gpui_kit::ScrollDelta::Pixels(top), cx);
            window.render_frame(cx);
            assert!(window.try_find(row(0)).is_some(), "and back");
            window.click(rows, cx);
            window.render_frame(cx);
            assert!(window.try_find(row(0)).is_none(), "folded");
            assert!(window.try_find(rows).is_some(), "on its one line");
        })
        .unwrap();
        snap(&mut cx, handle, "37-large-result");

        // A result small in bytes is drawn at once, every container open,
        // and still only as far as the view reaches.
        let small_row = item_index(&mut cx, &state, "list_values");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", small_row), cx);
            window.render_frame(cx);
            assert!(window.try_find("hist:call-small$").is_some());
            assert!(window.try_find("rowhist:call-small$.values[0]").is_some());
            assert!(
                window
                    .try_find("rowhist:call-small$.values[2999]")
                    .is_none(),
                "only the lines in view are built"
            );
        })
        .unwrap();
        // The folds are remembered for the row.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", large_row), cx);
            window.render_frame(cx);
            assert!(window.try_find(root).is_some(), "still shown");
            assert!(window.try_find(row(0)).is_none(), "still folded");
        })
        .unwrap();
    }

    /// A tool with a large answer, called live: the response is drawn as it
    /// arrives, open from its first row, and copies whole.
    fn live_large_result_flow() {
        use coco_mcp::calls::ResponseStatus;
        use std::time::Duration;

        let mock = mock_binary();
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let mut model = AppState::new(Some(bridge), None);
        let spec = ServerSpec::Stdio {
            command: mock.display().to_string(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        model.add_demo_server("mock", spec, Status::Off);
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| model));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        cx.update(|cx| state.update(cx, |s, cx| s.connect(0, cx)));
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(50));
            cx.run_until_parked();
            if cx.update(|cx| state.read(cx).servers[0].status == Status::Connected) {
                break;
            }
        }
        cx.update(|cx| assert_eq!(state.read(cx).servers[0].status, Status::Connected));

        let rows = item_index(&mut cx, &state, "rows");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("item", rows), cx);
            window.render_frame(cx);
            window.click("$.count", cx);
            window.input("20000", cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(
            wait_for_response(&mut cx, handle, &state),
            ResponseStatus::Ok
        );
        let prefix = cx.update(|cx| {
            let (server, mode, name) = state.read(cx).response_key().unwrap();
            format!("resp:{server}:{mode:?}:{name}")
        });
        let tree = format!("{prefix}c0$");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(tree.clone()).is_some(),
                "drawn as it arrives"
            );
            assert!(
                window.try_find(format!("row{prefix}c0$[0]")).is_some(),
                "open from the first row"
            );
            assert!(
                window.try_find(format!("row{prefix}c0$[19999]")).is_none(),
                "only the lines in view are built"
            );
            window.click(format!("{prefix}-copy"), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let copied: serde_json::Value =
            serde_json::from_str(&clipboard(&mut cx).unwrap_or_default()).unwrap();
        let text = copied["content"][0]["text"].as_str().unwrap_or_default();
        let listed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            listed.as_array().map(Vec::len),
            Some(20_000),
            "the header copies the whole answer"
        );
        cx.update(|cx| state.update(cx, |s, cx| s.disconnect(0, cx)));
        cx.run_until_parked();
    }

    /// Wait until server 0 has failed to start and the log holds, from row
    /// `from` on, both its failure and the reason it wrote to stderr.
    fn wait_for_failed_start(
        cx: &mut HeadlessAppContext,
        state: &gpui_kit::Entity<AppState>,
        from: usize,
    ) -> Vec<LogRow> {
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            let (failed, log) = cx.update(|cx| {
                let entry = &state.read(cx).servers[0];
                (
                    matches!(entry.status, Status::Error(_)),
                    entry.log().to_vec(),
                )
            });
            let rows = log.get(from..).unwrap_or_default();
            let explained = rows
                .iter()
                .any(|r| r.method == "stderr" && r.body.contains("unknown argument"));
            if failed && explained && rows.iter().any(|r| r.method == "failed") {
                return log;
            }
        }
        panic!("the server did not fail with its reason in the log");
    }

    /// What identifies a log row across renders: its clock, method and body.
    fn log_lines(log: &[LogRow]) -> Vec<(String, String, String)> {
        log.iter()
            .map(|r| (r.time.clone(), r.method.clone(), r.body.clone()))
            .collect()
    }

    /// Wait until the current selection has a finished response.
    fn wait_for_response(
        cx: &mut HeadlessAppContext,
        handle: gpui_kit::WindowHandle<Root>,
        state: &gpui_kit::Entity<AppState>,
    ) -> coco_mcp::calls::ResponseStatus {
        use coco_mcp::calls::ResponseStatus;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
            let status = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    state.read(cx).response().map(|r| r.status.clone())
                })
                .unwrap();
            match status {
                Some(ResponseStatus::Pending) | None => continue,
                Some(done) => return done,
            }
        }
        panic!("response did not arrive");
    }

    pub(crate) fn snap(
        cx: &mut HeadlessAppContext,
        handle: gpui_kit::WindowHandle<Root>,
        name: &str,
    ) {
        cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        cx.capture_screenshot(handle.into())
            .unwrap()
            .save(out_dir().join(format!("{name}.png")))
            .unwrap();
    }

    /// Tool call with typed arguments, nested form, Markdown and PNG
    /// resources, and a prompt, all through the real UI.
    fn drive_calls(
        cx: &mut HeadlessAppContext,
        handle: gpui_kit::WindowHandle<Root>,
        state: &gpui_kit::Entity<AppState>,
    ) {
        use coco_mcp::calls::ResponseStatus;
        use coco_mcp::state::Mode;
        // Rows are found by name: the mock's lists grow.
        let add = item_index(cx, state, "add");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("item", add), cx);
            window.render_frame(cx);
            // Before the first call the form has the whole pane to itself.
            assert!(window.try_find("detail-input").is_some());
            assert!(
                window.try_find("response").is_none(),
                "nothing to split yet"
            );
            window.click("$.a", cx);
            window.input("2", cx);
            window.click("$.b", cx);
            window.input("3.5", cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        // Answered, the pane splits: the form and the response each scroll
        // on their own under the pinned header and toolbar.
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("detail-input").is_some());
            assert!(
                window.try_find("response-body").is_some(),
                "its own scroll view"
            );
            assert!(window.try_find("call").is_some(), "the toolbar stays put");
            // The separator settles where the short form ends, once the
            // form has been measured, and the response has the rest.
            for _ in 0..3 {
                window.render_frame(cx);
            }
            let input = window.find("detail-input").bounds().size.height;
            let response = window.find("response").bounds().size.height;
            assert!(
                input < response,
                "a short form leaves the response the rest: {input:?} vs {response:?}"
            );
        })
        .unwrap();
        let structured = cx.update(|cx| {
            state.read(cx).response().unwrap().raw["structuredContent"]["value"].clone()
        });
        assert_eq!(structured, json!(5.5));
        // The response header copies the whole result, and the toolbar copies
        // the request that produced it.
        let response_copy = cx.update(|cx| {
            let s = state.read(cx);
            let (server, mode, name) = s.response_key().unwrap();
            format!("resp:{server}:{mode:?}:{name}-copy")
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(response_copy, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let copied: serde_json::Value =
            serde_json::from_str(&clipboard(cx).unwrap_or_default()).unwrap();
        assert_eq!(copied["structuredContent"]["value"], json!(5.5));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("copy-request", cx);
            window.render_frame(cx);
        })
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(&clipboard(cx).unwrap_or_default()).unwrap();
        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["arguments"]["a"], json!(2.0));
        // The confirmation gives the status bar back after a few seconds.
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(5));
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        assert!(
            cx.update(|cx| coco_mcp::clip::note(cx)).is_none(),
            "the copy confirmation expires"
        );
        snap(cx, handle, "07-tool-call");
        // The separator between the input and the response follows a drag,
        // and the fit that placed it does not snap it back.
        let (x, boundary, before) = cx
            .update_window(handle.into(), |_, window, _| {
                let bounds = window.find("detail-input").bounds();
                (bounds.center().x, bounds.bottom(), bounds.size.height)
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.drag(
                point(x, boundary - px(2.)),
                point(x, boundary + px(120.)),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        let after = cx
            .update_window(handle.into(), |_, window, _| {
                window.find("detail-input").bounds().size.height
            })
            .unwrap();
        assert!(
            (after - before - px(120.)).abs() <= px(2.),
            "dragging the separator moves it and it stays: {before:?} -> {after:?}"
        );
        // The raw editor takes the input panel above the response, as the
        // form does; it is not sized by its content.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("tab-raw", cx);
        })
        .unwrap();
        snap(cx, handle, "07-raw-above-response");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("tab-form", cx);
        })
        .unwrap();

        // Nested form for `complex`, then the Raw tab.
        let complex = item_index(cx, state, "complex");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", complex), cx);
        })
        .unwrap();
        snap(cx, handle, "08-complex-form");
        // A nested object in the generated form folds and unfolds.
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window.try_find("fold$.user").is_some(),
                "nested object has a fold"
            );
            window.click("fold$.user", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("$.user.name").is_none(),
                "folding hides the nested fields"
            );
            window.click("fold$.user", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("$.user.name").is_some(),
                "unfolding restores them"
            );
        })
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("tab-raw", cx);
        })
        .unwrap();
        snap(cx, handle, "09-raw-json");
        // The editor takes its colours from the theme, in light as well.
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("cmd-t", cx);
            window.render_frame(cx);
        })
        .unwrap();
        snap(cx, handle, "09-raw-json-light");
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("cmd-t", cx);
            window.render_frame(cx);
        })
        .unwrap();
        // Every type the form draws, with the mixed cases, on `kinds`.
        let kinds = item_index(cx, state, "kinds");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", kinds), cx);
        })
        .unwrap();
        snap(cx, handle, "08-kinds-form");
        cx.update_window(handle.into(), |_, window, cx| {
            // A required nullable field starts as an input with a `null`
            // link; the link makes it null, and the null offers the value.
            assert!(window.try_find("$.score").is_some(), "score is an input");
            window.click("null$.score", cx);
            window.render_frame(cx);
            assert!(window.try_find("$.score").is_none(), "score is null");
            window.click("unnull$.score", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("$.score").is_some(),
                "score is an input again"
            );
            // An optional field whose default is null is set to a value,
            // not to its default.
            assert!(
                window.try_find("$.nickname").is_none(),
                "nickname starts unset"
            );
            window.click("set$.nickname", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("$.nickname").is_some(),
                "nickname is an input"
            );
            // Back to `complex`, which the steps below look at.
            window.click(("item", complex), cx);
        })
        .unwrap();

        // The Schema tab: what the server declared about the tool, foldable.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("tab-schema", cx);
            window.render_frame(cx);
            let server_id = state.read(cx).servers[0].record.id.clone();
            let root = format!("schema:{server_id}:complex$");
            let props = format!("{root}.$defs");
            assert!(window.try_find(root.clone()).is_some(), "schema tree shown");
            assert!(
                window.try_find(props.clone()).is_some(),
                "nested schema node"
            );
            window.click(root.clone(), cx);
            window.render_frame(cx);
            assert!(
                window.try_find(props).is_none(),
                "folding the schema root hides its children"
            );
            window.click(root, cx);
            window.render_frame(cx);
        })
        .unwrap();
        snap(cx, handle, "30-tool-schema");

        // `fail` returns isError.
        let fail = item_index(cx, state, "fail");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", fail), cx);
            window.render_frame(cx);
            window.click("$.message", cx);
            window.input("boom", cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(
            wait_for_response(cx, handle, state),
            ResponseStatus::ToolError
        );

        // Pressing Call again while `sleep` is still out sends nothing: one
        // request, one answer, one history row.
        let sleep = item_index(cx, state, "sleep");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", sleep), cx);
            window.render_frame(cx);
            window.click("$.millis", cx);
            window.input("300", cx);
        })
        .unwrap();
        let before = cx.update(|cx| state.read(cx).count(Mode::History).unwrap());
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("call", cx);
            window.render_frame(cx);
            assert!(state.read(cx).response_pending(), "the first press sends");
            window.click("call", cx);
            window.press("cmd-enter", cx);
            window.render_frame(cx);
            assert!(state.read(cx).response_pending());
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        assert_eq!(response_text(cx, state), "slept 300ms");
        // Give any duplicate request time to come back before counting.
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
        }
        let after = cx.update(|cx| state.read(cx).count(Mode::History).unwrap());
        assert_eq!(after, before + 1, "the repeated presses sent nothing");
        assert_eq!(response_text(cx, state), "slept 300ms");

        // Resources: a Markdown document, then a PNG.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("mode", 1usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let readme = item_index(cx, state, "mock://md/readme");
        let pixel = item_index(cx, state, "mock://png/pixel");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", readme), cx);
            window.render_frame(cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        snap(cx, handle, "10-markdown-resource");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", pixel), cx);
            window.render_frame(cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        snap(cx, handle, "11-image-resource");

        // Prompt with an argument, then open the log.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("mode", 2usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let greet = item_index(cx, state, "greet");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", greet), cx);
            window.render_frame(cx);
            window.click("arg-name", cx);
            window.input("Ada", cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        let text = cx.update(|cx| {
            state.read(cx).response().unwrap().raw["messages"][0]["content"]["text"].clone()
        });
        assert!(text.as_str().unwrap().contains("Ada"), "{text}");
        // A prompt's one message fills the response panel, like a tool's
        // one text block.
        let message = cx.update(|cx| {
            let (server, mode, name) = state.read(cx).response_key().unwrap();
            format!("resp:{server}:{mode:?}:{name}m0txt")
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(message).is_some(),
                "the message's text area"
            );
        })
        .unwrap();
        snap(cx, handle, "65-prompt-answer");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("log-header", cx);
            window.render_frame(cx);
            // Filter tabs and Clear sit inside the header: they must not
            // bubble up to its toggle and close the drawer.
            window.click("requests", cx);
            window.render_frame(cx);
            assert!(
                state.read(cx).drawer_open,
                "filter click keeps the drawer open"
            );
            assert_eq!(
                state.read(cx).log_filter,
                coco_mcp::state::LogFilter::Requests
            );
            window.click("all", cx);
            window.render_frame(cx);
            assert!(state.read(cx).drawer_open);
            // A click in the gap between the controls is swallowed too.
            window.click("log-controls", cx);
            window.render_frame(cx);
            assert!(
                state.read(cx).drawer_open,
                "gap click keeps the drawer open"
            );
            window.click("clear-log", cx);
            window.render_frame(cx);
            assert!(state.read(cx).drawer_open, "clear keeps the drawer open");
        })
        .unwrap();
        // Rows keep arriving after a clear; expand one and make sure the rows
        // beneath it are still laid out (variable-height list, not uniform).
        cx.update_window(handle.into(), |_, window, cx| {
            // The gap click above landed on the "errors" label; show everything again.
            window.click("all", cx);
            window.click(("mode", 0usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        // `bump` takes no arguments and has not been called yet in this flow.
        let bump = item_index(cx, state, "bump");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", bump), cx);
            window.render_frame(cx);
            window.click("call", cx);
        })
        .unwrap();
        assert_eq!(wait_for_response(cx, handle, state), ResponseStatus::Ok);
        // The wire events reach the log through their own channel; give it a
        // few turns of the executor to catch up with the response.
        let mut rows = 0;
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
            rows = cx.update(|cx| state.read(cx).visible_log().len());
            if rows >= 2 {
                break;
            }
        }
        let (total, filter) = cx.update(|cx| {
            let s = state.read(cx);
            (s.server().map(|e| e.log().len()).unwrap_or(0), s.log_filter)
        });
        assert!(
            rows >= 2,
            "log has rows after the call: visible {rows}, total {total}, filter {filter:?}"
        );
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("log-row", 0usize), cx);
            window.render_frame(cx);
            // The element is named by the row's index, the model names the
            // row by its id, which the Clear above did not start again.
            let entry = &state.read(cx).servers[0];
            let first = entry.log()[0].row_id;
            assert!(first > 0, "ids carry on after Clear");
            assert_eq!(state.read(cx).expanded_log, [first].into());
            assert!(
                window.try_find(("log-row", 1usize)).is_some(),
                "next row still rendered"
            );
            // The payload's own nodes fold; the row re-measures around them.
            // Tree keys are namespaced by server and name a row by its id.
            let root = format!("{}$", coco_mcp::views::fold_prefix(&entry.record.id, first));
            let meta = format!("{root}._meta");
            assert!(
                window.try_find(meta.clone()).is_some(),
                "nested payload node"
            );
            window.click(root.clone(), cx);
            window.render_frame(cx);
            assert!(
                window.try_find(meta.clone()).is_none(),
                "folding the payload root hides its children"
            );
            assert!(
                window.try_find(("log-row", 1usize)).is_some(),
                "rows still laid out"
            );
            window.click(root, cx);
            window.render_frame(cx);
            assert!(window.try_find(meta).is_some(), "unfolding restores them");
        })
        .unwrap();
        snap(cx, handle, "28-log-expanded");
        // Rows open independently: the second joins the first, and each
        // closes on its own.
        let ids: Vec<u64> = cx.update(|cx| {
            state.read(cx).servers[0].log()[..2]
                .iter()
                .map(|r| r.row_id)
                .collect()
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("log-row", 1usize), cx);
            window.render_frame(cx);
            assert_eq!(
                state.read(cx).expanded_log,
                ids.iter().copied().collect(),
                "both rows open"
            );
        })
        .unwrap();
        snap(cx, handle, "62-log-two-rows");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("log-row", 1usize), cx);
            window.render_frame(cx);
            assert_eq!(state.read(cx).expanded_log, [ids[0]].into());
            window.click(("log-row", 0usize), cx);
            window.render_frame(cx);
            assert!(
                state.read(cx).expanded_log.is_empty(),
                "second click collapses"
            );
        })
        .unwrap();
        snap(cx, handle, "12-prompt-and-log");
    }

    /// Copying and exporting: the two ways data leaves the app. Drives
    /// the clipboard through the real UI and the exports through the payload
    /// builders the menu bar and the palette also call.
    fn copy_and_export_flow() {
        use coco_mcp::views::copy::Export;
        use mcp_core::AuthRef;
        use mcp_store::{CallKind, CallRecord, CallStatus};

        let mut cx = context();
        let mut state = demo_state(true);
        // One wire message and one recorded call, so both streams export.
        let sink = EventSink::new(8);
        let event = sink.emit(EventKind::Request {
            direction: Direction::Outbound,
            id: json!(1),
            method: "tools/call".into(),
            params: Some(json!({"name": "get_weather", "arguments": {"city": "Paris"}})),
        });
        state.servers[0].push_log(log_row(&event).unwrap());
        let server_id = state.servers[0].record.id.clone();
        state.servers[0].push_call(CallRecord {
            id: "call-1".into(),
            server_id,
            kind: CallKind::Tool,
            name: "get_weather".into(),
            args: json!({"city": "Paris"}),
            result: Some(json!({"structuredContent": {"c": 18}})),
            status: CallStatus::Ok,
            error: None,
            elapsed_ms: 7,
            at: time::OffsetDateTime::UNIX_EPOCH,
        });
        // An HTTP server with a bearer token: nothing that leaves the app may
        // carry it unless the action is the one named for it.
        let snapshot = state.servers[0].snapshot().cloned();
        let remote = state.add_demo_server(
            "remote",
            ServerSpec::Http {
                url: "https://api.example.com/mcp".into(),
                headers: Default::default(),
                auth: AuthRef::Bearer {
                    keyring_id: "remote-token".into(),
                },
            },
            Status::Connected,
        );
        state.servers[remote].set_snapshot(snapshot);
        state.secrets.set("remote-token", "s3cret").unwrap();

        let mut workspace = None;
        let state = cx.update(|cx| cx.new(|_| state));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                workspace = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let workspace = workspace.unwrap();
        cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();

        // Exports: the name is derived from the server, the body from the model.
        let exports = cx.update(|cx| {
            workspace.read_with(cx, |ws, cx| {
                [Export::Snapshot, Export::Log, Export::History].map(|kind| {
                    ws.export(kind, cx)
                        .map(|(n, b)| (n, String::from_utf8(b).unwrap()))
                })
            })
        });
        let [snapshot, log, history] = exports;
        let (name, body) = snapshot.expect("a connected server has a snapshot");
        assert_eq!(name, "weather-snapshot.json");
        assert!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["tools"]
                .as_array()
                .is_some_and(|t| !t.is_empty()),
            "the snapshot carries the tools it was diffed on"
        );
        let (name, body) = log.expect("one row was pushed");
        assert_eq!(name, "weather-log.jsonl");
        // One line per message, oldest first, so the row pushed above is last.
        let last = body.lines().next_back().unwrap();
        let line: serde_json::Value = serde_json::from_str(last).unwrap();
        assert_eq!(line["direction"], "out");
        assert_eq!(line["method"], "tools/call");
        assert_eq!(line["payload"]["arguments"]["city"], "Paris");
        // A reconnect's separator is written as a note, so a saved log still
        // shows where each session started.
        let log = cx.update(|cx| {
            state.update(cx, |s, _| {
                let at = time::OffsetDateTime::UNIX_EPOCH;
                s.servers[0].push_log(LogRow::session_break(at));
            });
            workspace.read_with(cx, |ws, cx| ws.export(Export::Log, cx))
        });
        let body = String::from_utf8(log.expect("the log has rows").1).unwrap();
        let last = body.lines().next_back().unwrap();
        let line: serde_json::Value = serde_json::from_str(last).unwrap();
        assert_eq!(line["direction"], "note");
        assert_eq!(line["method"], "session");
        let (name, body) = history.expect("one call was recorded");
        assert_eq!(name, "weather-history.jsonl");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body.trim()).unwrap()["name"],
            "get_weather"
        );

        // Nothing is exported when there is nothing to write.
        cx.update(|cx| {
            state.update(cx, |s, cx| s.select_server(remote, cx));
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            // A request needs a selected tool, and its form is built by the
            // render that follows the selection.
            window.click(("item", 0usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let (config, log, curl) = cx.update(|cx| {
            workspace.update(cx, |ws, cx| {
                (
                    ws.export(Export::Config, cx)
                        .map(|(n, b)| (n, String::from_utf8(b).unwrap())),
                    ws.export(Export::Log, cx),
                    ws.request_curl(cx),
                )
            })
        });
        assert!(log.is_none(), "the remote server has logged nothing");
        let (name, body) = config.expect("every server exports a config");
        assert_eq!(name, "remote-config.json");
        assert!(body.contains("Bearer <token>"), "{body}");
        assert!(!body.contains("s3cret"), "the token stays in the keyring");
        let curl = curl.expect("an HTTP server can be reproduced with curl");
        assert!(curl.contains("$MCP_TOKEN"), "{curl}");
        assert!(!curl.contains("s3cret"), "the token stays in the keyring");
        assert!(curl.contains("text/event-stream"), "{curl}");
        // The one action named for the token does produce it.
        let token = cx.update(|cx| workspace.read_with(cx, |ws, cx| ws.bearer_token(cx)));
        assert_eq!(token.as_deref(), Some("s3cret"));

        // A client configuration read back in: the servers it names are added,
        // its token reaches the keyring, and nothing is connected behind our
        // back.
        let before = cx.update(|cx| state.read(cx).servers.len());
        cx.update(|cx| import_small_config(&workspace, cx));
        let (count, imported) = cx.update(|cx| {
            let s = state.read(cx);
            (
                s.servers.len(),
                s.servers
                    .iter()
                    .map(|e| e.record.name.clone())
                    .collect::<Vec<_>>(),
            )
        });
        // `weather` is already in the sidebar and `broken` describes nothing,
        // so two of the four entries are added.
        assert_eq!(count, before + 2, "two new servers: {imported:?}");
        assert_eq!(
            imported.iter().filter(|n| *n == "weather").count(),
            1,
            "a name already in the sidebar is not added twice"
        );
        assert!(imported.contains(&"notes".to_owned()));
        assert!(imported.contains(&"paid".to_owned()));
        let paid = cx.update(|cx| {
            let s = state.read(cx);
            let entry = s.servers.iter().find(|e| e.record.name == "paid").unwrap();
            let id = coco_mcp::state::keyring_id(&entry.record.spec).unwrap();
            (
                entry.status.clone(),
                s.secrets.get(&id).ok().flatten(),
                entry.record.policy.clone(),
            )
        });
        assert_eq!(paid.0, Status::Off, "an import connects nothing");
        assert_eq!(
            paid.2,
            mcp_core::ServerRequestPolicy::default(),
            "an imported server gets the same request policy as `coco import-config` writes"
        );
        assert_eq!(paid.2.sampling, mcp_core::SamplingPolicy::Prompt);
        assert_eq!(
            paid.1.as_deref(),
            Some("from-the-file"),
            "the token in the file reaches the keyring"
        );

        // The import selected the first server it added; go back to the HTTP
        // one for the rest of the flow.
        cx.update(|cx| {
            state.update(cx, |s, cx| s.select_server(remote, cx));
        });

        // A logged request copies as the whole frame, and as the curl that
        // would send it again.
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.servers[remote].push_log(log_row(&event).unwrap());
                s.drawer_open = true;
                cx.notify();
            })
        });
        // The drawer follows the newest row, so address the one just logged.
        let newest = cx.update(|cx| state.read(cx).servers[remote].log().len() - 1);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.right_click(("log-row", newest), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let entries = cx
            .update(|cx| coco_mcp::clip::menu(cx))
            .expect("right-clicking a row opens the menu")
            .0;
        let labels: Vec<String> = entries.iter().map(|e| e.label.to_string()).collect();
        assert_eq!(
            labels,
            [
                "Copy message",
                "Copy as JSON-RPC",
                "Copy as curl",
                "Copy line"
            ]
        );
        let entry = |label: &str| {
            entries
                .iter()
                .find(|e| e.label == label)
                .map(|e| e.text.clone())
                .unwrap()
        };
        let frame: serde_json::Value = serde_json::from_str(&entry("Copy as JSON-RPC")).unwrap();
        assert_eq!(frame["jsonrpc"], "2.0");
        assert_eq!(frame["id"], json!(1));
        assert_eq!(frame["method"], "tools/call");
        assert_eq!(frame["params"]["arguments"]["city"], "Paris");
        let curl = entry("Copy as curl");
        assert!(curl.contains("https://api.example.com/mcp"), "{curl}");
        assert!(curl.contains(r#""method":"tools/call""#), "{curl}");
        assert!(
            curl.contains("$MCP_TOKEN") && !curl.contains("s3cret"),
            "{curl}"
        );
        cx.update(coco_mcp::clip::close_menu);

        // A payload past the row limit keeps only its head, and every way out
        // of the log says how large the message was.
        let large = sink.emit(EventKind::Request {
            direction: Direction::Outbound,
            id: json!(2),
            method: "tools/call".into(),
            params: Some(json!({"name": "upload", "arguments": {"blob": "x".repeat(200 * 1024)}})),
        });
        let row = log_row(&large).unwrap();
        let original = row.truncated.expect("a 200 KB payload is cut");
        let size = coco_mcp::state::size_label(original);
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.servers[remote].push_log(row);
                cx.notify();
            })
        });
        let newest = cx.update(|cx| state.read(cx).servers[remote].log().len() - 1);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.right_click(("log-row", newest), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let entries = cx
            .update(|cx| coco_mcp::clip::menu(cx))
            .expect("the large row has a menu too")
            .0;
        assert!(
            entries.iter().all(|e| e.label != "Copy as curl"),
            "a cut request does not copy as curl"
        );
        let message = &entries[0];
        assert_eq!(message.label, "Copy message");
        assert!(message.text.contains(&size), "{size}: {}", message.text);
        assert!(message.text.contains("originalBytes"));
        cx.update(coco_mcp::clip::close_menu);
        let log = cx.update(|cx| workspace.read_with(cx, |ws, cx| ws.export(Export::Log, cx)));
        let body = String::from_utf8(log.expect("the remote server has logged").1).unwrap();
        let line: serde_json::Value =
            serde_json::from_str(body.lines().next_back().unwrap()).unwrap();
        assert_eq!(line["payload"]["originalBytes"], original);
        assert!(
            body.len() < 100 * 1024,
            "the export writes the head, not the payload"
        );
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("log-header", cx);
            window.render_frame(cx);
        })
        .unwrap();

        // A node of any tree copies from its own right-click menu.
        let row = cx.update(|cx| {
            let id = state.read(cx).servers[remote].record.id.clone();
            format!("rowschema:{id}:get_weather$.type")
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("item", 0usize), cx);
            window.render_frame(cx);
            window.click("tab-schema", cx);
            window.render_frame(cx);
            window.right_click(row.clone(), cx);
            window.render_frame(cx);
        })
        .unwrap();
        snap(&mut cx, handle, "31-copy-menu");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("copy-menu", 0usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(
            clipboard(&mut cx).as_deref(),
            Some("object"),
            "a string node copies without its quotes"
        );
        assert!(
            cx.update(|cx| coco_mcp::clip::note(cx)).is_some(),
            "the status bar says what was copied"
        );
        assert!(
            cx.update(|cx| coco_mcp::clip::menu(cx).is_none()),
            "choosing an entry closes the menu"
        );
    }

    /// Read a small client configuration, the way the File menu does once
    /// the open panel has handed back a file.
    fn import_small_config(workspace: &gpui_kit::Entity<Workspace>, cx: &mut gpui_kit::App) {
        workspace.update(cx, |ws, cx| {
            ws.import_config(
                r#"{"mcpServers": {
                    "notes": {"command": "srv", "args": ["--verbose", "filesystem-server"]},
                    "paid": {"url": "https://paid.test/mcp",
                             "headers": {"Authorization": "Bearer from-the-file"}},
                    "weather": {"command": "already-there"},
                    "broken": {}
                }}"#,
                cx,
            );
        });
    }

    /// What is on the clipboard right now.
    fn clipboard(cx: &mut HeadlessAppContext) -> Option<String> {
        cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
    }

    /// HTTP transport with a bearer token typed into the form; the token is
    /// kept in the (in-memory) secret store and the mock requires it.
    fn http_bearer_flow() {
        let bridge = coco_mcp::bridge::Bridge::new().unwrap();
        let server = futures::executor::block_on(bridge.run(mcp_mockserver::http::serve_http(
            mcp_mockserver::Schema::V2,
            mcp_mockserver::http::HttpAuth::Bearer("s3cret".into()),
        )))
        .unwrap()
        .unwrap();
        let mut cx = context();
        let state = cx.update(|cx| cx.new(|_| AppState::new(Some(bridge), None)));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        let url = server.url.clone();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("add-server", cx);
            window.render_frame(cx);
            window.click("name", cx);
            window.input("remote", cx);
            window.click("transport-http", cx);
            window.render_frame(cx);
            window.click("url", cx);
            window.input(&url, cx);
            window.click("auth-bearer", cx);
            window.render_frame(cx);
            window.click("token", cx);
            window.input("s3cret", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.capture_screenshot(handle.into())
            .unwrap()
            .save(out_dir().join("13-add-http-bearer.png"))
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connect", cx);
            window.render_frame(cx);
        })
        .unwrap();
        let mut connected = false;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            cx.run_until_parked();
            // No server until its row is saved off the UI thread.
            let status = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    state.read(cx).servers.first().map(|s| s.status.clone())
                })
                .unwrap();
            if status == Some(Status::Connected) {
                connected = true;
                break;
            }
            if let Some(Status::Error(e)) = status {
                panic!("connect failed: {e}");
            }
        }
        assert!(connected, "http bearer server should connect");
        cx.update(|cx| {
            let s = state.read(cx);
            assert!(
                s.secrets
                    .get(&secret_key(&s.servers[0].record.spec))
                    .unwrap()
                    .is_some()
            );
            assert!(s.servers[0].snapshot().unwrap().tool("multiply").is_some());
        });
        cx.capture_screenshot(handle.into())
            .unwrap()
            .save(out_dir().join("14-http-connected.png"))
            .unwrap();
        server.shutdown();
    }

    fn secret_key(spec: &ServerSpec) -> String {
        match spec {
            ServerSpec::Http {
                auth: mcp_core::AuthRef::Bearer { keyring_id },
                ..
            } => keyring_id.clone(),
            other => panic!("expected bearer spec, got {other:?}"),
        }
    }

    /// The About window, opened through the same call the ⌘K entry makes.
    fn about_window(dark: bool, name: &str) {
        let mut cx = context();
        if !dark {
            cx.update(|cx| coco_mcp::theme::set_dark(false, None, cx));
        }
        let handle = cx
            .update(|cx| coco_mcp::views::about::open(None, cx))
            .expect("About window opens");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("about").is_some());
            assert!(window.try_find("source-code").is_some());
            assert!(window.try_find("report-issue").is_some());
        })
        .unwrap();
        let image = cx
            .capture_screenshot(handle.into())
            .expect("Metal rendering");
        let path = out_dir().join(format!("{name}.png"));
        image.save(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }

    /// TEMPORARY: frame timings over large results.
    fn timing_flow() {
        use mcp_store::{CallKind, CallRecord, CallStatus};
        let mut cx = context();
        let mut state = demo_state(true);
        let server_id = state.servers[0].record.id.clone();
        let call = |id: &str, name: &str, result: serde_json::Value| CallRecord {
            id: id.into(),
            server_id: server_id.clone(),
            kind: CallKind::Tool,
            name: name.into(),
            args: json!({}),
            result: Some(result),
            status: CallStatus::Ok,
            error: None,
            elapsed_ms: 40,
            at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let rows = |n: usize| -> serde_json::Value {
            serde_json::Value::Array((0..n).map(|i| json!({"id": i, "ok": true})).collect())
        };
        let text = |n: usize| json!({"content": [{"type": "text", "text": rows(n).to_string()}]});
        let structured = |n: usize| {
            let v = json!({"rows": rows(n)});
            json!({"structuredContent": v, "content": [{"type": "text", "text": v.to_string()}]})
        };
        state.servers[0].push_call(call(
            "call-base",
            "baseline",
            json!({"content": [{"type": "text", "text": "hi"}]}),
        ));
        state.servers[0].push_call(call("call-t1k", "text_1k", text(1_000)));
        state.servers[0].push_call(call("call-t8k", "text_8k", text(8_000)));
        state.servers[0].push_call(call("call-t100k", "text_100k", text(100_000)));
        state.servers[0].push_call(call("call-s8k", "structured_8k", structured(8_000)));
        let prose = |bytes: usize| {
            let mut s = String::new();
            let mut n = 1;
            while s.len() < bytes {
                s.push_str(&format!("Line {n}: the quick brown fox jumps over the lazy dog and reads the log again.\n"));
                n += 1;
            }
            s
        };
        let plain = |bytes: usize| json!({"content": [{"type": "text", "text": prose(bytes)}]});
        let md = |bytes: usize| {
            let mut s = String::from("# Long\n\n");
            let mut n = 1;
            while s.len() < bytes {
                s.push_str(&format!("## Section {n}\n\nParagraph {n} with **bold** and `code`.\n\n- one\n- two\n\n```json\n{{\"n\": {n}}}\n```\n\n"));
                n += 1;
            }
            json!({"content": [{"type": "resource", "resource": {"uri": "mock://md/long", "mimeType": "text/markdown", "text": s}}]})
        };
        state.servers[0].push_call(call("call-p256", "plain_256k", plain(256 * 1024)));
        state.servers[0].push_call(call("call-p2m", "plain_2m", plain(2 * 1024 * 1024)));
        state.servers[0].push_call(call("call-m256", "markdown_256k", md(256 * 1024)));
        let state = cx.update(|cx| cx.new(|_| state));
        let state_for_window = state.clone();
        let handle = cx
            .open_window(size(px(1200.), px(760.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state_for_window, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("mode", 3usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let cases: [(&str, &str, &str); 7] = [
            ("baseline", "call-base", "c0"),
            ("text_1k", "call-t1k", "c0"),
            ("text_8k", "call-t8k", "c0"),
            ("text_100k", "call-t100k", "c0"),
            ("structured_8k", "call-s8k", "s"),
            ("plain_2m", "call-p2m", "c0"),
            ("markdown_256k", "call-m256", "c0"),
        ];
        for (name, id, tree) in cases {
            let row = item_index(&mut cx, &state, name);
            cx.update_window(handle.into(), |_, window, cx| {
                window.click(("item", row), cx);
                window.render_frame(cx);
                let reveal = gpui_kit::SharedString::from(format!("hist:{id}-reveal"));
                if window.try_find(reveal.clone()).is_some() {
                    let t = std::time::Instant::now();
                    window.click(reveal.clone(), cx);
                    let clicked = t.elapsed();
                    window.render_frame(cx);
                    eprintln!(
                        "TIMING {name:<14} first frame {:?} (click {clicked:?})",
                        t.elapsed()
                    );
                }
            })
            .unwrap();
            // Whatever the result parses in the background, let it finish.
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                cx.run_until_parked();
                cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
            cx.update_window(handle.into(), |_, window, cx| {
                let time = |window: &mut gpui_kit::Window, cx: &mut gpui_kit::App, label: &str| {
                    let mut worst = std::time::Duration::ZERO;
                    let frames: u32 = std::env::var("COCO_TIMING_FRAMES")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(10);
                    let start = std::time::Instant::now();
                    for _ in 0..frames {
                        let t = std::time::Instant::now();
                        window.render_frame(cx);
                        worst = worst.max(t.elapsed());
                    }
                    let avg = start.elapsed() / frames;
                    eprintln!("TIMING {name:<14} {label:<10} avg {avg:?} worst {worst:?}");
                };
                time(window, cx, "as drawn");
                if name == "markdown_256k" {
                    assert!(
                        window
                            .try_find(gpui_kit::SharedString::from(format!("hist:{id}c0mdbox")))
                            .is_some(),
                        "the document box is drawn"
                    );
                }
                let root = format!("hist:{id}{tree}$");
                let key = gpui_kit::SharedString::from(if tree == "c0" {
                    root.clone()
                } else {
                    format!("{root}.rows")
                });
                if window.try_find(key.clone()).is_some() {
                    window.click(key.clone(), cx);
                    window.render_frame(cx);
                    time(window, cx, "root folded");
                    let deeper = gpui_kit::SharedString::from(format!("{key}[+2000]"));
                    if false && window.try_find(deeper.clone()).is_some() {
                        window.click(deeper.clone(), cx);
                        window.render_frame(cx);
                        time(window, cx, "4000 rows");
                    }
                }
            })
            .unwrap();
            if name == "markdown_256k" {
                snap(&mut cx, handle, "timing-markdown");
            }
        }
    }

    pub fn run() {
        if std::env::var_os("COCO_TIMING").is_some() {
            timing_flow();
            return;
        }
        copy_and_export_flow();
        http_bearer_flow();
        add_server_flow();
        diff_banner_flow();
        persistence_unavailable_flow();
        failed_write_flow();
        tolerant_connect_flow();
        failed_list_beside_items_flow();
        long_list_flow();
        edit_form_round_trip_flow();
        form_follows_its_target_flow();
        refused_edit_keeps_its_token_flow();
        stored_token_read_later_flow();
        lazy_history_flow();
        failed_connect_keeps_its_log_flow();
        chatty_server_flow();
        large_result_flow();
        live_large_result_flow();
        crate::protocol::cancel_and_progress_flow();
        crate::protocol::output_schema_flow();
        crate::protocol::list_changed_flow();
        crate::protocol::subscription_flow();
        crate::protocol::log_level_flow();
        crate::protocol::completion_flow();
        crate::protocol::keepalive_flow();
        crate::protocol::server_view_flow();
        crate::protocol::withdrawn_request_flow();
        crate::protocol::authorize_flow();
        crate::eras::modern_session_flow();
        crate::eras::auto_fallback_flow();
        crate::eras::protocol_select_flow();
        crate::eras::disabled_control_flow();
        crate::stored::clear_history_flow();
        crate::stored::forget_credentials_flow();
        crate::stored::snapshot_history_flow();
        crate::stored::compare_file_flow();
        crate::interaction::dialog_keyboard_flow();
        crate::interaction::empty_states_flow();
        crate::interaction::titles_and_declaration_flow();
        crate::interaction::replay_needs_session_flow();
        crate::interaction::log_level_filter_flow();
        crate::interaction::double_click_server_flow();
        crate::interaction::log_zoom_flow();
        crate::interaction::plain_text_flow();
        crate::interaction::markdown_result_flow();
        crate::unhappy::failed_call_flow();
        crate::unhappy::crash_mid_session_flow();
        crate::unhappy::request_outcomes_flow();
        crate::unhappy::replay_reads_and_prompts_flow();
        capture("03-empty", demo_state(false), true);
        let mut s = demo_state(true);
        s.screen = Screen::Detail;
        capture("01-connected", s, true);
        let mut s = demo_state(true);
        s.drawer_open = true;
        s.expanded_log.clear();
        capture("02-log-open", s, true);
        let mut s = demo_state(true);
        s.screen = Screen::AddServer;
        capture("04-add-server-placeholder", s, true);
        let s = demo_state(true);
        capture("05-light", s, false);
        about_window(true, "24-about");
        about_window(false, "25-about-light");
        assemble_tour();
    }

    /// The screens the README's animated tour shows, in order, with how
    /// long each stays. Every name is a render a flow above wrote.
    const TOUR: &[(&str, u32)] = &[
        ("04-add-server-placeholder", 1800),
        ("01-connected", 1800),
        ("07-tool-call", 2200),
        ("28-log-expanded", 2200),
        ("15-elicitation-dialog", 1800),
        ("18-history-detail", 2000),
        ("20-diff-details", 2200),
        ("21-command-palette", 2400),
    ];

    /// Width of the tour: half the render, so the file stays small and the
    /// README column is not asked for more than it has.
    const TOUR_WIDTH: u32 = 1200;

    /// Write `tour.gif` from the renders in [`TOUR`], looping. Built from
    /// the same frames the flows just wrote, so it never drifts from the
    /// UI; the file is copied into `docs/` by hand when it changes.
    fn assemble_tour() {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame, imageops};

        let path = out_dir().join("tour.gif");
        let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let mut encoder = GifEncoder::new_with_speed(file, 5);
        encoder.set_repeat(Repeat::Infinite).unwrap();
        for (name, hold_ms) in TOUR {
            let source = out_dir().join(format!("{name}.png"));
            let image = image::open(&source)
                .unwrap_or_else(|e| panic!("tour frame {}: {e}", source.display()));
            let height = image.height() * TOUR_WIDTH / image.width();
            let frame =
                imageops::resize(&image, TOUR_WIDTH, height, imageops::FilterType::Triangle);
            encoder
                .encode_frame(Frame::from_parts(
                    frame,
                    0,
                    0,
                    Delay::from_numer_denom_ms(*hold_ms, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        eprintln!("wrote {}", path.display());
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    macos::run();
    #[cfg(not(target_os = "macos"))]
    eprintln!("screenshots: skipped; no headless renderer on this platform");
}
