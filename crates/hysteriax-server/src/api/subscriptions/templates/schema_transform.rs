//! Shared build-time preparation of the fixed validation schemas.
use serde_json::{Value, json};

pub fn prepare(mut schema: Value) -> Value {
    // Unknown future fields warn. A field known to another union variant is still a structural error.
    fn fields(root: &Value, s: &Value, depth: usize) -> std::collections::BTreeSet<String> {
        if depth > 50 {
            return Default::default();
        }
        if let Some(target) = s["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|r| root.pointer(r))
        {
            return fields(root, target, depth + 1);
        }
        let mut result: std::collections::BTreeSet<_> = s["properties"]
            .as_object()
            .into_iter()
            .flat_map(|p| p.keys().cloned())
            .collect();
        for union in ["allOf", "anyOf", "oneOf"] {
            for part in s[union].as_array().into_iter().flatten() {
                result.extend(fields(root, part, depth + 1));
            }
        }
        result
    }
    fn walk(root: &Value, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["oneOf", "anyOf"] {
                    if let Some(parts) = map.get_mut(key).and_then(Value::as_array_mut) {
                        let definitions: Vec<_> =
                            parts.iter().map(|part| fields(root, part, 0)).collect();
                        if definitions.iter().all(|p| !p.is_empty()) {
                            let known: std::collections::BTreeSet<_> =
                                definitions.iter().flat_map(|p| p.iter().cloned()).collect();
                            for (part, own) in parts.iter_mut().zip(definitions) {
                                if let Some(branch) = part.as_object_mut() {
                                    let properties = branch
                                        .entry("properties")
                                        .or_insert(json!({}))
                                        .as_object_mut()
                                        .unwrap();
                                    for field in known.difference(&own) {
                                        properties.insert(field.clone(), Value::Bool(false));
                                    }
                                }
                            }
                        }
                    }
                }
                for key in ["additionalProperties", "unevaluatedProperties"] {
                    if map.get(key) == Some(&Value::Bool(false)) {
                        map.insert(key.into(), Value::Bool(true));
                    }
                }
                for value in map.values_mut() {
                    walk(root, value);
                }
            }
            Value::Array(items) => {
                for value in items {
                    walk(root, value);
                }
            }
            _ => (),
        }
    }
    let root = schema.clone();
    walk(&root, &mut schema);
    schema
}
