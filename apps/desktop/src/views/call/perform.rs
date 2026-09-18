//! Sending the selected call, moving into its detail, and opening a recorded
//! call in its form.

use super::*;

impl Workspace {
    /// Run the request for the current selection. Does nothing while the
    /// previous request for it is still waiting for its answer.
    pub fn perform_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).screen != Screen::Detail {
            return;
        }
        self.sync_selection(window, cx);
        if self.state.read(cx).response_pending() {
            return;
        }
        let (mode, name) = {
            let s = self.state.read(cx);
            (s.mode, s.selected_name())
        };
        let Some(name) = name else {
            return;
        };
        match mode {
            Mode::Tools => {
                let Some(form) = self.selection.form.clone() else {
                    return;
                };
                let args = form.update(cx, |f, cx| f.validated_json(cx));
                if let Some(args) = args {
                    self.state.update(cx, |s, cx| s.call_tool(name, args, cx));
                }
            }
            Mode::Resources => {
                let uri = self.expand_template(&name, cx);
                self.state
                    .update(cx, |s, cx| s.read_resource(name, uri, cx));
            }
            Mode::Prompts => {
                let arg_names: Vec<String> = self
                    .state
                    .read(cx)
                    .selected_prompt()
                    .map(|p| p.arguments.iter().map(|a| a.name.clone()).collect())
                    .unwrap_or_default();
                let mut args = Map::new();
                for arg in arg_names {
                    if let Some(input) = self.selection.args.get(&arg) {
                        let value = input.read(cx).value().to_string();
                        if !value.is_empty() {
                            args.insert(arg, Value::String(value));
                        }
                    }
                }
                self.state.update(cx, |s, cx| s.get_prompt(name, args, cx));
            }
            // The Server view has nothing to send.
            Mode::Server => {}
            Mode::History => {
                let record = self.state.read(cx).selected_call().cloned();
                if let Some(record) = record {
                    self.state.update(cx, |s, cx| s.replay(&record, cx));
                }
            }
        }
    }

    /// Move the keyboard into the selection's first field: the tool form's,
    /// or the first prompt argument or template variable, so typing starts
    /// there and ⌘⏎ sends.
    pub fn focus_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_selection(window, cx);
        if let Some(form) = self.selection.form.clone() {
            form.update(cx, |f, cx| f.focus_first(window, cx));
            return;
        }
        let first = {
            let state = self.state.read(cx);
            match state.mode {
                Mode::Prompts => state
                    .selected_prompt()
                    .and_then(|p| p.arguments.first())
                    .map(|a| a.name.clone()),
                Mode::Resources => state
                    .selected_name()
                    .and_then(|name| Self::template_vars(&name).into_iter().next()),
                _ => None,
            }
        };
        if let Some(name) = first {
            let input = self.arg_input(&name, "", window, cx);
            input.update(cx, |i, cx| i.focus(window, cx));
        }
    }

    /// Load the selected history row into the tool form (or select the
    /// prompt/resource it came from) so it can be edited before resending.
    pub fn open_call_in_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(record) = self.state.read(cx).selected_call().cloned() else {
            return;
        };
        let mode = match record.kind {
            CallKind::Tool => Mode::Tools,
            CallKind::Resource => Mode::Resources,
            CallKind::Prompt => Mode::Prompts,
        };
        let found = self
            .state
            .update(cx, |s, cx| s.select_named(mode, &record.name, cx));
        if !found {
            return;
        }
        item_list::reveal_selection(self, cx);
        self.sync_selection(window, cx);
        match record.kind {
            CallKind::Tool => {
                if let Some(form) = self.selection.form.clone() {
                    form.update(cx, |f, cx| f.load_value(&record.args, window, cx));
                }
            }
            CallKind::Prompt => {
                if let Some(args) = record.args.as_object() {
                    for (name, value) in args {
                        if let Some(text) = value.as_str() {
                            let input = self.arg_input(name, "", window, cx);
                            input.update(cx, |i, cx| i.set_value(text.to_owned(), window, cx));
                        }
                    }
                }
            }
            CallKind::Resource => {}
        }
        cx.notify();
    }
}
