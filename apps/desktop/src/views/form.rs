//! Generated argument form for a tool: one entity per selected tool holding
//! the `FormModel`, its `FormState` (structure), the input entities (leaf
//! values) and the raw JSON editor. `collect` turns all of it into the JSON
//! argument object; `validate` checks it against the schema.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{IndexPath, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, px,
};
use mcp_schema_form::{FormModel, FormState, validate};
use serde_json::{Map, Value};

use crate::theme::tokens;
use crate::views::{mono, muted};

mod inputs;
mod nodes;
mod paths;
mod rows;

use paths::*;
use rows::*;

type EnumSelect = SelectState<SearchableVec<SharedString>>;

/// The form for one tool.
pub struct ToolForm {
    schema: Value,
    model: FormModel,
    state: FormState,
    inputs: HashMap<String, Entity<InputState>>,
    selects: HashMap<String, Entity<EnumSelect>>,
    editors: HashMap<String, Entity<EditorState>>,
    raw: Entity<EditorState>,
    /// `Raw` tab active.
    pub raw_mode: bool,
    /// Validation or parse errors from the last `collect`.
    pub errors: Vec<String>,
    /// Folded nested object and array nodes, by form path.
    collapsed: HashSet<String>,
}

impl std::fmt::Debug for ToolForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolForm")
            .field("raw_mode", &self.raw_mode)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl ToolForm {
    /// Build the form for `schema`.
    pub fn new(schema: &Value, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = FormModel::from_schema(schema);
        let state = model.initial_state();
        let seed = serde_json::to_string_pretty(&model.default_json()).unwrap_or_default();
        let raw = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .language("json")
                .line_number(false);
            editor.set_value(seed, window, cx);
            editor
        });
        Self {
            schema: schema.clone(),
            model,
            state,
            inputs: HashMap::new(),
            selects: HashMap::new(),
            editors: HashMap::new(),
            raw,
            raw_mode: false,
            errors: Vec::new(),
            collapsed: HashSet::new(),
        }
    }

    /// Switch tabs, carrying the current values across.
    pub fn set_raw_mode(&mut self, raw: bool, window: &mut Window, cx: &mut Context<Self>) {
        if raw == self.raw_mode {
            return;
        }
        if raw {
            if let Ok(value) = self.collect_form(cx) {
                let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                self.raw.update(cx, |e, cx| e.set_value(text, window, cx));
            }
        } else {
            let text = self.raw.read(cx).value().to_string();
            if let Ok(value) = serde_json::from_str::<Value>(&text)
                && let Some(state) = self.model.state_from_value(&value)
            {
                self.state = state;
                self.inputs.clear();
                self.selects.clear();
                self.editors.clear();
            }
        }
        self.raw_mode = raw;
        self.errors.clear();
        cx.notify();
    }

    /// Replace the form's values with `value` (history replay). Values that
    /// do not fit the model fall back to the Raw tab so nothing is lost.
    pub fn load_value(&mut self, value: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let text = serde_json::to_string_pretty(value).unwrap_or_default();
        self.raw.update(cx, |e, cx| e.set_value(text, window, cx));
        match self.model.state_from_value(value) {
            Some(state) => {
                self.state = state;
                self.inputs.clear();
                self.selects.clear();
                self.editors.clear();
                self.raw_mode = false;
            }
            None => self.raw_mode = true,
        }
        self.errors.clear();
        cx.notify();
    }

    /// Current JSON from whichever tab is active, without validating and
    /// without touching `errors`: copying a request must not turn the form
    /// red, and an incomplete request is still worth copying.
    pub fn current_json(&self, cx: &Context<Self>) -> Value {
        let value = if self.raw_mode {
            serde_json::from_str(&self.raw.read(cx).value()).map_err(|e| e.to_string())
        } else {
            self.collect_form(cx)
        };
        value.unwrap_or_else(|_| Value::Object(Map::new()))
    }

    /// Current JSON from whichever tab is active, validated against the schema.
    /// Errors are stored in `errors` and returned as `None`.
    pub fn validated_json(&mut self, cx: &mut Context<Self>) -> Option<Value> {
        let value = if self.raw_mode {
            serde_json::from_str::<Value>(&self.raw.read(cx).value()).map_err(|e| format!("$: {e}"))
        } else {
            self.collect_form(cx)
        };
        match value {
            Ok(value) => {
                let issues = validate(&self.schema, &value);
                self.errors = issues.iter().map(ToString::to_string).collect();
                cx.notify();
                self.errors.is_empty().then_some(value)
            }
            Err(e) => {
                self.errors = vec![e];
                cx.notify();
                None
            }
        }
    }

    /// The raw editor (for the `Raw` tab).
    pub fn render_raw(&self, cx: &Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        div()
            .max_w(px(640.))
            .rounded(px(3.))
            .bg(t.field)
            .border_1()
            .border_color(t.hair)
            .px(px(12.))
            .py(px(10.))
            .child(
                Editor::new(&self.raw)
                    .h(px(240.))
                    .appearance(false)
                    .bordered(false),
            )
            .into_any_element()
    }

    /// The generated fields (for the `Form` tab).
    pub fn render_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.clone();
        let state = self.state.clone();
        let rows = match (&model, &state) {
            (FormModel::Object { fields, .. }, FormState::Object(values)) => {
                if fields.is_empty() {
                    vec![muted(cx, 12., "No arguments").into_any_element()]
                } else {
                    // The root folds like every nested object does.
                    let open = !self.collapsed.contains("$");
                    let summary = format!("{{ … {} keys }}", fields.len());
                    let mut rows = vec![self.fold_head("$", open, &summary, cx)];
                    if open {
                        rows.extend(self.object_rows(fields, values, "$", &[], window, cx));
                    }
                    rows
                }
            }
            _ => vec![self.node(&model, &state, "$", "value", window, cx)],
        };
        v_flex()
            .gap(px(10.))
            .max_w(px(640.))
            .children(rows)
            .into_any_element()
    }

    /// Focus the input of the first top-level field that has one.
    pub fn focus_first(&self, window: &mut Window, cx: &mut Context<Self>) {
        let FormModel::Object { fields, .. } = &self.model else {
            return;
        };
        let first = fields
            .iter()
            .find_map(|field| self.inputs.get(&format!("$.{}", field.name)));
        if let Some(input) = first {
            input.update(cx, |i, cx| i.focus(window, cx));
        }
    }

    /// The set of leaf paths that have inputs (tests).
    pub fn input_paths(&self) -> HashSet<String> {
        self.inputs.keys().cloned().collect()
    }
}

