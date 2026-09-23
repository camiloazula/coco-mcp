//! JSON Schema -> [`FormModel`].
//!
//! Hand-written: no maintained crate turns an arbitrary JSON Schema into a
//! form tree with graceful fallback. Validation is delegated to `jsonschema`;
//! this module only classifies schemas for rendering.

use serde_json::{Map, Value};

use crate::model::{EnumOption, Field, FormModel, Meta, NumberBounds, Variant};

/// Maximum `$ref` hops followed for one node before giving up.
const MAX_REF_DEPTH: usize = 32;

struct Builder<'a> {
    root: &'a Value,
}

impl FormModel {
    /// Build a model from a schema. Never fails: unsupported constructs become
    /// [`FormModel::Unknown`].
    pub fn from_schema(schema: &Value) -> Self {
        Builder { root: schema }.build(schema, &mut Vec::new())
    }
}

impl Builder<'_> {
    fn build(&self, schema: &Value, ref_stack: &mut Vec<String>) -> FormModel {
        match schema {
            Value::Bool(true) => FormModel::Unknown {
                meta: Meta::default(),
                raw: schema.clone(),
            },
            Value::Object(obj) => self.build_object(obj, schema, ref_stack),
            _ => FormModel::Unknown {
                meta: Meta::default(),
                raw: schema.clone(),
            },
        }
    }

    fn build_object(
        &self,
        obj: &Map<String, Value>,
        raw: &Value,
        ref_stack: &mut Vec<String>,
    ) -> FormModel {
        // $ref: resolve, then merge sibling keywords (title/description/default) over it.
        if let Some(Value::String(reference)) = obj.get("$ref") {
            if ref_stack.len() >= MAX_REF_DEPTH || ref_stack.iter().any(|r| r == reference) {
                return FormModel::Unknown {
                    meta: meta_of(obj, false),
                    raw: raw.clone(),
                };
            }
            let Some(target) = self.resolve_ref(reference) else {
                return FormModel::Unknown {
                    meta: meta_of(obj, false),
                    raw: raw.clone(),
                };
            };
            ref_stack.push(reference.clone());
            let mut model = self.build(target, ref_stack);
            ref_stack.pop();
            overlay_meta(model.meta_mut(), obj);
            return model;
        }

        let mut nullable = false;

        // Combinators.
        for key in ["anyOf", "oneOf"] {
            if let Some(Value::Array(alts)) = obj.get(key) {
                let (nulls, others): (Vec<&Value>, Vec<&Value>) =
                    alts.iter().partition(|alt| is_null_schema(alt));
                nullable |= !nulls.is_empty();
                return match others.len() {
                    0 => FormModel::Unknown {
                        meta: meta_of(obj, nullable),
                        raw: raw.clone(),
                    },
                    1 => {
                        let mut model = self.build(others[0], ref_stack);
                        overlay_meta(model.meta_mut(), obj);
                        model.meta_mut().nullable |= nullable;
                        model
                    }
                    _ => self.build_alternatives(obj, &others, nullable, ref_stack),
                };
            }
        }
        if let Some(Value::Array(parts)) = obj.get("allOf") {
            return self.build_all_of(obj, parts, raw, ref_stack);
        }

        // enum / const.
        if let Some(Value::Array(values)) = obj.get("enum") {
            let non_null: Vec<&Value> = values.iter().filter(|v| !v.is_null()).collect();
            nullable |= non_null.len() != values.len();
            return FormModel::Enum {
                meta: meta_of(obj, nullable),
                options: non_null
                    .into_iter()
                    .map(|v| EnumOption {
                        value: v.clone(),
                        label: label_of(v),
                    })
                    .collect(),
            };
        }
        if let Some(value) = obj.get("const") {
            return FormModel::Enum {
                meta: meta_of(obj, nullable),
                options: vec![EnumOption {
                    value: value.clone(),
                    label: label_of(value),
                }],
            };
        }

        // type: string | [string, "null"] | absent (inferred from keywords).
        let types = declared_types(obj);
        let non_null: Vec<&str> = types.iter().copied().filter(|t| *t != "null").collect();
        nullable |= types.len() != non_null.len();
        let inferred;
        let ty = match non_null.as_slice() {
            [single] => *single,
            [] => {
                inferred = infer_type(obj);
                match inferred {
                    Some(t) => t,
                    None => {
                        return FormModel::Unknown {
                            meta: meta_of(obj, nullable),
                            raw: raw.clone(),
                        };
                    }
                }
            }
            many => {
                // Several concrete types: offer each as an alternative.
                let alts: Vec<Value> = many
                    .iter()
                    .map(|t| {
                        let mut sub = obj.clone();
                        sub.insert("type".into(), Value::String((*t).to_owned()));
                        Value::Object(sub)
                    })
                    .collect();
                let refs: Vec<&Value> = alts.iter().collect();
                return self.build_alternatives(obj, &refs, nullable, ref_stack);
            }
        };

        let meta = meta_of(obj, nullable);
        match ty {
            "string" => FormModel::Text {
                meta,
                format: str_of(obj, "format"),
                min_length: u64_of(obj, "minLength"),
                max_length: u64_of(obj, "maxLength"),
                pattern: str_of(obj, "pattern"),
            },
            "number" => FormModel::Number {
                meta,
                bounds: bounds_of(obj),
            },
            "integer" => FormModel::Integer {
                meta,
                bounds: bounds_of(obj),
            },
            "boolean" => FormModel::Bool { meta },
            "object" => self.build_properties(obj, meta, raw, ref_stack),
            "array" => match obj.get("items") {
                Some(items @ (Value::Object(_) | Value::Bool(true))) => FormModel::Array {
                    meta,
                    items: Box::new(self.build(items, ref_stack)),
                    min_items: u64_of(obj, "minItems"),
                    max_items: u64_of(obj, "maxItems"),
                },
                // Tuple validation (`items: [...]`, `prefixItems`) or untyped arrays.
                _ => FormModel::Unknown {
                    meta,
                    raw: raw.clone(),
                },
            },
            _ => FormModel::Unknown {
                meta,
                raw: raw.clone(),
            },
        }
    }

    fn build_properties(
        &self,
        obj: &Map<String, Value>,
        meta: Meta,
        raw: &Value,
        ref_stack: &mut Vec<String>,
    ) -> FormModel {
        let required: Vec<&str> = obj
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        match obj.get("properties") {
            Some(Value::Object(props)) => FormModel::Object {
                meta,
                fields: props
                    .iter()
                    .map(|(name, schema)| Field {
                        name: name.clone(),
                        required: required.contains(&name.as_str()),
                        model: self.build(schema, ref_stack),
                    })
                    .collect(),
            },
            // No properties at all: an empty object is a legitimate form
            // (tools with no arguments). Anything map-like is raw JSON.
            None if !obj.contains_key("additionalProperties")
                && !obj.contains_key("patternProperties") =>
            {
                FormModel::Object {
                    meta,
                    fields: Vec::new(),
                }
            }
            _ => FormModel::Unknown {
                meta,
                raw: raw.clone(),
            },
        }
    }

    fn build_alternatives(
        &self,
        obj: &Map<String, Value>,
        alts: &[&Value],
        nullable: bool,
        ref_stack: &mut Vec<String>,
    ) -> FormModel {
        let models: Vec<FormModel> = alts.iter().map(|a| self.build(a, ref_stack)).collect();
        // A union of consts is an enum.
        if models.iter().all(|m| matches!(m, FormModel::Enum { .. })) {
            let options = models
                .into_iter()
                .flat_map(|m| match m {
                    FormModel::Enum { options, meta } => options
                        .into_iter()
                        .map(move |mut o| {
                            if let Some(title) = &meta.title {
                                o.label = title.clone();
                            }
                            o
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                })
                .collect();
            return FormModel::Enum {
                meta: meta_of(obj, nullable),
                options,
            };
        }
        let labels: Vec<String> = models.iter().map(variant_label).collect();
        // Two alternatives that read the same are told apart by their place.
        let repeated: Vec<bool> = labels
            .iter()
            .map(|l| labels.iter().filter(|other| *other == l).count() > 1)
            .collect();
        FormModel::OneOf {
            meta: meta_of(obj, nullable),
            variants: models
                .into_iter()
                .zip(labels)
                .zip(repeated)
                .enumerate()
                .map(|(i, ((model, label), repeated))| Variant {
                    label: if repeated {
                        format!("{label} {}", i + 1)
                    } else {
                        label
                    },
                    model,
                })
                .collect(),
        }
    }

    fn build_all_of(
        &self,
        obj: &Map<String, Value>,
        parts: &[Value],
        raw: &Value,
        ref_stack: &mut Vec<String>,
    ) -> FormModel {
        // Merge object parts into one property map; anything else is raw.
        let mut merged = Map::new();
        let mut required = Vec::new();
        for part in parts {
            let resolved = match part.get("$ref").and_then(Value::as_str) {
                Some(r) => self.resolve_ref(r),
                None => Some(part),
            };
            let Some(Value::Object(p)) = resolved else {
                return FormModel::Unknown {
                    meta: meta_of(obj, false),
                    raw: raw.clone(),
                };
            };
            let is_object = p.get("type").and_then(Value::as_str) == Some("object")
                || p.contains_key("properties");
            if !is_object {
                return FormModel::Unknown {
                    meta: meta_of(obj, false),
                    raw: raw.clone(),
                };
            }
            if let Some(Value::Object(props)) = p.get("properties") {
                for (k, v) in props {
                    merged.insert(k.clone(), v.clone());
                }
            }
            if let Some(Value::Array(req)) = p.get("required") {
                required.extend(req.iter().cloned());
            }
        }
        let mut synthetic = obj.clone();
        synthetic.remove("allOf");
        synthetic.insert("type".into(), Value::String("object".into()));
        synthetic.insert("properties".into(), Value::Object(merged));
        synthetic.insert("required".into(), Value::Array(required));
        let meta = meta_of(&synthetic, false);
        self.build_properties(&synthetic, meta, raw, ref_stack)
    }

    /// Resolve a local reference (`#/$defs/X`, `#/definitions/X`, any `#/...` pointer).
    fn resolve_ref(&self, reference: &str) -> Option<&Value> {
        let pointer = reference.strip_prefix('#')?;
        if pointer.is_empty() {
            return Some(self.root);
        }
        // JSON Pointer unescaping is handled by serde_json.
        self.root.pointer(pointer)
    }
}

fn is_null_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
        || schema.get("const").is_some_and(Value::is_null)
}

