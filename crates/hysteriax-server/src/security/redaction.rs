use serde_json::Value;
use url::Url;

pub fn redact_secret_values(message: &mut String, values: impl IntoIterator<Item = String>) {
    let mut values = values
        .into_iter()
        .filter(|value| !value.is_empty() && message.contains(value))
        .collect::<Vec<_>>();
    values.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    let mut marker = "[HYSTERIAX_SECRET_REDACTION_MARKER]".to_owned();
    while message.contains(&marker) || values.iter().any(|value| value.contains(&marker)) {
        marker.push('_');
    }
    for value in values {
        *message = message.replace(&value, &marker);
    }
    *message = message.replace(&marker, "[redacted]");
}

pub fn redact_config_secrets(message: &mut String, config: &Value) {
    let mut values = Vec::new();
    collect_config_secrets(config, None, None, false, &mut values);
    redact_secret_values(message, values);
}

fn collect_config_secrets(
    value: &Value,
    key: Option<&str>,
    parent_key: Option<&str>,
    secret_context: bool,
    values: &mut Vec<String>,
) {
    match value {
        Value::String(value) => {
            if secret_context || is_sensitive_key(key, parent_key) {
                values.push(value.clone());
            }
            if matches!(key, Some("url" | "addr" | "serverURL")) {
                collect_url_credentials(value, values);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_config_secrets(item, key, parent_key, secret_context, values);
            }
        }
        Value::Object(object) => {
            for (child_key, child_value) in object {
                let sensitive = secret_context
                    || is_sensitive_key(Some(child_key), key)
                    || (child_key == "config" && key == Some("dns"));
                collect_config_secrets(child_value, Some(child_key), key, sensitive, values);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_sensitive_key(key: Option<&str>, parent_key: Option<&str>) -> bool {
    let Some(key) = key else {
        return false;
    };
    let normalized = key
        .chars()
        .filter(|character| *character != '-' && *character != '_')
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.ends_with("password")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.contains("privatekey")
        || normalized.ends_with("credential")
        || normalized.ends_with("credentials")
        || normalized == "authorization"
        || (normalized == "username" && matches!(parent_key, Some("socks5" | "http")))
}

fn collect_url_credentials(value: &str, values: &mut Vec<String>) {
    let Ok(url) = Url::parse(value) else {
        return;
    };
    if url.username().is_empty() && url.password().is_none() {
        return;
    }
    values.push(value.to_owned());
    if !url.username().is_empty() {
        values.push(url.username().to_owned());
    }
    if let Some(password) = url.password() {
        values.push(password.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::redact_config_secrets;

    #[test]
    fn redacts_nested_passwords_provider_credentials_and_url_userinfo() {
        let config = json!({
            "server_config": {
                "obfs": {"gecko": {"password": "gecko-password"}},
                "outbounds": [
                    {"socks5": {"username": "socks-user", "password": "socks-password"}},
                    {"http": {"url": "https://url-user:url-password@proxy.example.test"}}
                ],
                "masquerade": {"proxy": {"url": "https://proxy-user:proxy-password@site.example.test"}},
                "acme": {"dns": {"config": {"api_token": "provider-token", "account": "provider-account"}}}
            }
        });
        let secrets = [
            "gecko-password",
            "socks-user",
            "socks-password",
            "url-user",
            "url-password",
            "proxy-user",
            "proxy-password",
            "provider-token",
            "provider-account",
        ];
        let mut message = serde_json::to_string(&config).unwrap();

        redact_config_secrets(&mut message, &config);

        for secret in secrets {
            assert!(
                !message.contains(secret),
                "secret remained in message: {secret}"
            );
        }
        assert!(message.contains("[redacted]"));
    }

    #[test]
    fn redacting_short_secrets_does_not_modify_redaction_markers() {
        let mut message = "short=x long=example-secret".to_owned();

        super::redact_secret_values(&mut message, ["x".to_owned(), "example-secret".to_owned()]);

        assert_eq!(message, "short=[redacted] long=[redacted]");
    }
}
