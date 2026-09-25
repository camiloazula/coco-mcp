//! The "Add server" form (design screen 04), also used to edit an existing
//! server: opened for one, it is prefilled and saves in place instead of
//! adding. Lives in the detail pane, where it is also what a saved server
//! that is not connected shows, ready to connect.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, TestSupportExt as _,
    Window, div, px,
};
use mcp_core::{AuthRef, ProtocolMode, ServerSpec};

use crate::state::{AppState, Screen, Status};
use crate::theme::tokens;
use crate::views::server_fields::{self, Kept, join_pairs, parse_command, parse_cwd, parse_pairs};
use crate::views::{accent_button, kbd, muted, text_tab};

mod heading;
mod submit;
mod view;

/// Why a save waits: the stored bearer token is still being read.
const READING_TOKEN: &str = "Reading the stored token…";

/// A protocol era as a row of the Protocol menu: its name, and beside it
/// what choosing it does. The closed field shows the name alone.
#[derive(Clone)]
struct ProtocolChoice(ProtocolMode);

impl SearchableListItem for ProtocolChoice {
    type Value = ProtocolMode;

    fn title(&self) -> SharedString {
        self.0.label().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .gap(px(12.))
            .items_baseline()
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.0.label()),
            )
            .child(muted(cx, 12., view::protocol_note(self.0)))
    }

    fn value(&self) -> &ProtocolMode {
        &self.0
    }
}

/// The Protocol menu's state: the three eras, one selected.
type ProtocolSelect = SelectState<SearchableVec<ProtocolChoice>>;

/// Form state: one entity so its inputs keep focus across re-renders.
pub struct AddServerForm {
    state: Entity<AppState>,
    name: Entity<InputState>,
    command: Entity<InputState>,
    cwd: Entity<InputState>,
    env: Entity<TextareaState>,
    url: Entity<InputState>,
    headers: Entity<TextareaState>,
    token: Entity<InputState>,
    stdio: bool,
    auth: AuthKind,
    /// The protocol era the server is connected in.
    protocol: ProtocolMode,
    /// The menu that picks it; `protocol` follows its confirmations.
    protocol_select: Entity<ProtocolSelect>,
    error: Option<String>,
    /// A save is on its way to the database.
    saving: bool,
    /// The edited server's stored bearer token is still being read from the
    /// keyring.
    token_pending: bool,
    /// The id of the server being edited; `None` adds one. Looked up when
    /// the form acts ([`Self::edited`]), so a server deleted, or moved in
    /// the list, is never mistaken for the one that took its place.
    editing: Option<String>,
    /// The edited server's bearer token as stored, once read: the form
    /// holds the saved settings while its field still says this.
    stored_token: Option<String>,
    /// An edited server's values as stored: saved again unchanged while
    /// their field's text is. The program and arguments, working directory
    /// and environment of a stdio server; the headers of an HTTP one.
    command_kept: Option<Kept<(String, Vec<String>)>>,
    cwd_kept: Option<Kept<Option<PathBuf>>>,
    env_kept: Option<Kept<BTreeMap<String, String>>>,
    headers_kept: Option<Kept<BTreeMap<String, String>>>,
}

/// Auth choice for HTTP servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// No credentials.
    None,
    /// Bearer token kept in the keyring.
    Bearer,
    /// OAuth 2.1 with PKCE; the browser opens on first connect.
    OAuth,
}