// ------------------------------------------------------------ path helpers

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model() -> FormModel {
        FormModel::from_schema(&json!({
            "type": "object",
            "properties": {
                "user": {"type": "object", "properties": {"name": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}}}, "required": ["name"]},
                "flag": {"type": "boolean"}
            },
            "required": ["user"]
        }))
    }

    #[test]
    fn paths_resolve_models_and_states() {
        let m = model();
        let mut s = m.initial_state();
        assert_eq!(model_at(&m, "$.user.name").map(|m| m.kind()), Some("text"));
        assert!(model_at(&m, "$.flag").is_some());
        assert!(model_at(&m, "$.nope").is_none());
        assert_eq!(trail_of("$.user.tags", &m), vec![0, 1]);
        // tags is optional and absent initially.
        assert!(state_at_mut(&mut s, &m, "$.user.tags").is_none());
        set_field(
            &mut s,
            &[0, 1],
            Some(FormState::Array(vec![FormState::Text("a".into())])),
        );
        assert!(
            matches!(state_at_mut(&mut s, &m, "$.user.tags[0]"), Some(FormState::Text(t)) if t == "a")
        );
        set_at(&mut s, &m, "$.user.tags[0]", FormState::Text("b".into()));
        assert_eq!(
            m.to_json(&s).unwrap(),
            json!({"user": {"name": "", "tags": ["b"]}})
        );
    }

    #[test]
    fn segments_parse_mixed_paths() {
        let segs = segments("$.a[2].b|1");
        assert_eq!(segs.len(), 4);
        assert!(matches!(segs[0], Seg::Field("a")));
        assert!(matches!(segs[1], Seg::Index(2)));
        assert!(matches!(segs[2], Seg::Field("b")));
        assert!(matches!(segs[3], Seg::Variant(1)));
    }

    #[test]
    fn a_field_says_what_it_accepts() {
        let model = FormModel::from_schema(&serde_json::json!({
            "type": "integer", "minimum": 1, "exclusiveMaximum": 10,
            "examples": [3], "deprecated": true
        }));
        assert_eq!(hints(&model), "≥ 1 · < 10 · e.g. 3 · deprecated");
        let text = FormModel::from_schema(&serde_json::json!({
            "type": "string", "format": "uri", "maxLength": 80
        }));
        assert_eq!(hints(&text), "uri · at most 80 characters");
    }

    #[test]
    fn a_schema_without_a_form_says_why() {
        let reason = |schema: Value| unknown_reason(&schema);
        assert_eq!(
            reason(serde_json::json!({"allOf": [], "not": {}})),
            "It uses allOf, not"
        );
        assert_eq!(reason(serde_json::json!({})), "It names no type");
        assert_eq!(
            reason(serde_json::json!({"type": ["string", "integer"]})),
            "it allows several types: string, integer"
        );
    }
}
