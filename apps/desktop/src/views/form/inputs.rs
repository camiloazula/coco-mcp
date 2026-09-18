//! Reading the generated form back into JSON, and the inputs, selects and
//! editors its fields are drawn with.

use super::*;

impl ToolForm {
    /// Read every leaf from its entity into a fresh state and convert it.
    pub(super) fn collect_form(&self, cx: &Context<Self>) -> Result<Value, String> {
        let state = self.harvest(&self.model, &self.state, "$", cx);
        self.model.to_json(&state).map_err(|e| e.to_string())
    }

    pub(super) fn harvest(
        &self,
        model: &FormModel,
        state: &FormState,
        path: &str,
        cx: &Context<Self>,
    ) -> FormState {
        match (model, state) {
            (FormModel::Text { .. }, FormState::Text(s)) => {
                FormState::Text(self.text_at(path, s, cx))
            }
            (FormModel::Number { .. }, FormState::Number(s)) => {
                FormState::Number(self.text_at(path, s, cx))
            }
            (FormModel::Integer { .. }, FormState::Integer(s)) => {
                FormState::Integer(self.text_at(path, s, cx))
            }
            (FormModel::Enum { .. }, FormState::Enum(ix)) => FormState::Enum(
                self.selects
                    .get(path)
                    .and_then(|s| s.read(cx).selected_index(cx).map(|i| i.row))
                    .or(*ix),
            ),
            (FormModel::Object { fields, .. }, FormState::Object(values)) => FormState::Object(
                fields
                    .iter()
                    .zip(values)
                    .map(|(f, v)| {
                        v.as_ref()
                            .map(|v| self.harvest(&f.model, v, &format!("{path}.{}", f.name), cx))
                    })
                    .collect(),
            ),
            (FormModel::Array { items, .. }, FormState::Array(values)) => FormState::Array(
                values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| self.harvest(items, v, &format!("{path}[{i}]"), cx))
                    .collect(),
            ),
            (FormModel::OneOf { variants, .. }, FormState::OneOf { selected, states }) => {
                FormState::OneOf {
                    selected: *selected,
                    states: variants
                        .iter()
                        .zip(states)
                        .enumerate()
                        .map(|(i, (v, s))| self.harvest(&v.model, s, &format!("{path}|{i}"), cx))
                        .collect(),
                }
            }
            (FormModel::Unknown { .. }, FormState::Raw(text)) => FormState::Raw(
                self.editors
                    .get(path)
                    .map(|e| e.read(cx).value().to_string())
                    .unwrap_or_else(|| text.clone()),
            ),
            (_, other) => other.clone(),
        }
    }

    pub(super) fn text_at(&self, path: &str, fallback: &str, cx: &Context<Self>) -> String {
        self.inputs
            .get(path)
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_else(|| fallback.to_owned())
    }

    /// Structural change: capture leaf values first, then drop stale entities.
    pub(super) fn restructure(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut FormState, &FormModel),
    ) {
        self.state = self.harvest(&self.model, &self.state, "$", cx);
        f(&mut self.state, &self.model);
        self.inputs.clear();
        self.selects.clear();
        self.editors.clear();
        cx.notify();
    }

    pub(super) fn input_at(
        &mut self,
        path: &str,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = self.inputs.get(path) {
            return input.clone();
        }
        let initial = initial.to_owned();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(initial, window, cx);
            state
        });
        self.inputs.insert(path.to_owned(), input.clone());
        input
    }

    pub(super) fn select_at(
        &mut self,
        path: &str,
        labels: Vec<SharedString>,
        selected: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EnumSelect> {
        if let Some(select) = self.selects.get(path) {
            return select.clone();
        }
        let select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(labels),
                selected.map(IndexPath::new),
                window,
                cx,
            )
        });
        self.selects.insert(path.to_owned(), select.clone());
        select
    }

    pub(super) fn editor_at(
        &mut self,
        path: &str,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EditorState> {
        if let Some(editor) = self.editors.get(path) {
            return editor.clone();
        }
        let initial = initial.to_owned();
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("json")
                .line_number(false);
            state.set_value(initial, window, cx);
            state
        });
        self.editors.insert(path.to_owned(), editor.clone());
        editor
    }
}