impl std::fmt::Debug for AddServerForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddServerForm")
            .field("stdio", &self.stdio)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl AddServerForm {
    /// Build the form with the design's placeholders, prefilled when the
    /// state says a server is being edited.
    /// `editing` is the id of the server whose settings the form starts
    /// from and saves over; `None` adds a new one.
    pub fn new(
        state: Entity<AppState>,
        editing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let existing = {
            let s = state.read(cx);
            editing
                .as_ref()
                .and_then(|id| s.servers.iter().find(|e| &e.record.id == id))
                .map(|e| e.record.clone())
        };
        let (secrets, bridge) = {
            let s = state.read(cx);
            (s.secrets.clone(), s.bridge.clone())
        };
        let mut token_key = None;
        let mut name_v = String::new();
        let mut command_v = String::new();
        let mut cwd_v = String::new();
        let mut stored_stdio = None;
        let mut env_v = String::new();
        let mut url_v = String::new();
        let mut headers_v = String::new();
        let mut stored_headers = None;
        let mut token_v = String::new();
        let mut stdio = true;
        let mut auth = AuthKind::None;
        let mut protocol = ProtocolMode::default();
        let mut editing = None;
        if let Some(record) = existing {
            name_v = record.name.clone();
            protocol = record.protocol;
            match &record.spec {
                ServerSpec::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                } => {
                    command_v = mcp_exchange::command_line(
                        std::iter::once(command.as_str()).chain(args.iter().map(String::as_str)),
                    );
                    cwd_v = cwd
                        .as_ref()
                        .map(|dir| dir.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    env_v = join_pairs(env, '=');
                    stored_stdio =
                        Some(((command.clone(), args.clone()), cwd.clone(), env.clone()));
                }
                ServerSpec::Http {
                    url,
                    headers,
                    auth: auth_ref,
                } => {
                    stdio = false;
                    url_v = url.clone();
                    headers_v = join_pairs(headers, ':');
                    stored_headers = Some(headers.clone());
                    auth = match auth_ref {
                        AuthRef::None => AuthKind::None,
                        AuthRef::Bearer { keyring_id } => {
                            token_key = Some(keyring_id.clone());
                            AuthKind::Bearer
                        }
                        AuthRef::OAuth { .. } => AuthKind::OAuth,
                    };
                }
            }
            editing = Some(record.id.clone());
        }
        let command = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("weather-server --units metric")
                .default_value(command_v)
        });
        let cwd = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/path/to/project (empty: the app's own directory)")
                .default_value(cwd_v)
        });
        let env = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("KEY=value, one per line")
                .rows(3)
                .default_value(env_v)
        });
        let headers = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("X-Api-Version: 1")
                .rows(3)
                .default_value(headers_v)
        });
        // Kept against the text the inputs hold rather than the text given
        // to them: a single-line input drops line breaks.
        let (command_kept, cwd_kept, env_kept) = match stored_stdio {
            Some((words, dir, vars)) => (
                Some(Kept::new(command.read(cx).value().to_string(), words)),
                Some(Kept::new(cwd.read(cx).value().to_string(), dir)),
                Some(Kept::new(env.read(cx).value().to_string(), vars)),
            ),
            None => (None, None, None),
        };
        let headers_kept =
            stored_headers.map(|map| Kept::new(headers.read(cx).value().to_string(), map));
        // A keyring can block (a locked keychain asks the user), so the
        // stored token is read on the blocking pool and filled in when it
        // arrives; a model without a bridge reads it in place.
        let token_read = match (token_key, bridge) {
            (Some(key), Some(bridge)) => Some(bridge.run_blocking(move || secrets.get(&key))),
            (Some(key), None) => {
                token_v = secrets.get(&key).ok().flatten().unwrap_or_default();
                None
            }
            (None, _) => None,
        };
        // The header says whether the form was opened or is the off
        // server's pane, which the model decides.
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let protocol_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(ProtocolMode::ALL.map(ProtocolChoice).to_vec()),
                ProtocolMode::ALL
                    .iter()
                    .position(|mode| *mode == protocol)
                    .map(IndexPath::new),
                window,
                cx,
            )
            .searchable(false)
        });
        cx.subscribe_in(
            &protocol_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<ProtocolChoice>>, _window, cx| {
                let SelectEvent::Confirm(value) = event;
                if let Some(mode) = value {
                    this.protocol = *mode;
                    cx.notify();
                }
            },
        )
        .detach();
        // Read in place without a bridge; otherwise when the read lands.
        let stored_token = token_read
            .is_none()
            .then(|| token_v.clone())
            .filter(|t| !t.is_empty());
        let form = Self {
            state,
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("weather")
                    .default_value(name_v)
            }),
            command,
            cwd,
            env,
            url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("http://localhost:3000/mcp")
                    .default_value(url_v)
            }),
            headers,
            token: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Token (stored in the keyring)")
                    .default_value(token_v)
            }),
            stdio,
            auth,
            protocol,
            protocol_select,
            error: None,
            saving: false,
            token_pending: token_read.is_some(),
            stored_token,
            editing,
            command_kept,
            cwd_kept,
            env_kept,
            headers_kept,
        };
        if let Some(read) = token_read {
            cx.spawn_in(window, async move |this, cx| {
                let token = read.await.and_then(Result::ok).flatten();
                let _ = this.update_in(cx, |form, window, cx| {
                    form.fill_token(token.unwrap_or_default(), window, cx);
                });
            })
            .detach();
        }
        form
    }

    /// Put the stored token in its field once it is read, unless one was
    /// typed in the meantime.
    fn fill_token(&mut self, token: String, window: &mut Window, cx: &mut Context<Self>) {
        // A save refused while the read was out is not refused any more.
        if self.error.as_deref() == Some(READING_TOKEN) {
            self.error = None;
        }
        self.token_pending = false;
        self.stored_token = Some(token.clone()).filter(|t| !t.is_empty());
        if self.token.read(cx).value().is_empty() {
            self.token
                .update(cx, |input, cx| input.set_value(token, window, cx));
        }
        cx.notify();
    }

    /// Whether the form edits an existing server.
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Focus the first field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Move the keyboard to the field most likely to need a change: the
    /// token of a server that refused it, otherwise the name.
    pub fn focus_fix(&self, window: &mut Window, cx: &mut Context<Self>) {
        let refused = self
            .edited(cx)
            .is_some_and(|ix| self.state.read(cx).servers[ix].unauthorized);
        if refused && !self.stdio && self.auth == AuthKind::Bearer {
            self.token.update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.focus(window, cx);
        }
    }

    fn field(
        &self,
        cx: &Context<Self>,
        label: &'static str,
        top: bool,
        control: impl IntoElement,
    ) -> Div {
        let t = *tokens(cx);
        h_flex()
            .gap(px(16.))
            .when(top, |el| el.items_start())
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .when(top, |el| el.pt(px(6.)))
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(control))
    }
}
