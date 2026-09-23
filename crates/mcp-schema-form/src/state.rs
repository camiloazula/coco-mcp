//! Form state and its conversion to JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::FormModel;

/// Error produced by [`FormModel::to_json`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct FormError {
    /// JSON path of the offending node (`$.user.tags[2]`).
    pub path: String,
    /// What went wrong.
    pub message: String,
}

/// The value a user has entered for one [`FormModel`] node.
///
/// Numbers keep the raw text so partially typed input can be shown; it is
/// parsed by [`FormModel::to_json`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FormState {
    /// A string.
    Text(String),
    /// A number as typed.
    Number(String),
    /// An integer as typed.
    Integer(String),
    /// A boolean.
    Bool(bool),
    /// Index into the enum's options; `None` when nothing is selected.
    Enum(Option<usize>),
    /// Object fields in model order. `None` means the optional field is omitted.
    Object(Vec<Option<FormState>>),
    /// Array elements.
    Array(Vec<FormState>),
    /// Selected variant and the state of every variant.
    OneOf {
        /// Index of the chosen alternative.
        selected: usize,
        /// State kept per alternative so switching back loses nothing.
        states: Vec<FormState>,
    },
    /// Raw JSON text for [`FormModel::Unknown`].
    Raw(String),
    /// Explicit `null` for a nullable node.
    Null,
}

impl FormModel {
    /// The state a fresh form starts with: schema defaults where present,
    /// otherwise the type's zero value. Optional object fields start omitted.
    pub fn initial_state(&self) -> FormState {
        if let Some(default) = &self.meta().default
            && let Some(state) = self.state_from_value(default)
        {
            return state;
        }
        self.blank_state()
    }

    /// The type's zero value, whatever the schema default: what a nullable
    /// node holds once the user turns its `null` into a value, and what an
    /// optional field whose default is `null` gets when it is set.
    pub fn blank_state(&self) -> FormState {
        match self {
            Self::Text { .. } => FormState::Text(String::new()),
            Self::Number { bounds, .. } => FormState::Number(zero_within(bounds, false)),
            Self::Integer { bounds, .. } => FormState::Integer(zero_within(bounds, true)),
            Self::Bool { .. } => FormState::Bool(false),
            Self::Enum { options, .. } => FormState::Enum((!options.is_empty()).then_some(0)),
            Self::Object { fields, .. } => FormState::Object(
                fields
                    .iter()
                    .map(|f| f.required.then(|| f.model.initial_state()))
                    .collect(),
            ),
            Self::Array {
                items, min_items, ..
            } => FormState::Array(
                (0..min_items.unwrap_or(0))
                    .map(|_| items.initial_state())
                    .collect(),
            ),
            Self::OneOf { variants, .. } => FormState::OneOf {
                selected: 0,
                states: variants.iter().map(|v| v.model.initial_state()).collect(),
            },
            Self::Unknown { .. } => FormState::Raw("{}".into()),
        }
    }

