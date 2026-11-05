use super::*;
use crate::api::subscriptions::build_node;

fn render_singbox_template(
    nodes: &[&SubscriptionNode],
    client: &ClientInfo,
) -> Result<String, ApiError> {
    let client = ClientInfo {
        platform: if client.platform == Platform::Unknown {
            Platform::Linux
        } else {
            client.platform
        },
        ..*client
    };
    let output = super::super::templates::render_nodes_for_test(
        "singbox",
        &nodes.iter().map(|n| (*n).clone()).collect::<Vec<_>>(),
        &client,
    )?;
    super::super::templates::document::serialize("singbox", &output["config"])
}
fn render_mihomo_template(
    nodes: &[&SubscriptionNode],
    platform: Platform,
) -> Result<String, ApiError> {
    let client = ClientInfo {
        platform: if platform == Platform::Unknown {
            Platform::Linux
        } else {
            platform
        },
        ..ClientInfo::default()
    };
    let output = super::super::templates::render_nodes_for_test(
        "mihomo",
        &nodes.iter().map(|n| (*n).clone()).collect::<Vec<_>>(),
        &client,
    )?;
    super::super::templates::document::serialize("mihomo", &output["config"])
}
fn render_subscription(
    nodes: &[SubscriptionNode],
    format: SubscriptionFormat,
    client: &ClientInfo,
) -> Result<Response, ApiError> {
    if matches!(format, SubscriptionFormat::Uri | SubscriptionFormat::Base64) {
        return super::render_subscription(nodes, format, client);
    }
    let format = if format == SubscriptionFormat::Mihomo {
        "mihomo"
    } else {
        "singbox"
    };
    let client = ClientInfo {
        platform: if client.platform == Platform::Unknown {
            Platform::Linux
        } else {
            client.platform
        },
        ..*client
    };
    super::super::templates::response_from_seed_for_test(format, nodes, &client)
}
fn node() -> SubscriptionNode {
    build_node(
        "東京: #&",
        "abcd-1234",
        "2001:db8::1",
        443,
        None,
        Some("edge.example".into()),
        true,
        "pass:word/@?#% 空",
        None,
        None,
        None,
        None,
        &json!({}),
    )
}

#[test]
fn detects_client_families_versions_and_boundaries() {
    for ua in ["MIHOMO/v1.19.31", "Clash.Meta/1.19.31", "clashmeta 1.19.31"] {
        let c = ClientInfo::detect(ua);
        assert_eq!(c.format, Some(SubscriptionFormat::Mihomo));
        assert_eq!(c.version, Some(Version(1, 19, 31)));
    }
    for ua in ["sing-box/1.14.2", "SingBox v1.14.2"] {
        let c = ClientInfo::detect(ua);
        assert_eq!(c.format, Some(SubscriptionFormat::Singbox));
        assert_eq!(c.version, Some(Version(1, 14, 2)));
    }
    assert_eq!(ClientInfo::detect("v2rayNG/1.9.0").name, "v2rayng");
    for ua in ["Shadowrocket/2.2.0", "v2rayN/7.0", "v2rayNG/1.9"] {
        assert_eq!(
            ClientInfo::detect(ua).format,
            Some(SubscriptionFormat::Base64)
        );
    }
    for ua in [
        "",
        "curl/8.0",
        "Mozilla/5.0",
        "Clash/1.0",
        "notmihomo/1.19.31",
        "singboxfoo",
    ] {
        assert!(ClientInfo::detect(ua).format.is_none());
    }
    assert!(ClientInfo::detect("sing-box/bad 1.14.2").version.is_none());
    assert!(
        ClientInfo::detect("sing-box/1.14.0-alpha.1")
            .version
            .is_none()
    );
    assert!(
        ClientInfo::detect("sing-box/1.14.2")
            .at_least(SubscriptionFormat::Singbox, Version(1, 14, 0))
    );
    assert!(
        !ClientInfo::detect("v2rayN/7.0").at_least(SubscriptionFormat::Singbox, Version(1, 14, 0))
    );
    assert_eq!(
        ClientInfo::detect("Clash/1.0 mihomo/1.19.31").name,
        "mihomo"
    );
}

