//! Root view: title bar, three resizable columns, log drawer, status bar.

use std::collections::HashSet;

use gpui_kit::component::IndexPath;
use gpui_kit::component::command::CommandState;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::{Root, TitleBar, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollHandle, ScrollStrategy, SharedString, Styled,
    Subscription, TestSupportExt as _, UniformListScrollHandle, Window, div, px,
};

use crate::actions::{
    AddServer, Call, Cancel, CancelCall, ClearHistory, ClearLog, CommandPalette,
    CompareSnapshotFile, CopyBearerToken, CopyRequest, CopyRequestCurl, CopyResponse,
    CopyServerConfig, DeleteServer, Disconnect, EditServer, ExportConfig, ExportHistory, ExportLog,
    ExportSnapshot, ForgetCredentials, ImportConfig, Quit, Reconnect, SelectServer, ShowHistory,
    ShowPrompts, ShowResources, ShowServer, ShowTools, ToggleLog, ToggleTheme, WORKSPACE, ZoomLog,
};
use crate::state::{AppState, Changed, Gone, Mode, Screen, Status};
use crate::theme::{self, tokens};
use crate::views::copy::Export;
use crate::views::kept::Decoded;
use crate::views::{
    AddServerForm, LogList, RequestUi, banner, confirm_dialog, copy_menu, detail, item_list,
    log_drawer, palette, request_dialog, sidebar, status_bar,
};

mod render;

/// The log drawer's level dropdown.
pub(crate) type LevelSelect = SelectState<SearchableVec<SharedString>>;

/// The dropdown's first entry, which shows every level.
const ALL_LEVELS: &str = "all levels";

