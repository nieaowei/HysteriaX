use super::*;
#[test]
fn compiles_san_acme_before_mtls_without_mutating_public_config() {
    let public = json!({"acme":{"domains":["a.example","b.example"]},"mtls":{"mode":"off"}});
    let c = compile(&public).unwrap();
    assert!(c.get("acme").is_none());
    assert!(
        c["tls"]["cert"]
            .as_str()
            .unwrap()
            .ends_with("/current/cert.pem")
    );
    assert!(public.get("acme").is_some());
    assert_eq!(c, compile(&public).unwrap());
    assert_ne!(
        c,
        compile(&json!({"acme":{"domains":["c.example"]}})).unwrap()
    );
    assert!(compile(&json!({"acme":{"domains":["a.example"]},"tls":{}})).is_err());
}
#[test]
fn preserves_relative_directory_semantics_and_rejects_parent_traversal() {
    assert_eq!(
        component_config(&json!({"acme":{"domains":["a.example"],"dir":"custom"}}))
            .unwrap()
            .unwrap()["acme"]["dir"],
        "/var/lib/hysteriax/custom"
    );
    assert!(component_config(&json!({"acme":{"domains":["a.example"],"dir":"../other"}})).is_err());
}