#[test]
fn explicit_formats_parse_independently_of_ua() {
    assert_eq!(requested_format(None).unwrap(), None);
    assert_eq!(requested_format(Some("format=auto")).unwrap(), None);
    for (text, target) in [
        ("mihomo", SubscriptionFormat::Mihomo),
        ("singbox", SubscriptionFormat::Singbox),
        ("base64", SubscriptionFormat::Base64),
        ("uri", SubscriptionFormat::Uri),
    ] {
        assert_eq!(
            requested_format(Some(&format!("format={text}"))).unwrap(),
            Some(target)
        );
    }
    for text in ["format=", "format=surge", "format=uri&format=singbox"] {
        assert!(requested_format(Some(text)).is_err());
    }
}

#[tokio::test]
async fn unknown_kernel_exports_advanced_nodes_with_all_required_material() {
    let mut advanced = node();
    advanced.certificate = Some("cert".into());
    advanced.private_key = Some("key".into());
    advanced.ech_config = Some("QUJD".into());
    advanced.obfs = Some("gecko".into());
    advanced.obfs_password = Some("obfs-secret".into());
    advanced.realm = Some(SubscriptionRealm {
        server_url: "https://realm.example".into(),
        token: "realm-token".into(),
        realm_id: "realm-id".into(),
        stun_servers: None,
        skip_cert_verify: None,
    });
    for ua in [
        "",
        "curl/8.0",
        "Mozilla/5.0",
        "sing-box",
        "sing-box/1.14.0-alpha.1",
        "v2rayN/7.0",
    ] {
        let client = ClientInfo::detect(ua);
        let response = render_subscription(
            std::slice::from_ref(&advanced),
            SubscriptionFormat::Singbox,
            &client,
        )
        .unwrap();
        assert_eq!(response.headers()["x-hysteriax-filtered-nodes"], "0");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        let outbound = json["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["type"] == "hysteria2")
            .unwrap();
        assert_eq!(outbound["tls"]["client_certificate"], json!(["cert"]));
        assert_eq!(outbound["tls"]["client_key"], json!(["key"]));
        assert_eq!(outbound["obfs"]["type"], "gecko");
        assert_eq!(outbound["realm"]["token"], "realm-token");
        assert!(
            outbound["tls"]["ech"]["config"][0]
                .as_str()
                .unwrap()
                .contains("QUJD")
        );
    }
    assert!(
        advanced
            .incompatible(
                SubscriptionFormat::Singbox,
                &ClientInfo::detect("sing-box/1.12.0")
            )
            .is_some()
    );
    assert!(
        advanced
            .incompatible(
                SubscriptionFormat::Singbox,
                &ClientInfo::detect("sing-box/1.14.2")
            )
            .is_none()
    );
}

#[test]
fn capabilities_never_strip_required_fields() {
    let mut n = node();
    n.certificate = Some("cert".into());
    n.private_key = Some("key".into());
    assert!(
        n.incompatible(SubscriptionFormat::Singbox, &ClientInfo::default())
            .is_none()
    );
    assert!(
        n.incompatible(
            SubscriptionFormat::Singbox,
            &ClientInfo::detect("sing-box/1.12.0")
        )
        .is_some()
    );
    assert!(
        n.incompatible(
            SubscriptionFormat::Singbox,
            &ClientInfo::detect("sing-box/1.14.0")
        )
        .is_none()
    );
    assert!(
        n.incompatible(SubscriptionFormat::Uri, &ClientInfo::default())
            .is_some()
    );
    n.ech_config = Some("QUJD".into());
    assert!(
        n.incompatible(
            SubscriptionFormat::Singbox,
            &ClientInfo::detect("sing-box/1.14.0")
        )
        .is_some()
    );
    assert!(
        n.incompatible(
            SubscriptionFormat::Singbox,
            &ClientInfo::detect("sing-box/1.14.2")
        )
        .is_none()
    );
    assert!(
        n.incompatible(SubscriptionFormat::Mihomo, &ClientInfo::default())
            .is_none()
    );
    assert!(
        n.incompatible(
            SubscriptionFormat::Mihomo,
            &ClientInfo::detect("mihomo/1.18.0")
        )
        .is_some()
    );
    let mut n = node();
    n.obfs = Some("gecko".into());
    n.obfs_password = Some("secret".into());
    assert!(
        n.incompatible(
            SubscriptionFormat::Singbox,
            &ClientInfo::detect("sing-box/1.13.0")
        )
        .is_some()
    );
    assert!(
        n.incompatible(
            SubscriptionFormat::Singbox,
            &ClientInfo::detect("sing-box/1.14.0")
        )
        .is_none()
    );
    assert!(
        n.incompatible(
            SubscriptionFormat::Base64,
            &ClientInfo::detect("Shadowrocket/2.2.0")
        )
        .is_some()
    );
    n.obfs = None;
    n.ports = Some("443,445-446".into());
    assert!(
        n.incompatible(SubscriptionFormat::Uri, &ClientInfo::default())
            .is_some()
    );
    assert!(
        n.incompatible(SubscriptionFormat::Singbox, &ClientInfo::default())
            .is_none()
    );
    assert!(
        n.incompatible(
            SubscriptionFormat::Singbox,
            &ClientInfo::detect("sing-box/1.10.0")
        )
        .is_some()
    );
}

#[test]
fn singbox_maps_fields_without_conflicting_endpoint_fields() {
    let mut n = node();
    n.ports = Some("443,445-446".into());
    n.hop_interval = Some(30);
    n.certificate = Some("cert\nchain\n".into());
    n.private_key = Some("key\n".into());
    n.ech_config = Some("QUJD".into());
    n.up = Some("100 Mbps".into());
    n.down = Some("200000 kbps".into());
    n.obfs = Some("gecko".into());
    n.obfs_password = Some("secret".into());
    n.obfs_min_packet_size = Some(512);
    n.obfs_max_packet_size = Some(1200);
    let v = n.singbox();
    assert_eq!(v["server_ports"], json!(["443:443", "445:446"]));
    assert!(v.get("server_port").is_none());
    assert_eq!(v["hop_interval"], "30s");
    assert_eq!(v["tls"]["client_certificate"], json!(["cert", "chain"]));
    assert_eq!(v["tls"]["client_key"], json!(["key"]));
    assert_eq!(v["disable_chrome_parrot"], json!(true));
    assert!(
        v["tls"]["ech"]["config"][0]
            .as_str()
            .unwrap()
            .contains("BEGIN ECH CONFIGS")
    );
    assert_eq!(v["up_mbps"], 100);
    assert_eq!(v["down_mbps"], 200);
    assert_eq!(v["obfs"]["min_packet_size"], 512);
    n.realm = Some(SubscriptionRealm {
        server_url: "https://realm.example".into(),
        token: "token".into(),
        realm_id: "realm-id".into(),
        stun_servers: Some(vec!["stun.example:3478".into()]),
        skip_cert_verify: Some(true),
    });
    let v = n.singbox();
    for key in ["server", "server_port", "server_ports", "hop_interval"] {
        assert!(v.get(key).is_none());
    }
    assert_eq!(v["realm"]["realm_id"], "realm-id");
    assert_eq!(v["realm"]["http_client"]["tls"]["insecure"], true);
    assert_eq!(v["tls"]["handshake_timeout"], "30s");
}

#[test]
fn singbox_keeps_chrome_parrot_without_client_certificate() {
    let n = node();
    assert!(n.certificate.is_none());
    assert!(n.singbox().get("disable_chrome_parrot").is_none());
}

#[test]
fn rejects_singbox_before_1_14_even_when_empty() {
    let nodes = [node()];
    for ua in [
        "sing-box/1.10.0",
        "sing-box/1.11.0",
        "sing-box/1.12.22",
        "sing-box/1.13.21",
    ] {
        for assigned in [nodes.as_slice(), &[]] {
            let error = render_subscription(
                assigned,
                SubscriptionFormat::Singbox,
                &ClientInfo::detect(ua),
            )
            .err()
            .unwrap();
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(error.code, "subscription_incompatible");
            assert!(error.message.contains("1.14.0"));
        }
    }
    for ua in [
        "sing-box/1.14.0",
        "sing-box/1.14.3",
        "sing-box",
        "curl/8.0",
        "",
    ] {
        let mut n = node();
        n.certificate = Some("cert".into());
        let body = render_singbox_template(&[&n], &ClientInfo::detect(ua)).unwrap();
        let config: Value = serde_json::from_str(&body).unwrap();
        assert!(config["dns"].get("optimistic").is_some());
        assert_eq!(
            config["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["type"] == "hysteria2")
                .unwrap()["disable_chrome_parrot"],
            true
        );
    }
}

#[test]
fn platform_detection_and_explicit_override() {
    for (ua, platform) in [
        ("sing-box/1.14.2 (linux; amd64)", Platform::Linux),
        ("sing-box (OpenWrt)", Platform::Linux),
        ("sing-box/1.14.2 (Windows NT 10.0)", Platform::Windows),
        ("Mozilla/5.0 (Macintosh; Intel Mac OS X)", Platform::Macos),
        ("sing-box/1.14.2 (darwin; arm64)", Platform::Macos),
        ("Mozilla/5.0 (Linux; Android 15)", Platform::Android),
        (
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18 like Mac OS X)",
            Platform::Ios,
        ),
        ("sing-box/1.14.2 (iPad)", Platform::Ios),
        ("notwindows notlinux notandroid", Platform::Unknown),
        ("sing-box", Platform::Unknown),
    ] {
        assert_eq!(ClientInfo::detect(ua).platform, platform, "{ua}");
    }
    assert_eq!(Platform::requested(None).unwrap(), None);
    assert_eq!(Platform::requested(Some("platform=auto")).unwrap(), None);
    for (value, expected) in [
        ("linux", Platform::Linux),
        ("openwrt", Platform::Linux),
        ("windows", Platform::Windows),
        ("macos", Platform::Macos),
        ("android", Platform::Android),
        ("ios", Platform::Ios),
    ] {
        assert_eq!(
            Platform::requested(Some(&format!("format=singbox&platform={value}"))).unwrap(),
            Some(expected)
        );
    }
    for query in [
        "platform=",
        "platform=unknown",
        "platform=windows&platform=linux",
    ] {
        assert_eq!(
            Platform::requested(Some(query)).err().unwrap().status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[test]
fn platform_templates_preserve_nodes_and_only_supported_listeners() {
    let mut n = node();
    n.certificate = Some("cert".into());
    n.private_key = Some("key".into());
    for platform in [
        Platform::Unknown,
        Platform::Linux,
        Platform::Windows,
        Platform::Macos,
        Platform::Android,
        Platform::Ios,
    ] {
        let client = ClientInfo {
            platform,
            ..ClientInfo::default()
        };
        let config: Value =
            serde_json::from_str(&render_singbox_template(&[&n], &client).unwrap()).unwrap();
        let inbounds = config["inbounds"].as_array().unwrap();
        let has = |kind| inbounds.iter().any(|i| i["type"] == kind);
        let linux = matches!(platform, Platform::Unknown | Platform::Linux);
        assert_eq!(has("tproxy"), linux);
        assert_eq!(has("redirect"), linux || platform == Platform::Macos);
        assert!(has("http") && has("socks") && has("tun") && has("direct"));
        let tun = inbounds.iter().find(|i| i["type"] == "tun").unwrap();
        assert_eq!(tun.get("auto_redirect").is_some(), linux);
        assert_eq!(
            tun.get("interface_name").is_some(),
            linux || platform == Platform::Windows
        );
        assert_eq!(
            tun.get("auto_route").and_then(Value::as_bool),
            if matches!(platform, Platform::Android | Platform::Ios) {
                None
            } else {
                Some(true)
            }
        );
        assert!(
            config["outbounds"]
                .as_array()
                .unwrap()
                .contains(&n.singbox())
        );
        assert_eq!(config["route"]["final"], "默认代理");
        assert!(
            config["route"]["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["inbound"] == "dns-in")
        );
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&render_mihomo_template(&[&n], platform).unwrap()).unwrap();
        assert_eq!(
            yaml.get("redir-port").is_some(),
            linux || platform == Platform::Macos
        );
        assert_eq!(yaml["mixed-port"].as_u64(), Some(7888));
        assert_eq!(yaml["proxies"][0]["certificate"].as_str(), Some("cert"));
    }
}

#[test]
fn uri_encodes_password_ipv6_names_and_query() {
    let mut n = node();
    n.obfs = Some("salamander".into());
    n.obfs_password = Some("/ #&".into());
    let uri = n.uri();
    let parsed = url::Url::parse(&uri).unwrap();
    assert_eq!(parsed.host_str(), Some("[2001:db8::1]"));
    assert_eq!(parsed.port(), Some(443));
    assert!(parsed.password().is_none());
    assert_eq!(
        url::form_urlencoded::parse(parsed.username().as_bytes())
            .next()
            .unwrap()
            .0,
        n.password
    );
    assert_eq!(
        url::form_urlencoded::parse(parsed.fragment().unwrap().as_bytes())
            .next()
            .unwrap()
            .0,
        n.name
    );
    let query: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
    assert_eq!(query.get("sni").unwrap(), "edge.example");
    assert_eq!(query.get("insecure").unwrap(), "1");
    assert_eq!(query.get("obfs-password").unwrap(), "/ #&");
}

#[tokio::test]
async fn renders_filtered_and_empty_subscriptions_and_base64_roundtrip() {
    let mut blocked = node();
    blocked.ech_config = Some("QUJD".into());
    let nodes = [node(), blocked];
    let response =
        render_subscription(&nodes, SubscriptionFormat::Uri, &ClientInfo::default()).unwrap();
    assert_eq!(response.headers()["x-hysteriax-filtered-nodes"], "1");
    let uri = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let response =
        render_subscription(&nodes, SubscriptionFormat::Base64, &ClientInfo::default()).unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(STANDARD.decode(bytes).unwrap(), uri);
    let error = render_subscription(&nodes[1..], SubscriptionFormat::Uri, &ClientInfo::default())
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error.code, "subscription_incompatible");
    for format in [
        SubscriptionFormat::Mihomo,
        SubscriptionFormat::Singbox,
        SubscriptionFormat::Uri,
        SubscriptionFormat::Base64,
    ] {
        let response = render_subscription(&[], format, &ClientInfo::default()).unwrap();
        assert_eq!(response.headers()["x-hysteriax-filtered-nodes"], "0");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        match format {
            SubscriptionFormat::Mihomo => {
                let v: serde_yaml::Value = serde_yaml::from_slice(&body).unwrap();
                assert_eq!(v["rules"][30], "MATCH,PROXY");
                assert_eq!(v["proxy-groups"][0]["proxies"][0], "DIRECT");
            }
            SubscriptionFormat::Singbox => {
                let v: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(v["route"]["final"], "默认代理");
                assert_eq!(v["outbounds"][0]["outbounds"], json!(["直连"]));
            }
            _ => assert!(body.is_empty()),
        }
    }
}

#[test]
fn singbox_template_resolves_groups_for_current_assignments() {
    let mut nodes = Vec::new();
    for name in [
        "新加坡-new",
        "美国旧金山-new",
        "美国硅谷-new",
        "韩国春川-new",
        "香港家宽-new",
    ] {
        let mut n = node();
        n.name = name.into();
        n.display_name = name.into();
        n.id = name.into();
        nodes.push(n);
    }
    for assigned in [nodes.iter().collect::<Vec<_>>(), vec![&nodes[0]], vec![]] {
        let body = render_singbox_template(&assigned, &ClientInfo::default()).unwrap();
        let config: Value = serde_json::from_str(&body).unwrap();
        let outbounds = config["outbounds"].as_array().unwrap();
        let tags: Vec<_> = outbounds
            .iter()
            .map(|o| o["tag"].as_str().unwrap())
            .collect();
        assert!(!body.contains("{{"));
        assert!(!body.contains("hx_"));
        assert_eq!(
            outbounds
                .iter()
                .filter(|o| o["type"] == "hysteria2")
                .count(),
            assigned.len()
        );
        for outbound in outbounds {
            if let Some(members) = outbound["outbounds"].as_array() {
                assert!(!members.is_empty());
                for member in members {
                    assert!(tags.contains(&member.as_str().unwrap()));
                    assert_ne!(member, &outbound["tag"]);
                }
                if let Some(default) = outbound.get("default") {
                    assert!(members.contains(default));
                }
            }
        }
        if assigned.len() > 1 {
            let group = |tag| outbounds.iter().find(|o| o["tag"] == tag).unwrap();
            assert_eq!(group("默认代理")["default"], "韩国春川-new");
            assert_eq!(group("手动选择")["default"], "美国硅谷-new");
            assert_eq!(
                group("美国节点")["outbounds"],
                json!(["美国旧金山-new", "美国硅谷-new"])
            );
            assert!(
                !group("自动选择")["outbounds"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("新加坡-new"))
            );
            assert_eq!(group("日本节点")["outbounds"].as_array().unwrap().len(), 1);
        }
    }
}

#[test]
fn bandwidth_conversion_preserves_decimal_units_and_rejects_lossy_values() {
    assert_eq!(bandwidth_mbps("1 Gbps"), Some(1000));
    assert_eq!(bandwidth_mbps("2000000 bps"), Some(2));
    assert_eq!(bandwidth_mbps("0 Mbps"), Some(0));
    for value in ["700 kbps", "1.5 Mbps", "", "18446744073709551615 Tbps"] {
        assert_eq!(bandwidth_mbps(value), None);
    }
}
