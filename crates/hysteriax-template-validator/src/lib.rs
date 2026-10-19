//! Ahead-of-time validators for the fixed subscription schemas.
//! This crate has no server dependency so ordinary backend changes reuse its compiled code.
use serde_json::Value;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/validators.rs"));
}

#[derive(Clone, Copy, Debug)]
pub enum Format {
    Mihomo,
    Singbox,
}

/// Collect only field paths; configuration values and credentials never enter error text here.
pub fn error_paths(format: Format, value: &Value) -> Vec<String> {
    match format {
        Format::Mihomo => generated::Mihomo::iter_errors(value)
            .map(|error| error.instance_path().to_string())
            .collect(),
        Format::Singbox => generated::Singbox::iter_errors(value)
            .map(|error| error.instance_path().to_string())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_validators_keep_values_and_field_paths() {
        for (format, valid, invalid, path) in [
            (
                Format::Mihomo,
                json!({"mixed-port":0,"future-option":null}),
                json!({"mixed-port":"invalid"}),
                "/mixed-port",
            ),
            (
                Format::Singbox,
                json!({"log":{"level":"info"},"future-option":false}),
                json!({"log":{"level":123}}),
                "/log/level",
            ),
        ] {
            let before = valid.clone();
            assert!(error_paths(format, &valid).is_empty());
            assert_eq!(valid, before);
            assert!(error_paths(format, &invalid).iter().any(|p| p == path));
        }
    }
}
