use super::*;
use rcgen::{CustomExtension, GeneralSubtree, NameConstraints};

fn parse(material: &Material) -> Vec<u8> {
    rustls_pemfile::certs(&mut Cursor::new(material.certificate.as_bytes()))
        .next()
        .unwrap()
        .unwrap()
        .to_vec()
}

fn authority_params(now: DateTime<Utc>) -> CertificateParams {
    let mut params = parameters("import-test", now, now + Duration::days(365)).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    params
}

fn material(params: &CertificateParams, key: &KeyPair) -> Material {
    Material {
        certificate: params.self_signed(key).unwrap().pem(),
        private_key: key.serialize_pem(),
    }
}

#[test]
fn generated_ca_and_leaves_have_correct_purposes_lifetimes_and_independent_keys() {
    let now = DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap();
    let ca = Authority::generate(now).unwrap();
    assert_eq!((ca.expires_at - now).num_days(), 3650);
    ca.validate_new_binding(now).unwrap();
    let id = uuid::Uuid::new_v4();
    let first = ca.sign_user(&id, Policy::default(), now).unwrap();
    let second = ca.sign_user(&id, Policy::default(), now).unwrap();
    let probe = ca.sign_probe(&id, now).unwrap();
    assert_ne!(first.material.private_key, second.material.private_key);
    assert_eq!(first.expires_at, now + Duration::days(180));
    assert_eq!(
        first.next_action_at,
        first.expires_at - Duration::hours(720)
    );
    assert_eq!(probe.expires_at, now + Duration::days(7));
    assert_eq!(probe.next_action_at, probe.expires_at - Duration::hours(48));
    let ca_der = parse(&ca.material);
    let (_, issuer) = x509_parser::parse_x509_certificate(&ca_der).unwrap();
    let first_der = parse(&first.material);
    let second_der = parse(&second.material);
    let (_, first_cert) = x509_parser::parse_x509_certificate(&first_der).unwrap();
    let (_, second_cert) = x509_parser::parse_x509_certificate(&second_der).unwrap();
    first_cert
        .verify_signature(Some(issuer.public_key()))
        .unwrap();
    assert!(!first_cert.is_ca());
    assert!(!first_cert.basic_constraints().unwrap().unwrap().value.ca);
    assert!(
        first_cert
            .key_usage()
            .unwrap()
            .unwrap()
            .value
            .digital_signature()
    );
    assert!(
        !first_cert
            .key_usage()
            .unwrap()
            .unwrap()
            .value
            .key_cert_sign()
    );
    let eku = first_cert.extended_key_usage().unwrap().unwrap();
    assert!(eku.value.client_auth);
    assert!(!eku.value.server_auth);
    assert_ne!(first_cert.raw_serial(), second_cert.raw_serial());
    assert_eq!(first_cert.subject(), second_cert.subject());
    assert_eq!(
        first_cert.validity().not_before.timestamp(),
        (now - Duration::minutes(5)).timestamp()
    );
    let metadata = ca.metadata.to_string();
    assert!(!metadata.contains("PRIVATE KEY"));
}

#[test]
fn policy_bounds_and_ca_expiry_do_not_create_renewal_loops() {
    let now = DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params = authority_params(now);
    params.not_after = offset(now + Duration::days(10)).unwrap();
    let ca = Authority::import(material(&params, &key), now).unwrap();
    assert!(ca.validate_new_binding(now).is_err());
    assert_eq!(
        ca.sign_user(&uuid::Uuid::new_v4(), Policy::default(), now)
            .unwrap()
            .expires_at,
        ca.expires_at - Duration::minutes(5)
    );
    let policy = Policy {
        client_validity_days: 30,
        renew_before_hours: 24,
    };
    let issued = ca.sign_user(&uuid::Uuid::new_v4(), policy, now).unwrap();
    assert_eq!(issued.expires_at, ca.expires_at - Duration::minutes(5));
    for policy in [
        Policy {
            client_validity_days: 0,
            renew_before_hours: 1,
        },
        Policy {
            client_validity_days: 366,
            renew_before_hours: 1,
        },
        Policy {
            client_validity_days: 1,
            renew_before_hours: 24,
        },
        Policy {
            client_validity_days: 1,
            renew_before_hours: 0,
        },
    ] {
        assert!(policy.validate().is_err());
    }
    Policy {
        client_validity_days: 1,
        renew_before_hours: 23,
    }
    .validate()
    .unwrap();
}