    /// Build a state that reproduces `value` (used for defaults and replay).
    /// Returns `None` when the value does not fit the model.
    pub fn state_from_value(&self, value: &Value) -> Option<FormState> {
        if value.is_null() {
            return self.meta().nullable.then_some(FormState::Null);
        }
        match self {
            Self::Text { .. } => value.as_str().map(|s| FormState::Text(s.to_owned())),
            Self::Number { .. } => value.as_f64().map(|_| FormState::Number(value.to_string())),
            Self::Integer { .. } => value
                .as_i64()
                .map(|_| FormState::Integer(value.to_string())),
            Self::Bool { .. } => value.as_bool().map(FormState::Bool),
            Self::Enum { options, .. } => options
                .iter()
                .position(|o| &o.value == value)
                .map(|i| FormState::Enum(Some(i))),
            Self::Object { fields, .. } => {
                let obj = value.as_object()?;
                Some(FormState::Object(
                    fields
                        .iter()
                        .map(|f| obj.get(&f.name).and_then(|v| f.model.state_from_value(v)))
                        .collect(),
                ))
            }
            Self::Array { items, .. } => {
                let arr = value.as_array()?;
                arr.iter()
                    .map(|v| items.state_from_value(v))
                    .collect::<Option<Vec<_>>>()
                    .map(FormState::Array)
            }
            Self::OneOf { variants, .. } => {
                let selected = variants
                    .iter()
                    .position(|v| v.model.state_from_value(value).is_some())?;
                Some(FormState::OneOf {
                    selected,
                    states: variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            if i == selected {
                                v.model
                                    .state_from_value(value)
                                    .unwrap_or_else(|| v.model.initial_state())
                            } else {
                                v.model.initial_state()
                            }
                        })
                        .collect(),
                })
            }
            Self::Unknown { .. } => Some(FormState::Raw(serde_json::to_string_pretty(value).ok()?)),
        }
    }

    /// Convert `state` into the JSON value to send.
    pub fn to_json(&self, state: &FormState) -> Result<Value, FormError> {
        self.to_json_at(state, "$")
    }

    /// Convenience: the JSON produced by [`Self::initial_state`].
    pub fn default_json(&self) -> Value {
        self.to_json(&self.initial_state()).unwrap_or(Value::Null)
    }

    fn to_json_at(&self, state: &FormState, path: &str) -> Result<Value, FormError> {
        let err = |message: String| FormError {
            path: path.to_owned(),
            message,
        };
        if let FormState::Null = state {
            return if self.meta().nullable {
                Ok(Value::Null)
            } else {
                Err(err("null is not allowed here".into()))
            };
        }
        match (self, state) {
            (Self::Text { .. }, FormState::Text(s)) => Ok(Value::String(s.clone())),
            (Self::Number { .. }, FormState::Number(s)) => {
                if s.trim().is_empty() {
                    return Err(err("enter a number".into()));
                }
                let n: f64 = s
                    .trim()
                    .parse()
                    .map_err(|_| err(format!("`{s}` is not a number")))?;
                serde_json::Number::from_f64(n)
                    .map(Value::Number)
                    .ok_or_else(|| err(format!("`{s}` is not a finite number")))
            }
            (Self::Integer { .. }, FormState::Integer(s)) => {
                if s.trim().is_empty() {
                    return Err(err("enter an integer".into()));
                }
                s.trim()
                    .parse::<i64>()
                    .map(Value::from)
                    .map_err(|_| err(format!("`{s}` is not an integer")))
            }
            (Self::Bool { .. }, FormState::Bool(b)) => Ok(Value::Bool(*b)),
            (Self::Enum { options, .. }, FormState::Enum(selected)) => selected
                .and_then(|i| options.get(i))
                .map(|o| o.value.clone())
                .ok_or_else(|| err("no option selected".into())),
            (Self::Object { fields, .. }, FormState::Object(values)) => {
                let mut out = Map::new();
                for (field, value) in fields.iter().zip(values) {
                    let child = format!("{path}.{}", field.name);
                    match value {
                        Some(v) => {
                            out.insert(field.name.clone(), field.model.to_json_at(v, &child)?);
                        }
                        None if field.required => {
                            return Err(FormError {
                                path: child,
                                message: "required".into(),
                            });
                        }
                        None => {}
                    }
                }
                Ok(Value::Object(out))
            }
            (Self::Array { items, .. }, FormState::Array(values)) => values
                .iter()
                .enumerate()
                .map(|(i, v)| items.to_json_at(v, &format!("{path}[{i}]")))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            (Self::OneOf { variants, .. }, FormState::OneOf { selected, states }) => {
                let variant = variants
                    .get(*selected)
                    .ok_or_else(|| err("no variant selected".into()))?;
                let state = states
                    .get(*selected)
                    .ok_or_else(|| err("variant state missing".into()))?;
                variant.model.to_json_at(state, path)
            }
            (Self::Unknown { .. }, FormState::Raw(text)) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Ok(Value::Null);
                }
                serde_json::from_str(trimmed).map_err(|e| err(format!("invalid JSON: {e}")))
            }
            (model, state) => Err(err(format!(
                "state {} does not match {} model",
                state_kind(state),
                model.kind()
            ))),
        }
    }
}

fn state_kind(state: &FormState) -> &'static str {
    match state {
        FormState::Text(_) => "text",
        FormState::Number(_) => "number",
        FormState::Integer(_) => "integer",
        FormState::Bool(_) => "bool",
        FormState::Enum(_) => "enum",
        FormState::Object(_) => "object",
        FormState::Array(_) => "array",
        FormState::OneOf { .. } => "one_of",
        FormState::Raw(_) => "raw",
        FormState::Null => "null",
    }
}

