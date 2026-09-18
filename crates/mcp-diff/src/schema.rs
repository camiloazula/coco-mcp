//! JSON Schema comparison with direction-aware severity.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::{Change, ItemKind, Severity};

/// Whether the schema constrains what the client sends or what it receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server (tool arguments).
    Input,
    /// Server → client (structured output).
    Output,
}

impl Direction {
    /// Severity of making the schema stricter.
    fn tighten(self) -> Severity {
        match self {
            Self::Input => Severity::Breaking,
            Self::Output => Severity::Compatible,
        }
    }

    /// Severity of making the schema looser.
    fn loosen(self) -> Severity {
        match self {
            Self::Input => Severity::Compatible,
            Self::Output => Severity::Breaking,
        }
    }
}

const MAX_DEPTH: usize = 32;
const METADATA_KEYS: &[&str] = &[
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "$comment",
    "$schema",
    "$id",
    "readOnly",
    "writeOnly",
];

struct Ctx<'a> {
    root_a: &'a Value,
    root_b: &'a Value,
    direction: Direction,
    kind: ItemKind,
    name: &'a str,
}

/// Compare two schemas rooted at `path` and append classified changes.
pub fn compare(
    a: &Value,
    b: &Value,
    direction: Direction,
    kind: ItemKind,
    name: &str,
    path: &str,
    out: &mut Vec<Change>,
) {
    if a == b {
        return;
    }
    let ctx = Ctx {
        root_a: a,
        root_b: b,
        direction,
        kind,
        name,
    };
    walk(&ctx, a, b, path, 0, out);
}

fn resolve<'v>(root: &'v Value, schema: &'v Value, depth: usize) -> &'v Value {
    let mut current = schema;
    for _ in 0..(MAX_DEPTH - depth.min(MAX_DEPTH)) {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
            return current;
        };
        let Some(target) = reference.strip_prefix('#').and_then(|p| root.pointer(p)) else {
            return current;
        };
        current = target;
    }
    current
}

fn push(
    ctx: &Ctx<'_>,
    out: &mut Vec<Change>,
    severity: Severity,
    path: &str,
    summary: String,
    before: Option<Value>,
    after: Option<Value>,
) {
    out.push(Change {
        severity,
        kind: ctx.kind,
        name: ctx.name.to_owned(),
        path: path.to_owned(),
        summary,
        before,
        after,
    });
}

fn types(schema: &Map<String, Value>) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = match schema.get("type") {
        Some(Value::String(t)) => BTreeSet::from([t.clone()]),
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => BTreeSet::new(),
    };
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(alts)) = schema.get(key)
            && alts
                .iter()
                .any(|alt| alt.get("type").and_then(Value::as_str) == Some("null"))
        {
            set.insert("null".into());
        }
    }
    set
}

fn set_of(v: Option<&Value>) -> BTreeSet<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(ToString::to_string).collect())
        .unwrap_or_default()
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

