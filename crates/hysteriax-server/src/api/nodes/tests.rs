use serde_json::json;

use super::{
    normalize_proxy_probe_url, validate_hop_public_port, validate_listen_addr,
    validate_listener_features,
};

#[test]
fn validates_optional_http_probe_targets_without_embedded_credentials() {
    assert_eq!(normalize_proxy_probe_url(None).unwrap(), None);
    assert_eq!(normalize_proxy_probe_url(Some("  ")).unwrap(), None);
    assert_eq!(
        normalize_proxy_probe_url(Some("http://status.example.test/health")).unwrap(),
        Some("http://status.example.test/health".to_owned())
    );
    assert!(normalize_proxy_probe_url(Some("https://status.example.test/health")).is_err());
    assert!(normalize_proxy_probe_url(Some("http://user:pass@status.example.test")).is_err());
    assert!(
        normalize_proxy_probe_url(Some("http://status.example.test/health?token=secret")).is_err()
    );
}

#[test]
fn validates_single_and_hopping_listener_port_lists() {
    assert_eq!(validate_listen_addr(":443").unwrap(), (443, false));
    assert_eq!(validate_listen_addr("0.0.0.0:443").unwrap(), (443, false));
    assert_eq!(validate_listen_addr(":443-445").unwrap(), (443, true));
    assert_eq!(validate_listen_addr(":443,445-446").unwrap(), (443, true));
    assert!(validate_listen_addr(":0").is_err());
    assert!(validate_listen_addr(":445-443").is_err());
    assert!(validate_listen_addr(":443,443-445").is_err());
    assert!(validate_listen_addr(":443, 445").is_err());
    assert!(validate_hop_public_port(443, 443, true).is_ok());
    assert!(validate_hop_public_port(8443, 443, true).is_err());
}

#[test]
fn realm_requires_a_single_listener_port() {
    let realm = json!({
        "realm": {"connection": {
            "serverURL": "https://rendezvous.example",
            "token": "realm-token",
            "realmID": "realm-test"
        }}
    });
    assert!(validate_listener_features(&realm, ":443").is_ok());
    assert!(validate_listener_features(&realm, ":443-445").is_err());
    assert!(validate_listener_features(&realm, ":443,445-446").is_err());
}