#[test]
fn import_rejects_mismatch_non_ca_expired_future_wrong_usage_and_unsupported_constraints() {
    let now = DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap();
    let key = KeyPair::generate().unwrap();
    let params = authority_params(now);
    let mut wrong_key = material(&params, &key);
    wrong_key.private_key = KeyPair::generate().unwrap().serialize_pem();
    assert!(Authority::import(wrong_key, now).is_err());
    let mut mixed = material(&params, &key);
    mixed.certificate.push_str(&mixed.private_key);
    assert!(Authority::import(mixed, now).is_err());
    let mut mixed = material(&params, &key);
    mixed.certificate.push_str("unexpected secret text");
    assert!(Authority::import(mixed, now).is_err());
    let mut invalid = Vec::new();
    let mut p = params.clone();
    p.is_ca = IsCa::ExplicitNoCa;
    invalid.push(p);
    let mut p = params.clone();
    p.not_after = offset(now - Duration::seconds(1)).unwrap();
    invalid.push(p);
    let mut p = params.clone();
    p.not_before = offset(now + Duration::hours(1)).unwrap();
    invalid.push(p);
    let mut p = params.clone();
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    invalid.push(p);
    let mut p = params.clone();
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    invalid.push(p);
    let mut p = params.clone();
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![GeneralSubtree::DnsName("example.test".into())],
        excluded_subtrees: vec![],
    });
    invalid.push(p);
    let mut p = params.clone();
    let mut ext = CustomExtension::from_oid_content(&[1, 2, 3, 4], vec![5, 0]);
    ext.set_criticality(true);
    p.custom_extensions.push(ext);
    invalid.push(p);
    for (index, params) in invalid.into_iter().enumerate() {
        assert!(
            Authority::import(material(&params, &key), now).is_err(),
            "invalid fixture {index}"
        );
    }
    assert!(
        Authority::from_payload(&json!({"certificate":"garbage", "private_key":"bad"}), now)
            .is_err()
    );
    assert!(
        Authority::from_payload(
            &json!({"certificate":"", "private_key":"", "ignored":true}),
            now
        )
        .is_err()
    );
}

#[test]
fn intermediate_requires_valid_complete_chain_and_only_selected_issuer_is_trusted() {
    let now = DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap();
    let root_key = KeyPair::generate().unwrap();
    let mut root_params = authority_params(now);
    root_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(1));
    let root_cert = root_params.self_signed(&root_key).unwrap();
    let issuer = Issuer::from_params(&root_params, &root_key);
    let child_key = KeyPair::generate().unwrap();
    let mut child_params = authority_params(now);
    child_params
        .distinguished_name
        .push(DnType::CommonName, "intermediate");
    child_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    let child_cert = child_params.signed_by(&child_key, &issuer).unwrap();
    assert!(
        Authority::import(
            Material {
                certificate: child_cert.pem(),
                private_key: child_key.serialize_pem()
            },
            now
        )
        .is_err()
    );
    let ca = Authority::import(
        Material {
            certificate: format!("{}{}", child_cert.pem(), root_cert.pem()),
            private_key: child_key.serialize_pem(),
        },
        now,
    )
    .unwrap();
    let trust = ca.trust_anchor().unwrap();
    let anchors = rustls_pemfile::certs(&mut Cursor::new(trust.as_bytes()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].as_ref(), child_cert.der().as_ref());
    let leaf = ca
        .sign_user(&uuid::Uuid::new_v4(), Policy::default(), now)
        .unwrap();
    let child_der = child_cert.der();
    let (_, child) = x509_parser::parse_x509_certificate(child_der).unwrap();
    let leaf_der = parse(&leaf.material);
    let (_, parsed_leaf) = x509_parser::parse_x509_certificate(&leaf_der).unwrap();
    parsed_leaf
        .verify_signature(Some(child.public_key()))
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(anchors[0].clone()).unwrap();
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        std::sync::Arc::new(roots),
        std::sync::Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .unwrap();
    let at = rustls::pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        now.timestamp() as u64,
    ));
    verifier
        .verify_client_cert(&leaf_der.clone().into(), &[], at)
        .unwrap();
    let root_authority = Authority::import(
        Material {
            certificate: root_cert.pem(),
            private_key: root_key.serialize_pem(),
        },
        now,
    )
    .unwrap();
    let sibling = root_authority
        .sign_user(&uuid::Uuid::new_v4(), Policy::default(), now)
        .unwrap();
    assert!(
        verifier
            .verify_client_cert(&parse(&sibling.material).into(), &[], at)
            .is_err()
    );

    root_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    let root_cert = root_params.self_signed(&root_key).unwrap();
    assert!(
        Authority::import(
            Material {
                certificate: format!("{}{}", child_cert.pem(), root_cert.pem()),
                private_key: child_key.serialize_pem()
            },
            now
        )
        .is_err()
    );
    let wrong_root_key = KeyPair::generate().unwrap();
    let wrong_root = root_params.self_signed(&wrong_root_key).unwrap();
    assert!(
        Authority::import(
            Material {
                certificate: format!("{}{}", child_cert.pem(), wrong_root.pem()),
                private_key: child_key.serialize_pem()
            },
            now
        )
        .is_err()
    );
}
