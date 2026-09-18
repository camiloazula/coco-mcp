//! Every fixture schema must (1) build a model without panicking, (2) produce
//! a default value that validates against the original schema, and
//! (3) round-trip that value through `state_from_value`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use mcp_schema_form::{FormModel, validate};
use serde_json::Value;

fn fixture(name: &str) -> Vec<(String, Value)> {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let tools: Vec<Value> = serde_json::from_str(&text).unwrap();
    tools
        .into_iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_owned(),
                t["inputSchema"].clone(),
            )
        })
        .collect()
}

/// `path` (`$.a.b`) names a required text field with `minLength >= 1`.
fn is_empty_min_length(model: &FormModel, path: &str) -> bool {
    let mut current = model;
    for segment in path
        .trim_start_matches('$')
        .split('.')
        .filter(|s| !s.is_empty())
    {
        match current.field(segment) {
            Some(field) => current = &field.model,
            None => return false,
        }
    }
    matches!(current, FormModel::Text { min_length: Some(n), .. } if *n >= 1)
}

fn check_all(name: &str) -> Vec<(String, FormModel)> {
    let mut out = Vec::new();
    for (tool, schema) in fixture(name) {
        let model = FormModel::from_schema(&schema);
        assert!(
            matches!(model, FormModel::Object { .. }),
            "{name}/{tool}: top level is {}",
            model.kind()
        );
        let value = model.default_json();
        // A fresh form starts every string empty on purpose; a required
        // string with `minLength` is therefore the one violation the initial
        // state is allowed to carry (the UI shows it as a validation error).
        let issues: Vec<_> = validate(&schema, &value)
            .into_iter()
            .filter(|i| !is_empty_min_length(&model, &i.path))
            .collect();
        assert!(
            issues.is_empty(),
            "{name}/{tool}: default {value} failed: {issues:?}"
        );
        let state = model.state_from_value(&value).expect("state from default");
        assert_eq!(model.to_json(&state).unwrap(), value, "{name}/{tool}");
        out.push((tool, model));
    }
    out
}

#[test]
fn filesystem_server() {
    let models = check_all("filesystem");
    assert!(models.iter().all(|(_, m)| !m.has_unknown()));
    let (_, edit) = models.iter().find(|(n, _)| n == "edit_file").unwrap();
    let edits = edit.field("edits").unwrap();
    assert!(edits.required);
    match &edits.model {
        FormModel::Array { items, .. } => {
            assert!(items.field("oldText").unwrap().required);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        edit.field("dryRun").unwrap().model.meta().default,
        Some(Value::Bool(false))
    );
}

#[test]
fn github_server() {
    let models = check_all("github");
    assert!(models.iter().all(|(_, m)| !m.has_unknown()));
    let (_, review) = models
        .iter()
        .find(|(n, _)| n == "create_pull_request_review")
        .unwrap();
    match &review.field("event").unwrap().model {
        FormModel::Enum { options, .. } => assert_eq!(options.len(), 3),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        review.field("pull_number").unwrap().model,
        FormModel::Number { .. }
    ));
}

#[test]
fn pydantic_server() {
    let models = check_all("pydantic");
    let (_, create) = models.iter().find(|(n, _)| n == "create_user").unwrap();
    let role = &create.field("role").unwrap().model;
    assert_eq!(role.kind(), "enum");
    assert_eq!(role.meta().default, Some(Value::String("member".into())));
    let age = &create.field("age").unwrap().model;
    assert_eq!(age.kind(), "integer");
    assert!(age.meta().nullable);
    let address = &create.field("address").unwrap().model;
    assert_eq!(address.kind(), "object");
    assert!(address.meta().nullable);
    assert_eq!(address.field("zip").unwrap().model.kind(), "text");
    // Free-form dict is the one place raw JSON is expected.
    assert_eq!(create.field("metadata").unwrap().model.kind(), "unknown");

    let (_, search) = models.iter().find(|(n, _)| n == "search").unwrap();
    let kind = &search.field("kind").unwrap().model;
    match kind {
        FormModel::Enum { options, meta } => {
            assert_eq!(options[0].label, "Web");
            assert!(meta.nullable);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(search.field("filters").unwrap().model.kind(), "one_of");
    assert_eq!(search.default_json(), serde_json::json!({"query": ""}));
}

#[test]
fn schemars_server() {
    let models = check_all("schemars");
    let (_, run) = models.iter().find(|(n, _)| n == "run_query").unwrap();
    let timeout = &run.field("timeout_ms").unwrap().model;
    assert_eq!(timeout.kind(), "integer");
    assert!(timeout.meta().nullable);
    match &run.field("params").unwrap().model {
        FormModel::Array { items, .. } => {
            assert_eq!(items.field("value").unwrap().model.kind(), "one_of");
        }
        other => panic!("{other:?}"),
    }
    match &run.field("mode").unwrap().model {
        FormModel::Enum { options, .. } => {
            let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(labels, ["read_only", "read_write"]);
        }
        other => panic!("{other:?}"),
    }
    let options = &run.field("options").unwrap().model;
    assert_eq!(options.kind(), "object");
    assert_eq!(
        options.field("max_rows").unwrap().model.meta().default,
        Some(Value::from(1000))
    );
    let (_, empty) = models.iter().find(|(n, _)| n == "empty").unwrap();
    assert_eq!(empty.default_json(), serde_json::json!({}));
}
