//! One labelled field row, and the hints and reasons shown under its control.

use super::*;

pub(super) fn kind_label(model: &FormModel) -> &'static str {
    match model.kind() {
        "text" => "string",
        "bool" => "boolean",
        "one_of" => "one of",
        "unknown" => "json",
        other => other,
    }
}

/// One form row: 140px mono label with required marker, then the control.
pub(super) fn row(
    cx: &Context<ToolForm>,
    name: &str,
    required: bool,
    model: &FormModel,
    control: AnyElement,
    trailing: Option<AnyElement>,
) -> AnyElement {
    let t = *tokens(cx);
    let hint = hints(model);
    let block = matches!(
        model,
        FormModel::Object { .. }
            | FormModel::Array { .. }
            | FormModel::Unknown { .. }
            | FormModel::OneOf { .. }
    );
    h_flex()
        .gap(px(16.))
        .when(block, |el| el.items_start())
        .child(
            h_flex()
                .w(px(140.))
                .flex_none()
                .gap(px(4.))
                .when(block, |el| el.pt(px(6.)))
                .child(mono(cx, 12., name.to_owned()).text_color(t.muted))
                .when(required, |el| {
                    el.child(div().text_size(px(12.)).text_color(t.accent).child("*"))
                })
                .children(trailing),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(control)
                .children(model.meta().description.clone().map(|d| muted(cx, 11., d)))
                .when(!hint.is_empty(), |el| el.child(muted(cx, 11., hint))),
        )
        .into_any_element()
}

/// What a field accepts beyond its type: format, lengths, pattern, bounds,
/// examples, deprecation, as the schema states them.
pub(super) fn hints(model: &FormModel) -> String {
    let mut parts = Vec::new();
    match model {
        FormModel::Text {
            format,
            min_length,
            max_length,
            pattern,
            ..
        } => {
            parts.extend(format.clone());
            match (min_length, max_length) {
                (Some(min), Some(max)) => parts.push(format!("{min}–{max} characters")),
                (Some(min), None) => parts.push(format!("at least {min} characters")),
                (None, Some(max)) => parts.push(format!("at most {max} characters")),
                (None, None) => {}
            }
            parts.extend(pattern.as_ref().map(|p| format!("matches {p}")));
        }
        FormModel::Number { bounds, .. } | FormModel::Integer { bounds, .. } => {
            parts.extend(bounds.minimum.map(|v| format!("≥ {v}")));
            parts.extend(bounds.exclusive_minimum.map(|v| format!("> {v}")));
            parts.extend(bounds.maximum.map(|v| format!("≤ {v}")));
            parts.extend(bounds.exclusive_maximum.map(|v| format!("< {v}")));
            parts.extend(bounds.multiple_of.map(|v| format!("multiple of {v}")));
        }
        _ => {}
    }
    let meta = model.meta();
    if !meta.examples.is_empty() {
        let examples: Vec<String> = meta.examples.iter().map(Value::to_string).collect();
        parts.push(format!("e.g. {}", examples.join(", ")));
    }
    if meta.nullable {
        parts.push("or null".to_owned());
    }
    if meta.deprecated {
        parts.push("deprecated".to_owned());
    }
    parts.join(" · ")
}

/// Why the form drew a schema as raw JSON rather than as fields.
pub(super) fn unknown_reason(schema: &Value) -> String {
    const CONSTRUCTS: [&str; 9] = [
        "allOf",
        "anyOf",
        "not",
        "if",
        "patternProperties",
        "additionalProperties",
        "dependentSchemas",
        "prefixItems",
        "$ref",
    ];
    let used: Vec<&str> = CONSTRUCTS
        .into_iter()
        .filter(|key| schema.get(key).is_some())
        .collect();
    if !used.is_empty() {
        return format!("It uses {}", used.join(", "));
    }
    match schema.get("type") {
        None => "It names no type".to_owned(),
        Some(Value::Array(types)) => format!(
            "it allows several types: {}",
            types
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(other) => format!("Type {other} has no form"),
    }
}
