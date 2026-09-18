//! JSON Schema -> [`FormModel`] -> JSON value. No UI.
//!
//! The UI renders a [`FormModel`] tree and keeps a matching [`FormState`];
//! [`FormModel::to_json`] turns the state into the argument object sent to
//! the server, and [`validate`] checks that object against the original
//! schema with human-readable error paths. Anything the builder does not
//! understand becomes [`FormModel::Unknown`], which the UI renders as a raw
//! JSON editor, so a form can always be shown.
//!
//! UI-free: must never depend on `gpui` or `gpui-kit`.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod build;
mod model;
mod state;
mod validate;

pub use model::{EnumOption, Field, FormModel, Meta, NumberBounds, Variant};
pub use state::{FormError, FormState};
pub use validate::{ValidationIssue, validate};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn end_to_end_default_state_produces_valid_json() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "default": "x"},
                "count": {"type": "integer", "minimum": 1},
                "flag": {"type": "boolean"},
                "opt": {"type": "string"}
            },
            "required": ["name", "count", "flag"]
        });
        let model = FormModel::from_schema(&schema);
        let state = model.initial_state();
        let value = model.to_json(&state).unwrap();
        assert_eq!(value, json!({"name": "x", "count": 1, "flag": false}));
        assert!(validate(&schema, &value).is_empty());
        assert_eq!(model.default_json(), value);
    }
}
