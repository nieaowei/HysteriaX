//! Compact database representation; wire responses keep their existing shape.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const FORMAT: &str = "sha256-v1";

// JSONB does not preserve object key order. Array order is meaningful; callers
// already sort ID sets and connection pairs before constructing snapshots.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect(),
            )
        }
        Value::Array(array) => Value::Array(array.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

pub(super) fn digest(value: &Value) -> Value {
    let bytes = serde_json::to_vec(&canonical(value)).expect("JSON value serializes");
    json!({"format": FORMAT, "digest": hex::encode(Sha256::digest(bytes))})
}

pub(super) fn matches(stored: &Value, current: &Value) -> bool {
    stored == &digest(current)
}

pub(super) fn compact_result(result: &Value) -> Value {
    let diff = json!({
        "additions_count": result["additions_count"],
        "removals_count": result["removals_count"],
        "additions_user_count": result["additions_user_count"],
        "removals_user_count": result["removals_user_count"],
        "additions": result["additions"],
        "removals": result["removals"],
    });
    json!({
        "storage_format": FORMAT,
        "diff_digest": digest(&diff),
        "additions_count": result["additions_count"],
        "removals_count": result["removals_count"],
        "additions_user_count": result["additions_user_count"],
        "removals_user_count": result["removals_user_count"],
        "automatic_issue_count": result["automatic_issue_count"],
        "pending_issue_count": result["pending_issue_count"],
        "mtls_plan": result["mtls_plan"],
    })
}

pub(super) fn compact_diff_matches(stored: &Value, diff: &Value) -> bool {
    stored["storage_format"] == FORMAT
        && stored["diff_digest"] == digest(diff)
        && stored["additions_count"] == diff["additions_count"]
        && stored["removals_count"] == diff["removals_count"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_across_jsonb_round_trip_and_detects_same_size_changes() {
        let value = json!({"users":[{"id":"alice","revision":1}],"nodes":["node-a"]});
        let round_trip: Value = serde_json::from_str(&value.to_string()).unwrap();
        let stored = digest(&value);
        assert!(matches(&stored, &round_trip));
        assert!(!matches(&value, &round_trip));
        assert!(!matches(
            &stored,
            &json!({"users":[{"id":"bob","revision":1}],"nodes":["node-a"]})
        ));
        assert!(!matches(
            &json!({"format":"unknown","digest":"anything"}),
            &value
        ));
    }

    #[test]
    fn compact_diff_rejects_different_connections_with_identical_counts() {
        let full = super::super::diff_json(&[("alice".into(), "node-a".into())], &[]);
        let stored = compact_result(&full);
        assert!(compact_diff_matches(&stored, &full));
        assert!(!compact_diff_matches(&full, &full));
        let different = super::super::diff_json(&[("alice".into(), "node-b".into())], &[]);
        assert!(!compact_diff_matches(&stored, &different));
        let mut unknown = stored;
        unknown["storage_format"] = json!("unknown");
        assert!(!compact_diff_matches(&unknown, &full));
    }
}
