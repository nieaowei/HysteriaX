//! Normalize native source once on the server. Comments, anchors, and layout are not storage identities.
use crate::error::ApiError;
use serde_json::Value;

pub fn yaml(source: &str) -> Result<Value, ApiError> {
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(source).map_err(|e| ApiError::bad_request(e.to_string()))?;
    value
        .apply_merge()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    fn tagged(value: &serde_yaml::Value) -> bool {
        match value {
            serde_yaml::Value::Tagged(_) => true,
            serde_yaml::Value::Sequence(items) => items.iter().any(tagged),
            serde_yaml::Value::Mapping(map) => map.iter().any(|(k, v)| tagged(k) || tagged(v)),
            _ => false,
        }
    }
    if tagged(&value) {
        return Err(ApiError::bad_request(
            "custom YAML tags cannot be represented in a native JSON-compatible configuration document",
        ));
    }
    fn finite(value: &serde_yaml::Value) -> bool {
        match value {
            serde_yaml::Value::Number(n) => n.as_f64().is_none_or(f64::is_finite),
            serde_yaml::Value::Sequence(items) => items.iter().all(finite),
            serde_yaml::Value::Mapping(map) => map
                .iter()
                .all(|(k, v)| matches!(k, serde_yaml::Value::String(_)) && finite(v)),
            _ => true,
        }
    }
    if !finite(&value) {
        return Err(ApiError::bad_request(
            "YAML documents require finite JSON-compatible numbers and string mapping keys; quote numeric names",
        ));
    }
    serde_json::to_value(value).map_err(|e| ApiError::bad_request(e.to_string()))
}
pub fn jsonc(source: &str) -> Result<Value, ApiError> {
    let source = source.trim_start_matches('\u{feff}');
    let mut bytes = source.as_bytes().to_vec();
    let mut i = 0;
    let mut quoted = false;
    let mut escaped = false;
    while i < bytes.len() {
        if quoted {
            if escaped {
                escaped = false;
            } else if bytes[i] == b'\\' {
                escaped = true;
            } else if bytes[i] == b'"' {
                quoted = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            quoted = true;
            i += 1;
            continue;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && !matches!(bytes[i], b'\n' | b'\r') {
                bytes[i] = b' ';
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let begin = i;
            bytes[i] = b' ';
            bytes[i + 1] = b' ';
            i += 2;
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    bytes[i] = b' ';
                    bytes[i + 1] = b' ';
                    i += 2;
                    closed = true;
                    break;
                }
                if !matches!(bytes[i], b'\n' | b'\r') {
                    bytes[i] = b' ';
                }
                i += 1;
            }
            if !closed {
                let prefix = &source[..begin];
                let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
                let column = prefix
                    .rsplit_once('\n')
                    .map_or(prefix.len() + 1, |(_, last)| last.len() + 1);
                return Err(ApiError::bad_request(format!(
                    "unterminated JSON block comment at line {line} column {column}"
                )));
            }
            continue;
        }
        i += 1;
    }
    i = 0;
    quoted = false;
    escaped = false;
    while i < bytes.len() {
        if quoted {
            if escaped {
                escaped = false;
            } else if bytes[i] == b'\\' {
                escaped = true;
            } else if bytes[i] == b'"' {
                quoted = false;
            }
        } else if bytes[i] == b'"' {
            quoted = true;
        } else if bytes[i] == b',' {
            let next = bytes[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
            if matches!(next, Some(b'}' | b']')) {
                bytes[i] = b' ';
            }
        }
        i += 1;
    }
    serde_json::from_slice(&bytes).map_err(|e| ApiError::bad_request(e.to_string()))
}
