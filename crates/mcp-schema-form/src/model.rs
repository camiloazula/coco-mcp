//! The form model tree.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Presentation metadata shared by every node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Meta {
    /// `title`.
    pub title: Option<String>,
    /// `description`.
    pub description: Option<String>,
    /// `default`, verbatim.
    pub default: Option<Value>,
    /// `true` when the schema also allows `null` (`anyOf: [T, null]`,
    /// `type: [T, "null"]`).
    pub nullable: bool,
    /// `deprecated`.
    pub deprecated: bool,
    /// `examples`, verbatim.
    pub examples: Vec<Value>,
}

/// Numeric constraints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NumberBounds {
    /// `minimum` (inclusive).
    pub minimum: Option<f64>,
    /// `maximum` (inclusive).
    pub maximum: Option<f64>,
    /// `exclusiveMinimum`.
    pub exclusive_minimum: Option<f64>,
    /// `exclusiveMaximum`.
    pub exclusive_maximum: Option<f64>,
    /// `multipleOf`.
    pub multiple_of: Option<f64>,
}

/// One choice of an [`FormModel::Enum`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumOption {
    /// The JSON value sent when selected.
    pub value: Value,
    /// Label shown to the user.
    pub label: String,
}

/// One property of an [`FormModel::Object`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Property name (the JSON key).
    pub name: String,
    /// Whether the property is listed in `required`.
    pub required: bool,
    /// The property's model.
    pub model: FormModel,
}

/// One alternative of a [`FormModel::OneOf`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    /// Label shown in the selector (`title`, or a synthesized name).
    pub label: String,
    /// The alternative's model.
    pub model: FormModel,
}

/// A renderable form node derived from a JSON Schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FormModel {
    /// `type: string`.
    Text {
        /// Presentation metadata.
        meta: Meta,
        /// `format` (`uri`, `date-time`, `email`, ...).
        format: Option<String>,
        /// `minLength`.
        min_length: Option<u64>,
        /// `maxLength`.
        max_length: Option<u64>,
        /// `pattern`.
        pattern: Option<String>,
    },
    /// `type: number`.
    Number {
        /// Presentation metadata.
        meta: Meta,
        /// Numeric constraints.
        bounds: NumberBounds,
    },
    /// `type: integer`.
    Integer {
        /// Presentation metadata.
        meta: Meta,
        /// Numeric constraints.
        bounds: NumberBounds,
    },
    /// `type: boolean`.
    Bool {
        /// Presentation metadata.
        meta: Meta,
    },
    /// `enum`, `const`, or a `oneOf`/`anyOf` made only of `const`s.
    Enum {
        /// Presentation metadata.
        meta: Meta,
        /// Choices in schema order.
        options: Vec<EnumOption>,
    },
    /// `type: object` with known `properties`.
    Object {
        /// Presentation metadata.
        meta: Meta,
        /// Properties in schema order.
        fields: Vec<Field>,
    },
    /// `type: array` with a single `items` schema.
    Array {
        /// Presentation metadata.
        meta: Meta,
        /// Model of each element.
        items: Box<FormModel>,
        /// `minItems`.
        min_items: Option<u64>,
        /// `maxItems`.
        max_items: Option<u64>,
    },
    /// `oneOf`/`anyOf` with several non-null alternatives.
    OneOf {
        /// Presentation metadata.
        meta: Meta,
        /// Alternatives in schema order.
        variants: Vec<Variant>,
    },
    /// Anything else; rendered as a raw JSON editor.
    Unknown {
        /// Presentation metadata.
        meta: Meta,
        /// The schema as received (after `$ref` resolution when possible).
        raw: Value,
    },
}

impl FormModel {
    /// Presentation metadata of this node.
    pub fn meta(&self) -> &Meta {
        match self {
            Self::Text { meta, .. }
            | Self::Number { meta, .. }
            | Self::Integer { meta, .. }
            | Self::Bool { meta }
            | Self::Enum { meta, .. }
            | Self::Object { meta, .. }
            | Self::Array { meta, .. }
            | Self::OneOf { meta, .. }
            | Self::Unknown { meta, .. } => meta,
        }
    }

    /// Mutable presentation metadata.
    pub(crate) fn meta_mut(&mut self) -> &mut Meta {
        match self {
            Self::Text { meta, .. }
            | Self::Number { meta, .. }
            | Self::Integer { meta, .. }
            | Self::Bool { meta }
            | Self::Enum { meta, .. }
            | Self::Object { meta, .. }
            | Self::Array { meta, .. }
            | Self::OneOf { meta, .. }
            | Self::Unknown { meta, .. } => meta,
        }
    }

    /// Short kind name for logs and tests.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Number { .. } => "number",
            Self::Integer { .. } => "integer",
            Self::Bool { .. } => "bool",
            Self::Enum { .. } => "enum",
            Self::Object { .. } => "object",
            Self::Array { .. } => "array",
            Self::OneOf { .. } => "one_of",
            Self::Unknown { .. } => "unknown",
        }
    }

    /// The kind as a user reads it: `string` for text, `boolean`, `json`
    /// for a schema without a form, `one of` for alternatives.
    pub fn kind_label(&self) -> &'static str {
        match self.kind() {
            "text" => "string",
            "bool" => "boolean",
            "one_of" => "one of",
            "unknown" => "json",
            other => other,
        }
    }

    /// Whether any node in the tree fell back to [`FormModel::Unknown`].
    pub fn has_unknown(&self) -> bool {
        match self {
            Self::Unknown { .. } => true,
            Self::Object { fields, .. } => fields.iter().any(|f| f.model.has_unknown()),
            Self::Array { items, .. } => items.has_unknown(),
            Self::OneOf { variants, .. } => variants.iter().any(|v| v.model.has_unknown()),
            _ => false,
        }
    }

    /// Look up a field of an object model by name.
    pub fn field(&self, name: &str) -> Option<&Field> {
        match self {
            Self::Object { fields, .. } => fields.iter().find(|f| f.name == name),
            _ => None,
        }
    }
}
