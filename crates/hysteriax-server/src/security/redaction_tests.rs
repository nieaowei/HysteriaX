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
