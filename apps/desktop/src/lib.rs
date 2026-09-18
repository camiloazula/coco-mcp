//! GPUI desktop app. Thin views over the `mcp-*` crates.
//!
//! The crate is a library so the headless screenshot tests can drive the
//! same views the binary shows; `main.rs` only calls [`run`].

#![forbid(unsafe_code)]
#![recursion_limit = "256"]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod actions;
pub mod assets;
pub mod bridge;
pub mod calls;
pub mod clip;
pub mod features;
pub mod menus;
pub mod persistence;
pub mod state;
pub mod theme;
pub mod views;

use gpui_kit::component::{Root, TitleBar};
use gpui_kit::{AppContext as _, Bounds, Entity, WindowBounds, WindowOptions, px, size};

use crate::bridge::Bridge;
use crate::state::AppState;
use crate::views::Workspace;

/// The product name, as shown in the menu bar and window titles.
pub const APP_NAME: &str = "Coco MCP";

/// Window size from the design.
pub const WINDOW_SIZE: (f32, f32) = (1200., 760.);

/// Window options: transparent OS title bar so the app draws its own 36px bar.
pub fn window_options(cx: &gpui_kit::App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
            cx,
        ))),
        window_min_size: Some(size(px(800.), px(500.))),
        ..TitleBar::window_options()
    }
}

/// The database at the default path, or why there is none.
fn open_store() -> Result<mcp_store::Store, String> {
    match mcp_store::Store::default_path() {
        Some(path) => mcp_store::Store::open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display())),
        None => Err("no data directory for this user".to_owned()),
    }
}

/// The app's model over `store`. Without a database the app still runs,
/// but says so from the first frame and refuses changes it could not keep;
/// it never falls back to a model that keeps servers in memory.
fn model(bridge: Option<Bridge>, store: Result<mcp_store::Store, String>) -> AppState {
    match store {
        Ok(store) => AppState::new(bridge, Some(store)),
        Err(reason) => {
            tracing::warn!("{reason}; running without persistence");
            AppState::without_database(bridge, reason)
        }
    }
}

/// Start the desktop app.
///
/// The database is opened, and its server rows and theme read, before the
/// window opens: the first frame draws the sidebar in that theme. Nothing
/// else is read up front. A server's recorded calls are read the first time
/// its History is shown, and the keyring only when a token is needed, both
/// off the GPUI thread.
pub fn run() -> anyhow::Result<()> {
    let bridge = Bridge::new()?;
    let store = open_store();
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            if let Err(e) = theme::install(cx) {
                tracing::error!("theme: {e}");
            }
            actions::bind(cx);
            menus::install(cx);
            let state: Entity<AppState> = cx.new(|_| {
                let mut state = model(Some(bridge), store);
                state.secrets = std::sync::Arc::new(mcp_auth::KeyringStore::default());
                state
            });
            let options = window_options(cx);
            let opened = cx.open_window(options, |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            });
            match opened {
                // Closing the window ends the app, so a later launch never
                // finds an earlier session still running behind it.
                Ok(window) => {
                    let main = window.window_id();
                    cx.on_window_closed(move |cx, closed| {
                        if closed == main {
                            cx.quit();
                        }
                    })
                    .detach();
                }
                Err(e) => {
                    tracing::error!("open window: {e}");
                    cx.quit();
                    return;
                }
            }
            cx.activate(true);
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Persistence;

    #[test]
    fn a_database_that_cannot_be_opened_is_not_replaced_by_memory() {
        let reason = "cannot open /nowhere/coco.db: permission denied";
        let state = model(None, Err(reason.to_owned()));
        assert_eq!(state.persistence, Persistence::Unavailable(reason.into()));
        assert!(state.writable_store().is_err(), "changes are refused");

        let state = model(None, Ok(mcp_store::Store::open_in_memory().unwrap()));
        assert_eq!(state.persistence, Persistence::Saved);
    }
}
