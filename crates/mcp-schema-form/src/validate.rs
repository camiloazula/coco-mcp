//! Validation of a JSON value against a schema, with readable paths.

use serde_json::Value;

/// One validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Location in the instance (`$.user.tags[2]`; `$` for the root).
    pub path: String,
    /// Human-readable message from `jsonschema`.
    pub message: String,
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Validate `instance` against `schema`. An empty list means valid. A schema
/// that cannot be compiled yields a single issue at the root.
pub fn validate(schema: &Value, instance: &Value) -> Vec<ValidationIssue> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => {
            return vec![ValidationIssue {
                path: "$".into(),
                message: format!("invalid schema: {e}"),
            }];
        }
    };
    validator
        .iter_errors(instance)
        .map(|e| ValidationIssue {
            path: dollar_path(&e.instance_path().to_string()),
            message: e.to_string(),
        })
        .collect()
}

/// Convert a JSON Pointer (`/user/tags/2`) into `$.user.tags[2]`.
fn dollar_path(pointer: &str) -> String {
    let mut out = String::from("$");
    for segment in pointer.split('/').skip(1) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        if segment.chars().all(|c| c.is_ascii_digit()) && !segment.is_empty() {
            out.push('[');
            out.push_str(&segment);
            out.push(']');
        } else {
            out.push('.');
            out.push_str(&segment);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_paths_and_messages() {
        let schema = json!({
            "type": "object",
            "properties": {
                "user": {"type": "object", "properties": {"tags": {"type": "array", "items": {"type": "string"}}}},
                "n": {"type": "integer", "minimum": 3}
            },
            "required": ["n"]
        });
        let issues = validate(&schema, &json!({"user": {"tags": ["a", 1]}, "n": 1}));
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"$.user.tags[1]"), "{paths:?}");
        assert!(paths.contains(&"$.n"), "{paths:?}");
        assert!(validate(&schema, &json!({"n": 3})).is_empty());
        let missing = validate(&schema, &json!({}));
        assert_eq!(missing[0].path, "$");
        assert!(missing[0].message.contains("required"));
    }

    #[test]
    fn bad_schema_is_reported_not_panicked() {
        let issues = validate(&json!({"type": "nope"}), &json!(1));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.starts_with("invalid schema"));
    }

    #[test]
    fn pointer_conversion() {
        assert_eq!(dollar_path(""), "$");
        assert_eq!(dollar_path("/a/0/b~1c"), "$.a[0].b/c");
    }
}