/// `0` clamped into the schema's bounds, rendered as text.
fn zero_within(bounds: &crate::model::NumberBounds, integer: bool) -> String {
    let mut n: f64 = 0.0;
    if let Some(min) = bounds.minimum {
        n = n.max(min);
    }
    if let Some(min) = bounds.exclusive_minimum {
        n = n.max(if integer {
            min + 1.0
        } else {
            // The next number up: adding EPSILON leaves any bound past 1 unchanged.
            min.next_up()
        });
    }
    if let Some(max) = bounds.maximum {
        n = n.min(max);
    }
    if let Some(max) = bounds.exclusive_maximum {
        n = n.min(if integer { max - 1.0 } else { max.next_down() });
    }
    if integer {
        format!("{}", n.ceil() as i64)
    } else if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model() -> FormModel {
        FormModel::from_schema(&json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "n": {"type": "number", "minimum": 1.5},
                "i": {"type": "integer", "exclusiveMinimum": 0},
                "b": {"type": "boolean", "default": true},
                "e": {"enum": ["x", "y"]},
                "tags": {"type": "array", "items": {"type": "string"}, "minItems": 2},
                "opt": {"type": "string"},
                "nul": {"type": ["string", "null"]},
                "raw": {"additionalProperties": {"type": "integer"}, "type": "object"},
                "u": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
            },
            "required": ["name", "n", "i", "b", "e", "tags", "nul", "raw", "u"]
        }))
    }

    #[test]
    fn initial_state_respects_defaults_bounds_and_required() {
        let m = model();
        let v = m.default_json();
        assert_eq!(
            v,
            json!({"name": "", "n": 1.5, "i": 1, "b": true, "e": "x", "tags": ["", ""], "nul": "", "raw": {}, "u": ""})
        );
        assert!(crate::validate(&m_schema(), &v).is_empty());
    }

    fn m_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "n": {"type": "number", "minimum": 1.5},
                "i": {"type": "integer", "exclusiveMinimum": 0},
                "b": {"type": "boolean", "default": true},
                "e": {"enum": ["x", "y"]},
                "tags": {"type": "array", "items": {"type": "string"}, "minItems": 2},
                "opt": {"type": "string"},
                "nul": {"type": ["string", "null"]},
                "raw": {"additionalProperties": {"type": "integer"}, "type": "object"},
                "u": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
            },
            "required": ["name", "n", "i", "b", "e", "tags", "nul", "raw", "u"]
        })
    }

    #[test]
    fn errors_carry_paths() {
        let m = model();
        let mut state = m.initial_state();
        if let FormState::Object(values) = &mut state {
            values[1] = Some(FormState::Number("abc".into()));
        }
        let err = m.to_json(&state).unwrap_err();
        assert_eq!(err.path, "$.n");
        assert!(err.message.contains("not a number"));

        let mut state = m.initial_state();
        if let FormState::Object(values) = &mut state {
            values[0] = None;
        }
        assert_eq!(m.to_json(&state).unwrap_err().path, "$.name");

        let mut state = m.initial_state();
        if let FormState::Object(values) = &mut state {
            values[5] = Some(FormState::Array(vec![
                FormState::Text("a".into()),
                FormState::Bool(true),
            ]));
        }
        assert_eq!(m.to_json(&state).unwrap_err().path, "$.tags[1]");

        let mut state = m.initial_state();
        if let FormState::Object(values) = &mut state {
            values[8] = Some(FormState::Raw("{".into()));
        }
        assert_eq!(m.to_json(&state).unwrap_err().path, "$.raw");
    }

    #[test]
    fn a_null_default_starts_null_and_blank_is_the_zero_value() {
        let m = FormModel::from_schema(&json!({"type": ["string", "null"], "default": null}));
        assert!(matches!(m.initial_state(), FormState::Null));
        assert!(matches!(m.blank_state(), FormState::Text(s) if s.is_empty()));
    }

    #[test]
    fn null_only_where_nullable() {
        let m = model();
        let mut state = m.initial_state();
        if let FormState::Object(values) = &mut state {
            values[7] = Some(FormState::Null);
        }
        assert_eq!(m.to_json(&state).unwrap()["nul"], Value::Null);
        if let FormState::Object(values) = &mut state {
            values[0] = Some(FormState::Null);
        }
        assert_eq!(m.to_json(&state).unwrap_err().path, "$.name");
    }

    #[test]
    fn state_from_value_round_trips() {
        let m = model();
        let value = json!({"name": "a", "n": 2.5, "i": 3, "b": false, "e": "y", "tags": ["t"], "opt": "o", "nul": null, "raw": {"k": 1}, "u": 7});
        let state = m.state_from_value(&value).unwrap();
        assert_eq!(m.to_json(&state).unwrap(), value);
        assert!(
            m.state_from_value(&json!({"name": 1})).is_some(),
            "wrong types are dropped, not fatal"
        );
        assert!(
            FormModel::from_schema(&json!({"type": "string"}))
                .state_from_value(&json!(1))
                .is_none()
        );
    }

    #[test]
    fn a_number_default_passes_an_exclusive_bound_of_any_size() {
        for min in [0.0, 1.0, 2.0, 1e6] {
            let bounds = crate::model::NumberBounds {
                exclusive_minimum: Some(min),
                ..Default::default()
            };
            let n: f64 = zero_within(&bounds, false).parse().unwrap();
            assert!(n > min, "{n} is not above {min}");
        }
        let bounds = crate::model::NumberBounds {
            exclusive_maximum: Some(-2.0),
            ..Default::default()
        };
        let n: f64 = zero_within(&bounds, false).parse().unwrap();
        assert!(n < -2.0, "{n} is not below -2");
    }
}