fn walk(ctx: &Ctx<'_>, a: &Value, b: &Value, path: &str, depth: usize, out: &mut Vec<Change>) {
    if depth > MAX_DEPTH {
        return;
    }
    let a = resolve(ctx.root_a, a, depth);
    let b = resolve(ctx.root_b, b, depth);
    if a == b {
        return;
    }
    let (Some(oa), Some(ob)) = (a.as_object(), b.as_object()) else {
        push(
            ctx,
            out,
            Severity::Breaking,
            path,
            "schema changed".into(),
            Some(a.clone()),
            Some(b.clone()),
        );
        return;
    };
    let mut handled: BTreeSet<&str> = BTreeSet::new();

    // Metadata.
    for key in METADATA_KEYS {
        handled.insert(key);
        if oa.get(*key) != ob.get(*key) {
            push(
                ctx,
                out,
                Severity::Cosmetic,
                &format!("{path}.{key}"),
                format!("{key} changed"),
                oa.get(*key).cloned(),
                ob.get(*key).cloned(),
            );
        }
    }

    // Types (including nullability).
    handled.extend(["type"]);
    let (ta, tb) = (types(oa), types(ob));
    if ta != tb {
        let severity = if ta.is_empty() || tb.is_empty() {
            Severity::Breaking
        } else if tb.is_superset(&ta) {
            ctx.direction.loosen()
        } else if ta.is_superset(&tb) {
            ctx.direction.tighten()
        } else {
            Severity::Breaking
        };
        push(
            ctx,
            out,
            severity,
            &format!("{path}.type"),
            format!("type {} → {}", join(&ta), join(&tb)),
            oa.get("type").cloned(),
            ob.get("type").cloned(),
        );
    }

    // enum / const.
    handled.extend(["enum", "const"]);
    let (ea, eb) = (set_of(oa.get("enum")), set_of(ob.get("enum")));
    if ea != eb {
        let removed: Vec<&String> = ea.difference(&eb).collect();
        let added: Vec<&String> = eb.difference(&ea).collect();
        if !removed.is_empty() {
            push(
                ctx,
                out,
                ctx.direction.tighten(),
                &format!("{path}.enum"),
                format!(
                    "enum values removed: {}",
                    removed
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                oa.get("enum").cloned(),
                ob.get("enum").cloned(),
            );
        }
        if !added.is_empty() {
            push(
                ctx,
                out,
                ctx.direction.loosen(),
                &format!("{path}.enum"),
                format!(
                    "enum values added: {}",
                    added
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                oa.get("enum").cloned(),
                ob.get("enum").cloned(),
            );
        }
    }
    if oa.get("const") != ob.get("const") {
        push(
            ctx,
            out,
            Severity::Breaking,
            &format!("{path}.const"),
            "const changed".into(),
            oa.get("const").cloned(),
            ob.get("const").cloned(),
        );
    }

    // Numeric and length constraints: (key, higher value is stricter).
    for (key, higher_is_stricter) in [
        ("minimum", true),
        ("exclusiveMinimum", true),
        ("minLength", true),
        ("minItems", true),
        ("minProperties", true),
        ("maximum", false),
        ("exclusiveMaximum", false),
        ("maxLength", false),
        ("maxItems", false),
        ("maxProperties", false),
    ] {
        handled.insert(key);
        let (va, vb) = (num(oa.get(key)), num(ob.get(key)));
        if va == vb {
            continue;
        }
        let stricter = match (va, vb) {
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (Some(x), Some(y)) => (y > x) == higher_is_stricter,
            (None, None) => continue,
        };
        let severity = if stricter {
            ctx.direction.tighten()
        } else {
            ctx.direction.loosen()
        };
        push(
            ctx,
            out,
            severity,
            &format!("{path}.{key}"),
            format!("{key} {} → {}", fmt(va), fmt(vb)),
            oa.get(key).cloned(),
            ob.get(key).cloned(),
        );
    }
    for key in ["pattern", "format", "multipleOf", "uniqueItems"] {
        handled.insert(key);
        let (va, vb) = (oa.get(key), ob.get(key));
        if va == vb {
            continue;
        }
        let severity = match (va, vb) {
            (None, Some(_)) => ctx.direction.tighten(),
            (Some(_), None) => ctx.direction.loosen(),
            _ => Severity::Breaking,
        };
        push(
            ctx,
            out,
            severity,
            &format!("{path}.{key}"),
            format!("{key} changed"),
            va.cloned(),
            vb.cloned(),
        );
    }

    // additionalProperties: true/absent → false tightens.
    handled.insert("additionalProperties");
    let forbids =
        |o: &Map<String, Value>| o.get("additionalProperties") == Some(&Value::Bool(false));
    if forbids(oa) != forbids(ob) {
        let severity = if forbids(ob) {
            ctx.direction.tighten()
        } else {
            ctx.direction.loosen()
        };
        push(
            ctx,
            out,
            severity,
            &format!("{path}.additionalProperties"),
            if forbids(ob) {
                "additional properties no longer allowed"
            } else {
                "additional properties now allowed"
            }
            .into(),
            oa.get("additionalProperties").cloned(),
            ob.get("additionalProperties").cloned(),
        );
    }

    // Properties and required.
    handled.extend(["properties", "required"]);
    let (ra, rb) = (set_of(oa.get("required")), set_of(ob.get("required")));
    let empty = Map::new();
    let pa = oa
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let pb = ob
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let quoted = |s: &str| format!("\"{s}\"");
    for (name, sa) in pa {
        let prop_path = format!("{path}.properties.{name}");
        match pb.get(name) {
            None => {
                let severity = match ctx.direction {
                    Direction::Input if forbids(ob) => Severity::Breaking,
                    Direction::Input => Severity::Compatible,
                    Direction::Output => Severity::Breaking,
                };
                push(
                    ctx,
                    out,
                    severity,
                    &prop_path,
                    "property removed".into(),
                    Some(sa.clone()),
                    None,
                );
            }
            Some(sb) => {
                let (was, is) = (ra.contains(&quoted(name)), rb.contains(&quoted(name)));
                if !was && is {
                    push(
                        ctx,
                        out,
                        ctx.direction.tighten(),
                        &prop_path,
                        "property is now required".into(),
                        None,
                        None,
                    );
                } else if was && !is {
                    push(
                        ctx,
                        out,
                        ctx.direction.loosen(),
                        &prop_path,
                        "property is now optional".into(),
                        None,
                        None,
                    );
                }
                walk(ctx, sa, sb, &prop_path, depth + 1, out);
            }
        }
    }
    for (name, sb) in pb.iter().filter(|(n, _)| !pa.contains_key(*n)) {
        let prop_path = format!("{path}.properties.{name}");
        let required = rb.contains(&quoted(name));
        let severity = match (ctx.direction, required) {
            (Direction::Input, true) => Severity::Breaking,
            _ => Severity::Compatible,
        };
        push(
            ctx,
            out,
            severity,
            &prop_path,
            if required {
                "required property added"
            } else {
                "optional property added"
            }
            .into(),
            None,
            Some(sb.clone()),
        );
    }

    // items.
    handled.insert("items");
    match (oa.get("items"), ob.get("items")) {
        (Some(ia), Some(ib)) => walk(ctx, ia, ib, &format!("{path}.items"), depth + 1, out),
        (None, None) => {}
        (ia, ib) => push(
            ctx,
            out,
            Severity::Breaking,
            &format!("{path}.items"),
            "items schema changed".into(),
            ia.cloned(),
            ib.cloned(),
        ),
    }

    // anyOf / oneOf / allOf.
    for key in ["anyOf", "oneOf", "allOf"] {
        handled.insert(key);
        let (la, lb) = (
            oa.get(key).and_then(Value::as_array),
            ob.get(key).and_then(Value::as_array),
        );
        match (la, lb) {
            (Some(la), Some(lb)) if la.len() == lb.len() => {
                for (i, (x, y)) in la.iter().zip(lb).enumerate() {
                    walk(ctx, x, y, &format!("{path}.{key}[{i}]"), depth + 1, out);
                }
            }
            (Some(la), Some(lb)) => {
                let severity = if key == "allOf" {
                    if lb.len() > la.len() {
                        ctx.direction.tighten()
                    } else {
                        ctx.direction.loosen()
                    }
                } else if lb.len() > la.len() {
                    ctx.direction.loosen()
                } else {
                    ctx.direction.tighten()
                };
                push(
                    ctx,
                    out,
                    severity,
                    &format!("{path}.{key}"),
                    format!("{key} alternatives {} → {}", la.len(), lb.len()),
                    None,
                    None,
                );
            }
            (None, None) => {}
            _ => push(
                ctx,
                out,
                Severity::Breaking,
                &format!("{path}.{key}"),
                format!("{key} changed"),
                oa.get(key).cloned(),
                ob.get(key).cloned(),
            ),
        }
    }

    // $defs / definitions are reached through $ref; a definition that is no
    // longer referenced is not a change for callers.
    handled.extend(["$defs", "definitions", "$ref"]);

    // Anything else that differs is reported conservatively.
    for key in oa.keys().chain(ob.keys()).collect::<BTreeSet<_>>() {
        if !handled.contains(key.as_str()) && oa.get(key) != ob.get(key) {
            push(
                ctx,
                out,
                Severity::Breaking,
                &format!("{path}.{key}"),
                format!("{key} changed"),
                oa.get(key).cloned(),
                ob.get(key).cloned(),
            );
        }
    }
}

fn join(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        "any".into()
    } else {
        set.iter().cloned().collect::<Vec<_>>().join("|")
    }
}

fn fmt(v: Option<f64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "none".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(a: Value, b: Value, direction: Direction) -> Vec<(Severity, String, String)> {
        let mut out = Vec::new();
        compare(&a, &b, direction, ItemKind::Tool, "t", "schema", &mut out);
        out.into_iter()
            .map(|c| (c.severity, c.path, c.summary))
            .collect()
    }

    #[test]
    fn new_required_input_field_is_breaking_and_optional_is_compatible() {
        let a =
            json!({"type": "object", "properties": {"a": {"type": "number"}}, "required": ["a"]});
        let b = json!({"type": "object", "properties": {"a": {"type": "number"}, "p": {"type": "integer"}, "q": {"type": "string"}}, "required": ["a", "p"]});
        let changes = run(a, b, Direction::Input);
        assert!(changes.contains(&(
            Severity::Breaking,
            "schema.properties.p".into(),
            "required property added".into()
        )));
        assert!(changes.contains(&(
            Severity::Compatible,
            "schema.properties.q".into(),
            "optional property added".into()
        )));
    }

    #[test]
    fn bounds_patterns_and_formats_by_direction() {
        let at = |changes: &[(Severity, String, String)], path: &str| {
            changes.iter().find(|c| c.1 == path).map(|c| c.0)
        };
        let a = json!({"type": "string", "minLength": 1, "maxLength": 80});
        let b = json!({"type": "string", "minLength": 2, "maxLength": 100, "pattern": "^[a-z]+$"});
        let input = run(a.clone(), b.clone(), Direction::Input);
        assert_eq!(at(&input, "schema.minLength"), Some(Severity::Breaking));
        assert_eq!(at(&input, "schema.maxLength"), Some(Severity::Compatible));
        assert_eq!(at(&input, "schema.pattern"), Some(Severity::Breaking));
        let output = run(a, b, Direction::Output);
        assert_eq!(at(&output, "schema.minLength"), Some(Severity::Compatible));
        assert_eq!(at(&output, "schema.maxLength"), Some(Severity::Breaking));
        assert_eq!(at(&output, "schema.pattern"), Some(Severity::Compatible));

        // A dropped bound loosens; a format that changes breaks either way.
        let c = json!({"type": "number", "minimum": 0, "format": "float"});
        let d = json!({"type": "number", "format": "double", "multipleOf": 0.5});
        let changes = run(c, d, Direction::Input);
        assert_eq!(at(&changes, "schema.minimum"), Some(Severity::Compatible));
        assert_eq!(at(&changes, "schema.format"), Some(Severity::Breaking));
        assert_eq!(at(&changes, "schema.multipleOf"), Some(Severity::Breaking));
    }

    #[test]
    fn output_direction_flips_tightening() {
        let a = json!({"type": "object", "properties": {"v": {"type": "number", "minimum": 0}}, "required": ["v"]});
        let b = json!({"type": "object", "properties": {"v": {"type": "number", "minimum": 1}}});
        let input = run(a.clone(), b.clone(), Direction::Input);
        assert!(input.contains(&(
            Severity::Breaking,
            "schema.properties.v.minimum".into(),
            "minimum 0 → 1".into()
        )));
        assert!(input.contains(&(
            Severity::Compatible,
            "schema.properties.v".into(),
            "property is now optional".into()
        )));
        let output = run(a, b, Direction::Output);
        assert!(output.contains(&(
            Severity::Compatible,
            "schema.properties.v.minimum".into(),
            "minimum 0 → 1".into()
        )));
        assert!(output.contains(&(
            Severity::Breaking,
            "schema.properties.v".into(),
            "property is now optional".into()
        )));
    }

    #[test]
    fn enums_types_nullability_and_metadata() {
        let a = json!({"properties": {"e": {"enum": ["x", "y"]}, "n": {"anyOf": [{"type": "string"}, {"type": "null"}]}, "t": {"type": "integer", "description": "old"}}});
        let b = json!({"properties": {"e": {"enum": ["x"]}, "n": {"type": "string"}, "t": {"type": "integer", "description": "new"}}});
        let changes = run(a, b, Direction::Input);
        assert!(changes.iter().any(|(s, p, m)| *s == Severity::Breaking
            && p == "schema.properties.e.enum"
            && m.contains("removed: \"y\"")));
        assert!(
            changes
                .iter()
                .any(|(s, p, _)| *s == Severity::Breaking && p == "schema.properties.n.type")
        );
        assert!(changes.iter().any(|(s, p, _)| *s == Severity::Cosmetic && p == "schema.properties.t.description"));
    }

    #[test]
    fn refs_are_resolved_through_defs() {
        let a = json!({"$defs": {"P": {"type": "object", "properties": {"x": {"type": "string"}}}}, "properties": {"p": {"$ref": "#/$defs/P"}}});
        let b = json!({"$defs": {"P": {"type": "object", "properties": {"x": {"type": "string"}, "y": {"type": "string"}}, "required": ["y"]}}, "properties": {"p": {"$ref": "#/$defs/P"}}});
        let changes = run(a, b, Direction::Input);
        assert_eq!(
            changes,
            vec![(
                Severity::Breaking,
                "schema.properties.p.properties.y".into(),
                "required property added".into()
            )]
        );
    }

    #[test]
    fn removed_property_depends_on_additional_properties() {
        let a = json!({"properties": {"old": {"type": "string"}}});
        let loose = json!({"properties": {}});
        let strict = json!({"properties": {}, "additionalProperties": false});
        assert_eq!(
            run(a.clone(), loose, Direction::Input)[0].0,
            Severity::Compatible
        );
        let changes = run(a, strict, Direction::Input);
        assert!(
            changes
                .iter()
                .any(|(s, p, _)| *s == Severity::Breaking && p == "schema.properties.old")
        );
    }

    #[test]
    fn unknown_keyword_changes_are_breaking_and_cycles_terminate() {
        let a = json!({"type": "string", "contentEncoding": "base64"});
        let b = json!({"type": "string"});
        assert_eq!(run(a, b, Direction::Input)[0].0, Severity::Breaking);
        let cyc_a = json!({"$defs": {"N": {"properties": {"next": {"$ref": "#/$defs/N"}}}}, "$ref": "#/$defs/N"});
        let cyc_b = json!({"$defs": {"N": {"properties": {"next": {"$ref": "#/$defs/N"}, "v": {"type": "integer"}}}}, "$ref": "#/$defs/N"});
        let changes = run(cyc_a, cyc_b, Direction::Input);
        assert!(!changes.is_empty());
    }
}