fn declared_types(obj: &Map<String, Value>) -> Vec<&str> {
    match obj.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn infer_type(obj: &Map<String, Value>) -> Option<&'static str> {
    if obj.contains_key("properties") || obj.contains_key("required") {
        Some("object")
    } else if obj.contains_key("items") {
        Some("array")
    } else if ["minLength", "maxLength", "pattern", "format"]
        .iter()
        .any(|k| obj.contains_key(*k))
    {
        Some("string")
    } else if ["minimum", "maximum", "multipleOf"]
        .iter()
        .any(|k| obj.contains_key(*k))
    {
        Some("number")
    } else {
        None
    }
}

fn meta_of(obj: &Map<String, Value>, nullable: bool) -> Meta {
    Meta {
        title: str_of(obj, "title"),
        description: str_of(obj, "description"),
        default: obj.get("default").cloned(),
        nullable: nullable || obj.get("default").is_some_and(Value::is_null),
        deprecated: obj
            .get("deprecated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        examples: obj
            .get("examples")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

/// Keywords next to a `$ref` (or wrapping a combinator) win over the target's.
fn overlay_meta(meta: &mut Meta, obj: &Map<String, Value>) {
    if let Some(t) = str_of(obj, "title") {
        meta.title = Some(t);
    }
    if let Some(d) = str_of(obj, "description") {
        meta.description = Some(d);
    }
    if let Some(d) = obj.get("default") {
        meta.default = Some(d.clone());
        if d.is_null() {
            meta.nullable = true;
        }
    }
    if obj.get("deprecated").and_then(Value::as_bool) == Some(true) {
        meta.deprecated = true;
    }
}

fn str_of(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn u64_of(obj: &Map<String, Value>, key: &str) -> Option<u64> {
    obj.get(key).and_then(Value::as_u64)
}

fn f64_of(obj: &Map<String, Value>, key: &str) -> Option<f64> {
    obj.get(key).and_then(Value::as_f64)
}

fn bounds_of(obj: &Map<String, Value>) -> NumberBounds {
    NumberBounds {
        minimum: f64_of(obj, "minimum"),
        maximum: f64_of(obj, "maximum"),
        exclusive_minimum: f64_of(obj, "exclusiveMinimum"),
        exclusive_maximum: f64_of(obj, "exclusiveMaximum"),
        multiple_of: f64_of(obj, "multipleOf"),
    }
}

/// What an alternative is called in the selector: its `title`; for an
/// object, the one value of its tag field (`kind: {const: "circle"}`, the
/// way tagged unions are written); otherwise its kind, as `string` or
/// `integer`.
fn variant_label(model: &FormModel) -> String {
    if let Some(title) = &model.meta().title {
        return title.clone();
    }
    if let FormModel::Object { fields, .. } = model
        && let Some(tag) = fields.iter().find_map(|f| match &f.model {
            FormModel::Enum { options, .. } if options.len() == 1 => Some(options[0].label.clone()),
            _ => None,
        })
    {
        return tag;
    }
    model.kind_label().to_owned()
}

fn label_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_defs_and_definitions() {
        let schema = json!({
            "$defs": {"Name": {"type": "string", "description": "a name"}},
            "definitions": {"Age": {"type": "integer", "minimum": 0}},
            "type": "object",
            "properties": {
                "name": {"$ref": "#/$defs/Name", "title": "Name override"},
                "age": {"$ref": "#/definitions/Age"}
            },
            "required": ["name"]
        });
        let model = FormModel::from_schema(&schema);
        let name = model.field("name").unwrap();
        assert!(name.required);
        assert!(matches!(name.model, FormModel::Text { .. }));
        assert_eq!(name.model.meta().title.as_deref(), Some("Name override"));
        assert_eq!(name.model.meta().description.as_deref(), Some("a name"));
        let age = model.field("age").unwrap();
        assert!(
            matches!(age.model, FormModel::Integer { ref bounds, .. } if bounds.minimum == Some(0.0))
        );
    }

    #[test]
    fn cyclic_refs_fall_back_to_unknown() {
        let schema = json!({
            "$defs": {"Node": {"type": "object", "properties": {"next": {"$ref": "#/$defs/Node"}}}},
            "$ref": "#/$defs/Node"
        });
        let model = FormModel::from_schema(&schema);
        assert!(matches!(model, FormModel::Object { .. }));
        assert!(model.has_unknown());
    }

    #[test]
    fn unwraps_nullable_anyof_and_type_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": {"anyOf": [{"type": "string"}, {"type": "null"}], "default": null},
                "b": {"type": ["integer", "null"]},
                "c": {"oneOf": [{"type": "null"}, {"type": "boolean"}]}
            }
        });
        let model = FormModel::from_schema(&schema);
        for (name, kind) in [("a", "text"), ("b", "integer"), ("c", "bool")] {
            let f = model.field(name).unwrap();
            assert_eq!(f.model.kind(), kind, "{name}");
            assert!(f.model.meta().nullable, "{name}");
        }
    }

    #[test]
    fn enums_and_const_unions() {
        let schema = json!({
            "type": "object",
            "properties": {
                "color": {"enum": ["red", "green", null]},
                "mode": {"oneOf": [
                    {"const": "fast", "title": "Fast"},
                    {"const": "slow", "title": "Slow"}
                ]},
                "one": {"const": 1}
            }
        });
        let model = FormModel::from_schema(&schema);
        match &model.field("color").unwrap().model {
            FormModel::Enum { options, meta } => {
                assert_eq!(options.len(), 2);
                assert!(meta.nullable);
            }
            other => panic!("{other:?}"),
        }
        match &model.field("mode").unwrap().model {
            FormModel::Enum { options, .. } => {
                assert_eq!(options[0].label, "Fast");
                assert_eq!(options[1].value, json!("slow"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            model.field("one").unwrap().model,
            FormModel::Enum { .. }
        ));
    }

    #[test]
    fn oneof_with_objects_becomes_variants() {
        let schema = json!({
            "oneOf": [
                {"type": "object", "title": "By id", "properties": {"id": {"type": "integer"}}},
                {"type": "object", "properties": {"name": {"type": "string"}}}
            ]
        });
        match FormModel::from_schema(&schema) {
            FormModel::OneOf { variants, .. } => {
                assert_eq!(variants[0].label, "By id");
                assert_eq!(variants[1].label, "object");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn multiple_types_become_variants() {
        let schema = json!({"type": ["string", "integer"]});
        match FormModel::from_schema(&schema) {
            FormModel::OneOf { variants, .. } => {
                let labels: Vec<&str> = variants.iter().map(|v| v.label.as_str()).collect();
                assert_eq!(labels, ["string", "integer"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn variants_are_named_by_tag_kind_or_place() {
        // Tagged objects take the tag's one value.
        let shapes = json!({"oneOf": [
            {"type": "object", "properties": {"kind": {"type": "string", "const": "circle"}, "radius": {"type": "number"}}, "required": ["kind"]},
            {"type": "object", "properties": {"kind": {"type": "string", "enum": ["rect"]}, "width": {"type": "number"}}, "required": ["kind"]}
        ]});
        let labels = |schema: &Value| match FormModel::from_schema(schema) {
            FormModel::OneOf { variants, .. } => {
                variants.into_iter().map(|v| v.label).collect::<Vec<_>>()
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(labels(&shapes), ["circle", "rect"]);
        // Untagged objects that read the same are numbered.
        let plain = json!({"oneOf": [
            {"type": "object", "properties": {"a": {"type": "string"}}},
            {"type": "object", "properties": {"b": {"type": "string"}}}
        ]});
        assert_eq!(labels(&plain), ["object 1", "object 2"]);
    }

    #[test]
    fn arrays_maps_and_tuples() {
        let schema = json!({
            "type": "object",
            "properties": {
                "list": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "map": {"type": "object", "additionalProperties": {"type": "number"}},
                "tuple": {"type": "array", "items": [{"type": "string"}]},
                "anything": {},
                "empty": {"type": "object"}
            }
        });
        let model = FormModel::from_schema(&schema);
        assert_eq!(model.field("list").unwrap().model.kind(), "array");
        assert_eq!(model.field("map").unwrap().model.kind(), "unknown");
        assert_eq!(model.field("tuple").unwrap().model.kind(), "unknown");
        assert_eq!(model.field("anything").unwrap().model.kind(), "unknown");
        assert_eq!(model.field("empty").unwrap().model.kind(), "object");
    }

    #[test]
    fn all_of_merges_object_parts() {
        let schema = json!({
            "$defs": {"Base": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}},
            "allOf": [
                {"$ref": "#/$defs/Base"},
                {"type": "object", "properties": {"name": {"type": "string"}}}
            ]
        });
        let model = FormModel::from_schema(&schema);
        assert!(model.field("id").unwrap().required);
        assert!(!model.field("name").unwrap().required);
    }

    #[test]
    fn inferred_types_and_metadata() {
        let schema = json!({
            "type": "object",
            "properties": {
                "s": {"minLength": 2, "deprecated": true, "examples": ["ab"]},
                "n": {"maximum": 5, "default": 2, "description": "d"}
            }
        });
        let model = FormModel::from_schema(&schema);
        let s = &model.field("s").unwrap().model;
        assert_eq!(s.kind(), "text");
        assert!(s.meta().deprecated);
        assert_eq!(s.meta().examples.len(), 1);
        let n = &model.field("n").unwrap().model;
        assert_eq!(n.kind(), "number");
        assert_eq!(n.meta().default, Some(json!(2)));
        assert_eq!(n.meta().description.as_deref(), Some("d"));
    }

    #[test]
    fn garbage_never_panics() {
        for schema in [
            json!(null),
            json!(false),
            json!("x"),
            json!([1]),
            json!({"type": 5}),
            json!({"$ref": "#/nope"}),
        ] {
            let model = FormModel::from_schema(&schema);
            assert_eq!(model.kind(), "unknown");
        }
    }
}
