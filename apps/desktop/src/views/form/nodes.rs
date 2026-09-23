//! Drawing the generated form: a node per schema, the rows of an object and
//! the fold header above them.

use super::*;

impl ToolForm {
    /// The chevron that folds a nested object or array node, with the
    /// one-line summary shown in its place when it is closed.
    pub(super) fn fold_head(
        &self,
        path: &str,
        open: bool,
        summary: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = path.to_owned();
        h_flex()
            .gap(px(4.))
            .child(
                crate::views::fold_icon(open, cx)
                    .id(SharedString::from(format!("fold{path}")))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&p) {
                            this.collapsed.insert(p.clone());
                        }
                        cx.notify();
                    }))
                    .test_support(),
            )
            .child(mono(cx, 12., summary.to_owned()).text_color(tokens(cx).muted))
            .into_any_element()
    }

    pub(super) fn object_rows(
        &mut self,
        fields: &[mcp_schema_form::Field],
        values: &[Option<FormState>],
        path: &str,
        indices: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let mut rows = Vec::new();
        for (i, (field, value)) in fields.iter().zip(values).enumerate() {
            let child_path = format!("{path}.{}", field.name);
            let mut trail: Vec<usize> = indices.to_vec();
            trail.push(i);
            match value {
                Some(v) => {
                    let control = self.node(&field.model, v, &child_path, &field.name, window, cx);
                    let clear = (!field.required).then(|| {
                        let trail = trail.clone();
                        div()
                            .id(SharedString::from(format!("clear{child_path}")))
                            .text_size(px(11.))
                            .text_color(t.muted)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.restructure(cx, |s, _| set_field(s, &trail, None));
                            }))
                            .child("×")
                            .test_support()
                            .into_any_element()
                    });
                    rows.push(row(
                        cx,
                        &field.name,
                        field.required,
                        &field.model,
                        control,
                        clear,
                    ));
                }
                None => {
                    let trail = trail.clone();
                    let add = div()
                        .id(SharedString::from(format!("set{child_path}")))
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.restructure(cx, |s, m| {
                                if let Some(f) = field_model(m, &trail) {
                                    // A default of null is no value to set; the
                                    // blank one is.
                                    let state = match f.initial_state() {
                                        FormState::Null => f.blank_state(),
                                        state => state,
                                    };
                                    set_field(s, &trail, Some(state));
                                }
                            });
                        }))
                        .child(format!("+ Set ({})", kind_label(&field.model)))
                        .test_support();
                    rows.push(row(
                        cx,
                        &field.name,
                        field.required,
                        &field.model,
                        add.into_any_element(),
                        None,
                    ));
                }
            }
        }
        rows
    }

    /// The control of one node. A nullable node carries a `null` link
    /// beside its control, and, while it is null, the word and a link that
    /// puts the type's blank value in its place.
    pub(super) fn node(
        &mut self,
        model: &FormModel,
        state: &FormState,
        path: &str,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !model.meta().nullable {
            return self.typed_node(model, state, path, label, window, cx);
        }
        let t = *tokens(cx);
        let p = path.to_owned();
        if let FormState::Null = state {
            return h_flex()
                .gap(px(8.))
                .items_center()
                .child(mono(cx, 12., "null".to_owned()).text_color(t.muted))
                .child(
                    div()
                        .id(SharedString::from(format!("unnull{path}")))
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let p = p.clone();
                            this.restructure(cx, |s, m| {
                                if let Some(blank) = model_at(m, &p).map(FormModel::blank_state) {
                                    set_at(s, m, &p, blank);
                                }
                            });
                        }))
                        .child(format!("+ Set ({})", kind_label(model)))
                        .test_support(),
                )
                .into_any_element();
        }
        let control = self.typed_node(model, state, path, label, window, cx);
        h_flex()
            .gap(px(8.))
            .items_start()
            .child(div().flex_1().min_w_0().child(control))
            .child(
                div()
                    .id(SharedString::from(format!("null{path}")))
                    .pt(px(6.))
                    .text_size(px(11.))
                    .text_color(t.muted)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let p = p.clone();
                        this.restructure(cx, |s, m| set_at(s, m, &p, FormState::Null));
                    }))
                    .child("null")
                    .test_support(),
            )
            .into_any_element()
    }

    /// The control of a node with a value, by its type.
    fn typed_node(
        &mut self,
        model: &FormModel,
        state: &FormState,
        path: &str,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        match (model, state) {
            (
                FormModel::Text { .. } | FormModel::Number { .. } | FormModel::Integer { .. },
                FormState::Text(s) | FormState::Number(s) | FormState::Integer(s),
            ) => {
                // Numbers without a schema default start empty rather than as a literal 0.
                let numeric = !matches!(model, FormModel::Text { .. });
                let fresh =
                    numeric && model.meta().default.is_none() && !self.inputs.contains_key(path);
                let initial = if fresh { "" } else { s.as_str() };
                let input = self.input_at(path, initial, window, cx);
                Input::new(&input)
                    .id(SharedString::from(path.to_owned()))
                    .h(px(26.))
                    .into_any_element()
            }
            (FormModel::Bool { meta }, FormState::Bool(b)) => {
                let hint = match &meta.default {
                    Some(d) => format!("Boolean · default {d}"),
                    None => "Boolean".to_string(),
                };
                let path_owned = path.to_owned();
                h_flex()
                    .gap(px(8.))
                    .child(
                        Checkbox::new(SharedString::from(path.to_owned()))
                            .checked(*b)
                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                let v = *v;
                                let p = path_owned.clone();
                                this.restructure(cx, |s, m| set_at(s, m, &p, FormState::Bool(v)));
                            })),
                    )
                    .child(muted(cx, 12., hint))
                    .into_any_element()
            }
            (FormModel::Enum { options, .. }, FormState::Enum(ix)) => {
                let labels: Vec<SharedString> = options
                    .iter()
                    .map(|o| SharedString::from(o.label.clone()))
                    .collect();
                let select = self.select_at(path, labels, *ix, window, cx);
                Select::new(&select).into_any_element()
            }
            (FormModel::Object { fields, .. }, FormState::Object(values)) => {
                // Nested objects fold like a JSON tree node. The top-level
                // object never reaches here: `render_fields` draws it.
                if self.collapsed.contains(path) {
                    return self.fold_head(
                        path,
                        false,
                        &format!("{{ … {} keys }}", fields.len()),
                        cx,
                    );
                }
                let head = self.fold_head(path, true, "{", cx);
                let trail = trail_of(path, &self.model);
                let rows = self.object_rows(fields, values, path, &trail, window, cx);
                v_flex()
                    .gap(px(10.))
                    .child(head)
                    .child(
                        v_flex()
                            .gap(px(10.))
                            .pl(px(16.))
                            .border_l_1()
                            .border_color(t.hair)
                            .when(rows.is_empty(), |el| {
                                el.child(muted(cx, 12., "Empty object"))
                            })
                            .children(rows),
                    )
                    .into_any_element()
            }
            (FormModel::Array { items, .. }, FormState::Array(values)) => {
                if self.collapsed.contains(path) {
                    return self.fold_head(
                        path,
                        false,
                        &format!("[ … {} items ]", values.len()),
                        cx,
                    );
                }
                let mut rows = vec![self.fold_head(path, true, "[", cx)];
                for (i, v) in values.iter().enumerate() {
                    let item_path = format!("{path}[{i}]");
                    let control =
                        self.node(items, v, &item_path, &format!("{label}[{i}]"), window, cx);
                    let p = path.to_owned();
                    rows.push(
                        h_flex()
                            .gap(px(8.))
                            .items_start()
                            .child(div().flex_1().min_w_0().child(control))
                            .child(
                                div()
                                    .id(SharedString::from(format!("rm{item_path}")))
                                    .pt(px(6.))
                                    .text_size(px(11.))
                                    .text_color(t.muted)
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let p = p.clone();
                                        this.restructure(cx, |s, m| {
                                            if let Some(FormState::Array(items)) =
                                                state_at_mut(s, m, &p)
                                                && i < items.len()
                                            {
                                                items.remove(i);
                                            }
                                        });
                                    }))
                                    .child("×"),
                            )
                            .into_any_element(),
                    );
                }
                let p = path.to_owned();
                rows.push(
                    div()
                        .id(SharedString::from(format!("add{path}")))
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let p = p.clone();
                            this.restructure(cx, |s, m| {
                                if let Some(FormModel::Array { items, .. }) = model_at(m, &p)
                                    && let Some(FormState::Array(values)) = state_at_mut(s, m, &p)
                                {
                                    values.push(items.initial_state());
                                }
                            });
                        }))
                        .child("+ add item")
                        .into_any_element(),
                );
                v_flex().gap(px(8.)).children(rows).into_any_element()
            }
            (FormModel::OneOf { variants, .. }, FormState::OneOf { selected, states }) => {
                let labels: Vec<SharedString> = variants
                    .iter()
                    .map(|v| SharedString::from(v.label.clone()))
                    .collect();
                let select =
                    self.select_at(&format!("{path}|"), labels, Some(*selected), window, cx);
                let chosen = select
                    .read(cx)
                    .selected_index(cx)
                    .map(|i| i.row)
                    .unwrap_or(*selected);
                let inner = variants.get(chosen).zip(states.get(chosen)).map(|(v, s)| {
                    self.node(&v.model, s, &format!("{path}|{chosen}"), label, window, cx)
                });
                v_flex()
                    .gap(px(8.))
                    .child(Select::new(&select))
                    .children(inner)
                    .into_any_element()
            }
            (FormModel::Unknown { raw, .. }, FormState::Raw(text)) => {
                let editor = self.editor_at(path, text, window, cx);
                let why = format!("No form for this schema: {}", unknown_reason(raw));
                let field = div()
                    .rounded(px(3.))
                    .bg(t.field)
                    .border_1()
                    .border_color(t.hair)
                    .px(px(8.))
                    .py(px(6.))
                    .child(
                        Editor::new(&editor)
                            .h(px(96.))
                            .appearance(false)
                            .bordered(false),
                    );
                v_flex()
                    .gap(px(4.))
                    .child(muted(cx, 11., why))
                    .child(field)
                    .into_any_element()
            }
            (_, FormState::Null) => muted(cx, 12., "Null").into_any_element(),
            // Every state is built from its own model, so no other pairing
            // arises; the arm exists because a match over both must be total.
            _ => muted(cx, 12., "Unsupported").into_any_element(),
        }
    }
}
