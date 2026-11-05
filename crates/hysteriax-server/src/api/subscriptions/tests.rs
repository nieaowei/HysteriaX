use serde_json::json;

use super::{build_node, extract_ech_config_list, subscription_realm_options};

#[test]
fn serializes_special_node_names_ipv6_and_port_hopping_as_yaml_strings() {
    let proxy = build_node(
        "東京: edge",
        "7ed8a6e1-1234-5678-9abc-def012345678",
        "2001:db8::1",
        443,
        Some("443,445-446".to_owned()),
        Some("edge.example".to_owned()),
        false,
        "pass:word/with?symbols",
        None,
        None,
        None,
        None,
        &json!({}),
    );
    let yaml = serde_yaml::to_string(&proxy.to_mihomo()).unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(value["name"].as_str(), Some("東京: edge:7ed8a6e1"));
    assert_eq!(value["server"].as_str(), Some("2001:db8::1"));
    assert_eq!(value["password"].as_str(), Some("pass:word/with?symbols"));
    assert_eq!(value["ports"].as_str(), Some("443,445-446"));
    assert!(value.get("handshake-timeout").is_none());
}

#[test]
fn extracts_the_public_ech_config_list_from_the_uploaded_key_file() {
    let pem = "-----BEGIN ECH KEYS-----\nprivate-key\n-----END ECH KEYS-----\n-----BEGIN ECH CONFIGS-----\nQUJD\n-----END ECH CONFIGS-----\n";
    assert_eq!(extract_ech_config_list(pem).unwrap(), "QUJD");
    assert!(extract_ech_config_list("-----BEGIN ECH KEYS-----\nprivate\n").is_err());
    assert!(
        extract_ech_config_list(
            "-----BEGIN ECH CONFIGS-----\nnot-base64!\n-----END ECH CONFIGS-----"
        )
        .is_err()
    );
}

#[test]
fn maps_realm_registration_secrets_to_mihomo_realm_options() {
    let config = json!({
        "realm": {
            "connection": {
                "serverURL": "https://rendezvous.example",
                "token": "realm-token",
                "realmID": "node-realm-1"
            },
            "insecure": true,
            "stunServers": ["stun.example:3478"]
        }
    });
    let realm = subscription_realm_options(&config).unwrap().unwrap();
    let yaml = serde_yaml::to_string(&realm.to_mihomo()).unwrap();
    assert!(yaml.contains("server-url: https://rendezvous.example"));
    assert!(yaml.contains("token: realm-token"));
    assert!(yaml.contains("realm-id: node-realm-1"));
    assert!(yaml.contains("stun-servers:"));
    assert!(yaml.contains("skip-cert-verify: true"));

    let proxy = build_node(
        "Realm node",
        "node-realm-id",
        "node.example",
        443,
        None,
        Some("node.example".to_owned()),
        false,
        "user-pass",
        None,
        None,
        None,
        Some(realm),
        &config,
    );
    let proxy_yaml = serde_yaml::to_string(&proxy.to_mihomo()).unwrap();
    let proxy_value: serde_yaml::Value = serde_yaml::from_str(&proxy_yaml).unwrap();
    assert_eq!(proxy_value["handshake-timeout"].as_u64(), Some(30));
}
