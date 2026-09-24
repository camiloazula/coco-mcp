//! `Call` (⌘⏎ / button) dispatch and the per-selection view entities the
//! detail pane needs: the tool form, argument inputs for prompts and
//! resource templates, and the JSON collapse state.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{AppContext as _, Context, Entity, Subscription, Window};
use mcp_core::CompletionTarget;
use serde_json::{Map, Value};

use crate::calls::ResponseKey;
use crate::state::{Mode, Screen};
use crate::views::Workspace;
use crate::views::form::ToolForm;
use crate::views::item_list;
use crate::views::json::{Folds, Toggle};
use mcp_store::CallKind;

mod perform;

/// Per-selection UI state owned by the workspace.
#[derive(Default)]
pub struct Selection {
    /// Key the entities below belong to.
    pub key: Option<ResponseKey>,
    /// Tool argument form.
    pub form: Option<Entity<ToolForm>>,
    /// String argument inputs (prompt arguments, template variables) by name.
    pub args: HashMap<String, Entity<InputState>>,
    /// `Schema` tab active instead of `Form` / `Raw`.
    pub schema_tab: bool,
    /// Values the server suggested for each argument input, by name.
    pub suggestions: HashMap<String, Vec<String>>,
    /// Bumped per completion request, so an answer to an older keystroke is
    /// dropped.
    completion_seq: u64,
    /// A suggestion just put into an input, whose own change event asks for
    /// nothing.
    chosen: Option<(String, String)>,
    /// The inputs' change subscriptions, dropped with the selection.
    completion_subs: Vec<Subscription>,
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field("key", &self.key)
            .field("schema_tab", &self.schema_tab)
            .field("suggestions", &self.suggestions)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// Ensure the per-selection entities match the current selection.
    pub fn sync_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.state.read(cx).response_key();
        if self.selection.key == key {
            return;
        }
        self.selection = Selection {
            key: key.clone(),
            ..Selection::default()
        };
        let Some((_, mode, _)) = key else {
            return;
        };
        if mode == Mode::Tools
            && let Some(schema) = self
                .state
                .read(cx)
                .selected_tool()
                .map(|t| t.input_schema.clone())
        {
            self.selection.form = Some(cx.new(|cx| ToolForm::new(&schema, window, cx)));
        }
    }

    /// Get or create the input for a named string argument.
    pub fn arg_input(
        &mut self,
        name: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = self.selection.args.get(name) {
            return input.clone();
        }
        let placeholder = placeholder.to_owned();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        self.selection.args.insert(name.to_owned(), input.clone());
        // Each keystroke asks the server what could go here.
        let arg = name.to_owned();
        let changes = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let typed = input.read(cx).value().to_string();
                    this.request_completion(&arg, typed, cx);
                }
            },
        );
        self.selection.completion_subs.push(changes);
        input
    }

    /// Ask the server for values of argument `arg` of the selected prompt or
    /// resource template (`completion/complete`), given what is typed and
    /// the other arguments filled in. The answer replaces the suggestions
    /// under that input, unless a later keystroke asked again first.
    fn request_completion(&mut self, arg: &str, typed: String, cx: &mut Context<Self>) {
        if self
            .selection
            .chosen
            .take_if(|(a, v)| a == arg && *v == typed)
            .is_some()
        {
            return;
        }
        let asked = {
            let state = self.state.read(cx);
            let offered = state
                .features()
                .get(crate::features::Feature::Completions)
                .is_available();
            let target = match (state.mode, state.selected_name()) {
                (Mode::Prompts, Some(name)) => Some(CompletionTarget::Prompt(name)),
                (Mode::Resources, Some(name)) if state.selected_template().is_some() => {
                    Some(CompletionTarget::ResourceTemplate(name))
                }
                _ => None,
            };
            let session = state.server().and_then(|s| s.session.clone());
            match (offered, target, session, state.bridge.clone()) {
                (true, Some(target), Some(session), Some(bridge)) => {
                    Some((target, session, bridge))
                }
                _ => None,
            }
        };
        let Some((target, session, bridge)) = asked else {
            return;
        };
        let filled: BTreeMap<String, String> = self
            .selection
            .args
            .iter()
            .filter(|(name, _)| name.as_str() != arg)
            .map(|(name, input)| (name.clone(), input.read(cx).value().to_string()))
            .filter(|(_, value)| !value.is_empty())
            .collect();
        self.selection.completion_seq += 1;
        let seq = self.selection.completion_seq;
        let key = self.selection.key.clone();
        let arg = arg.to_owned();
        let name = arg.clone();
        let run =
            bridge.run(async move { session.complete(&target, &name, &typed, &filled).await });
        cx.spawn(async move |this, cx| {
            let answer = run.await;
            let _ = this.update(cx, |ws, cx| {
                if ws.selection.key != key || ws.selection.completion_seq != seq {
                    return;
                }
                match answer {
                    Some(Ok(completion)) if !completion.values.is_empty() => {
                        ws.selection.suggestions.insert(arg, completion.values);
                    }
                    _ => {
                        ws.selection.suggestions.remove(&arg);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Put suggestion `value` into argument `arg`'s input.
    pub fn choose_suggestion(
        &mut self,
        arg: &str,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.selection.args.get(arg).cloned() else {
            return;
        };
        self.selection.suggestions.remove(arg);
        self.selection.chosen = Some((arg.to_owned(), value.clone()));
        input.update(cx, |i, cx| i.set_value(value, window, cx));
        cx.notify();
    }

    /// Toggle callback shared by every JSON tree in the window.
    pub fn collapse_toggle(&self, cx: &Context<Self>) -> Toggle {
        let this = cx.entity();
        Rc::new(move |key: String, open: bool, _window, cx| {
            this.update(cx, |ws, cx| {
                if open {
                    ws.collapsed.insert(key);
                } else {
                    ws.collapsed.remove(&key);
                }
                ws.collapse_rev = ws.collapse_rev.wrapping_add(1);
                cx.notify();
            });
        })
    }

    /// The window's fold sets with `toggle`, for a tree drawn while nothing
    /// else of the workspace is borrowed mutably.
    pub fn folds<'a>(&'a self, toggle: &'a Toggle) -> Folds<'a> {
        Folds {
            collapsed: &self.collapsed,
            rev: self.collapse_rev,
            toggle,
            scrolls: &self.tree_scrolls,
        }
    }

    /// Id of the selected server. Tree keys that are only unique within one
    /// server (log row ids, tool names) are namespaced with it, so folds from
    /// one server never apply to another's payloads, and a deleted server's
    /// keys can be found and dropped.
    pub fn server_scope(&self, cx: &Context<Self>) -> String {
        self.state
            .read(cx)
            .server()
            .map(|s| s.record.id.clone())
            .unwrap_or_else(|| "none".to_owned())
    }

    /// Tree prefix of the response under the current selection. Keyed by the
    /// selection rather than by call number, so folds survive a re-run.
    pub fn response_prefix(&self, cx: &Context<Self>) -> String {
        match self.state.read(cx).response_key() {
            Some((server, mode, name)) => format!("resp:{server}:{mode:?}:{name}"),
            None => "resp:none".to_owned(),
        }
    }

    /// Variables of an RFC 6570 level-1 template (`{name}`).
    pub fn template_vars(template: &str) -> Vec<String> {
        let mut vars = Vec::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else {
                break;
            };
            let name = rest[start + 1..start + end]
                .trim_start_matches(['+', '#', '.', '/', ';', '?', '&']);
            if !name.is_empty() && !vars.iter().any(|v| v == name) {
                vars.push(name.to_owned());
            }
            rest = &rest[start + end + 1..];
        }
        vars
    }

    /// Expand a template with the current argument inputs.
    pub fn expand_template(&self, template: &str, cx: &Context<Self>) -> String {
        let mut out = template.to_owned();
        for var in Self::template_vars(template) {
            let value = self
                .selection
                .args
                .get(&var)
                .map(|i| i.read(cx).value().to_string())
                .unwrap_or_default();
            out = out.replace(&format!("{{{var}}}"), &value);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_vars_are_extracted_once() {
        assert_eq!(
            Workspace::template_vars("mock://item/{id}/{id}/{+path}"),
            vec!["id", "path"]
        );
        assert!(Workspace::template_vars("weather://stations").is_empty());
    }
}
