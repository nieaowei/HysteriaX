use std::collections::BTreeMap;

use serde_json::json;

use super::{validate_resource_kinds, validate_text_resource};

#[test]
fn validates_resource_types_before_deployment() {
    let kinds = BTreeMap::from([
        ("cert-id".to_owned(), "certificate".to_owned()),
        ("key-id".to_owned(), "private_key".to_owned()),
        ("ech-id".to_owned(), "ech_key".to_owned()),
    ]);
    assert!(
        validate_resource_kinds(
            &json!({"tls": {"cert": "resource://cert-id", "key": "resource://key-id"}}),
            &kinds
        )
        .is_ok()
    );
    assert!(
        validate_resource_kinds(&json!({"tls": {"cert": "resource://key-id"}}), &kinds).is_err()
    );
    assert!(
        validate_resource_kinds(&json!({"tls": {"clientCA": "resource://cert-id"}}), &kinds)
            .is_ok()
    );
    assert!(
        validate_resource_kinds(&json!({"tls": {"clientCA": "resource://key-id"}}), &kinds)
            .is_err()
    );
    assert!(
        validate_resource_kinds(&json!({"ech": {"keyPath": "resource://ech-id"}}), &kinds).is_ok()
    );
    assert!(
        validate_resource_kinds(&json!({"ech": {"keyPath": "resource://key-id"}}), &kinds).is_err()
    );
}

#[test]
fn ech_key_resources_require_both_base64_pem_blocks() {
    let content = b"-----BEGIN ECH KEYS-----\nS0VZ\n-----END ECH KEYS-----\n-----BEGIN ECH CONFIGS-----\nQ09ORklH\n-----END ECH CONFIGS-----\n";
    assert!(validate_text_resource("ech_key", content).is_ok());
    assert!(
        validate_text_resource(
            "ech_key",
            b"-----BEGIN ECH KEYS-----\nS0VZ\n-----END ECH KEYS-----"
        )
        .is_err()
    );
    assert!(validate_text_resource("ech_key", b"-----BEGIN ECH KEYS-----\ninvalid!\n-----END ECH KEYS-----\n-----BEGIN ECH CONFIGS-----\nQ09ORklH\n-----END ECH CONFIGS-----").is_err());
}

#[test]
fn server_certificate_and_key_resources_must_parse_as_pem() {
    assert!(
        validate_text_resource(
            "certificate",
            b"-----BEGIN CERTIFICATE-----\n%%%\n-----END CERTIFICATE-----"
        )
        .is_err()
    );
    assert!(
        validate_text_resource(
            "private_key",
            b"-----BEGIN PRIVATE KEY-----\n%%%\n-----END PRIVATE KEY-----"
        )
        .is_err()
    );
    assert!(
        validate_text_resource(
            "certificate",
            b"-----BEGIN CERTIFICATE-----\nS0VZ\n-----END CERTIFICATE-----"
        )
        .is_err()
    );
    assert!(
        validate_text_resource(
            "private_key",
            b"-----BEGIN PRIVATE KEY-----\nS0VZ\n-----END PRIVATE KEY-----"
        )
        .is_err()
    );
}