/// The window's content.
pub struct Workspace {
    /// The model.
    pub state: Entity<AppState>,
    focus: FocusHandle,
    server_focus: FocusHandle,
    pub(crate) list_focus: FocusHandle,
    filter: Entity<InputState>,
    /// The log drawer's level dropdown.
    pub(crate) log_level: Entity<LevelSelect>,
    columns: Entity<ResizableState>,
    rows: Entity<ResizableState>,
    add_form: Option<Entity<AddServerForm>>,
    /// What `add_form` was built for (see [`Self::wanted_form`]).
    add_form_target: Option<(Option<usize>, bool)>,
    filter_mode: Mode,
    /// Per-selection entities (form, argument inputs, collapse state).
    pub selection: crate::views::Selection,
    /// Entities of the server-request dialog, while one is open.
    pub request_ui: Option<RequestUi>,
    /// Command palette state, created on first ⌘K.
    pub(crate) palette: Option<Entity<CommandState>>,
    /// Whether the palette overlay is shown.
    pub palette_open: bool,
    /// Scroll state of the middle list's rows.
    pub(crate) item_scroll: UniformListScrollHandle,
    /// Scroll state of the sidebar's server rows.
    pub(crate) server_scroll: ScrollHandle,
    /// Focus of the open dialog, which takes the window's keys while open.
    pub(crate) dialog_focus: FocusHandle,
    /// The server and item last scrolled into view, so a selection moved from
    /// anywhere (a key, the palette, a history row) is followed once.
    followed: (Option<usize>, Option<usize>),
    /// Scroll and measurement state of the log drawer's rows.
    pub log_list: LogList,
    /// Folded nodes of every JSON tree in the window, keyed by the tree's
    /// prefix plus the node's path. One set so the log drawer, a tool's
    /// schema and a response can each keep their own folds.
    pub collapsed: HashSet<String>,
    /// Nodes a tree's line budget folded that the user opened, by the same
    /// keys. A key here never folds anything, and `collapsed` still wins.
    pub unfolded: HashSet<String>,
    /// Bumped on every fold so the log drawer's list re-measures the row
    /// whose payload changed height.
    pub collapse_rev: u64,
    /// Response prefixes whose large result the user asked to see. Kept per
    /// selection, like folds, so running the same item again stays shown.
    pub revealed: HashSet<String>,
    /// Decoded blobs and parsed text of the responses on screen, dropped
    /// once not drawn.
    pub decoded: Decoded,
    /// Where the separator between input and response sits, per selection.
    pub(crate) splits: crate::views::split::Splits,
    /// The diff-banner row whose before/after values are open, identified by
    /// the change itself so it cannot follow an index onto another server.
    pub expanded_change: Option<String>,
    /// The Server view's roots editor: the server it edits, whether its
    /// stored roots were read when it was filled, and the field.
    pub roots_editor: Option<(String, bool, Entity<TextareaState>)>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("filter_mode", &self.filter_mode)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// Build the root view over `state`.
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(Mode::Tools.placeholder()));
        let mut subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe(&state, |this, _, _: &Changed, cx| {
                this.sync_after_change(cx)
            }),
            cx.subscribe(&state, |this, _, gone: &Gone, _| this.forget(gone)),
        ];
        subscriptions.push(cx.subscribe_in(
            &filter,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.state.update(cx, |s, cx| s.set_filter(text, cx));
                }
            },
        ));
        let labels: Vec<SharedString> = std::iter::once(ALL_LEVELS)
            .chain(log_drawer::LEVEL_CHOICES)
            .map(|label| SharedString::from(crate::views::capitalized(label)))
            .collect();
        let log_level = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(labels),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        subscriptions.push(cx.subscribe_in(
            &log_level,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SharedString>>, _window, cx| {
                let SelectEvent::Confirm(value) = event;
                let level = value.as_ref().and_then(|v| {
                    log_drawer::LEVEL_CHOICES
                        .iter()
                        .copied()
                        .find(|level| level.eq_ignore_ascii_case(v.as_ref()))
                });
                this.state.update(cx, |s, cx| s.choose_log_level(level, cx));
            },
        ));
        let ws = Self {
            state,
            log_level,
            focus: cx.focus_handle(),
            server_focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            filter,
            columns: cx.new(|_| ResizableState::default()),
            rows: cx.new(|_| ResizableState::default()),
            add_form: None,
            add_form_target: None,
            filter_mode: Mode::Tools,
            selection: crate::views::Selection::default(),
            request_ui: None,
            palette: None,
            palette_open: false,
            item_scroll: UniformListScrollHandle::new(),
            server_scroll: ScrollHandle::new(),
            dialog_focus: cx.focus_handle(),
            followed: (None, None),
            log_list: LogList::default(),
            collapsed: HashSet::new(),
            unfolded: HashSet::new(),
            collapse_rev: 0,
            revealed: HashSet::new(),
            decoded: Decoded::default(),
            splits: Default::default(),
            expanded_change: None,
            roots_editor: None,
            _subscriptions: subscriptions,
        };
        window.focus(&ws.list_focus, cx);
        ws
    }

    /// Keep the theme and the form in step with the model.
    fn sync_after_change(&mut self, cx: &mut Context<Self>) {
        let (dark, wanted, selection) = {
            let s = self.state.read(cx);
            (
                s.dark,
                Self::wanted_form(s),
                (s.selected_server, s.selected_item),
            )
        };
        // A selection that moved is scrolled into view, whatever moved it.
        if let Some(ix) = selection.0
            && selection.0 != self.followed.0
        {
            self.server_scroll.scroll_to_item(ix);
        }
        if let Some(ix) = selection.1
            && selection != self.followed
        {
            self.item_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.followed = selection;
        let available = crate::menus::Available::of(self.state.read(cx));
        crate::menus::refresh(available, cx);
        if dark != tokens(cx).dark {
            theme::set_dark(dark, None, cx);
        }
        // A form built for another target is dropped at the next draw
        // (see `render`); this makes sure that draw comes.
        if wanted != self.add_form_target {
            self.add_form = None;
        }
        cx.notify();
    }

    /// The server form built for this frame, if one is wanted.
    pub(crate) fn server_form(&self) -> Option<Entity<AddServerForm>> {
        self.add_form.clone()
    }

    /// The server form the detail pane shows, if any: the add screen's, for
    /// the server it edits (`None` adds one), or the selected server's own
    /// settings when it is off, ready to connect. The flag says which, so
    /// opening the pane's form with the pencil, and cancelling back to the
    /// pane, each start from the saved settings again. History stays
    /// readable without a session, so it is not replaced.
    fn wanted_form(state: &AppState) -> Option<(Option<usize>, bool)> {
        if state.screen == Screen::AddServer {
            return Some((state.editing, true));
        }
        let ix = state.selected_server?;
        let entry = state.servers.get(ix)?;
        (state.mode != Mode::History && entry.status == Status::Off).then_some((Some(ix), false))
    }

    /// The filter placeholder follows the mode; needs the window, so it runs in render.
    /// Show the model's log level in the drawer's dropdown, whoever set it.
    /// Setting the dropdown's value emits nothing, so this cannot loop.
    fn sync_level_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted =
            crate::views::capitalized(self.state.read(cx).log_min_level.unwrap_or(ALL_LEVELS));
        let shown = self
            .log_level
            .read(cx)
            .selected_value()
            .map(|v| v.to_string());
        if shown.as_deref() != Some(wanted.as_str()) {
            let value = SharedString::from(wanted);
            self.log_level.update(cx, |select, cx| {
                select.set_selected_value(&value, window, cx)
            });
        }
    }

    fn sync_filter_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = self.state.read(cx).mode;
        if mode != self.filter_mode {
            self.filter_mode = mode;
            self.filter.update(cx, |input, cx| {
                input.set_placeholder(mode.placeholder(), window, cx);
            });
        }
    }

    /// Show the add-server form; render creates and focuses it.
    pub fn open_add_server(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.show_add_server(cx));
        cx.notify();
    }

    fn on_call(&mut self, _: &Call, window: &mut Window, cx: &mut Context<Self>) {
        // A form on screen, opened or shown for an off server, is what ⌘⏎
        // submits.
        if let Some(form) = &self.add_form {
            form.update(cx, |f, cx| {
                f.submit(cx);
            });
            return;
        }
        self.perform_call(window, cx);
    }

    /// Switch the tool form between `Form` and `Raw`.
    pub fn set_raw(&mut self, raw: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.selection.schema_tab = false;
        if let Some(form) = &self.selection.form {
            form.update(cx, |f, cx| f.set_raw_mode(raw, window, cx));
        }
        cx.notify();
    }

    /// Show a resource's, template's or prompt's detail again instead of its
    /// declaration.
    pub fn set_detail_tab(&mut self, cx: &mut Context<Self>) {
        self.selection.schema_tab = false;
        cx.notify();
    }

    /// Show the tool's declared schemas instead of the argument editor.
    pub fn set_schema_tab(&mut self, cx: &mut Context<Self>) {
        self.selection.schema_tab = true;
        cx.notify();
    }

    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if crate::clip::menu(cx).is_some() {
            crate::clip::close_menu(cx);
            return;
        }
        if self.palette_open {
            self.close_palette(window, cx);
            return;
        }
        if self.state.read(cx).confirm.is_some() {
            self.state.update(cx, |s, cx| s.cancel_confirm(cx));
            return;
        }
        if self.state.read(cx).current_request().is_some() {
            self.cancel_request(cx);
            return;
        }
        // The drawer is not on this list: it opens and closes only by its
        // own buttons, ⌘J and ⌘⇧J, so reading a log never ends by accident.
        if self.state.read(cx).screen == Screen::AddServer {
            self.state.update(cx, |s, cx| s.cancel_add_server(cx));
            window.focus(&self.list_focus, cx);
        }
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}
