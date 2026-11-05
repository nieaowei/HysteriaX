use serde_json::{Value, json};

use super::{
    DEFAULT_REALM_STUN_SERVERS, realm_client_uri, realm_connection, render_server_yaml,
    render_server_yaml_preview, validate_server_options,
};

fn assert_yaml_preserves_options(options: &serde_json::Value) {
    let yaml = render_server_yaml(
        options,
        ":443",
        "node-id",
        "node-token",
        "stats-secret",
        "https://management.example",
    )
    .unwrap();
    let rendered: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let expected = serde_yaml::to_value(options).unwrap();
    for (key, value) in expected.as_mapping().unwrap() {
        assert_eq!(
            rendered.get(key),
            Some(value),
            "rendered YAML did not preserve option {key:?} from {options}"
        );
    }
}

#[test]
fn rejects_managed_authentication_and_statistics_fields() {
    assert!(validate_server_options(&json!({"auth": {"type": "password"}})).is_err());
    assert!(validate_server_options(&json!({"trafficStats": {"listen": ":9999"}})).is_err());
}

#[test]
fn rejects_mutually_exclusive_tls_modes_and_mimic() {
    assert!(validate_server_options(&json!({"tls": {}, "acme": {}})).is_err());
    assert!(validate_server_options(&json!({"mimic": {"enabled": true}})).is_err());
    assert!(validate_server_options(&json!({"ech": {"keyPath": "resource://ech-id"}})).is_err());
    assert!(validate_server_options(&json!({"realm": {"insecure": false}})).is_ok());
    assert!(
        validate_server_options(&json!({
            "tls": {
                "cert": "/etc/server.pem",
                "key": "/etc/server-key.pem",
                "clientCA": "/etc/client-ca.pem"
            }
        }))
        .is_ok()
    );
    assert!(
        validate_server_options(&json!({
            "tls": {"cert": "/etc/server.pem", "key": "/etc/server-key.pem", "clientCA": 42}
        }))
        .is_err()
    );
}

#[test]
fn validates_ech_key_resource_and_tls_dependency() {
    assert!(
        validate_server_options(&json!({
            "tls": {"cert": "/etc/server.pem", "key": "/etc/server-key.pem"},
            "ech": {"keyPath": "resource://ech-key-id"}
        }))
        .is_ok()
    );
    assert!(
        validate_server_options(&json!({
            "tls": {"cert": "/etc/server.pem", "key": "/etc/server-key.pem"},
            "ech": {"keyPath": "/etc/hysteriax/ech.pem"}
        }))
        .is_err()
    );
    assert!(
        validate_server_options(&json!({
            "ech": {"keyPath": "resource://ech-key-id"}
        }))
        .is_err()
    );
}

#[test]
fn validates_realm_connection_and_requires_tls() {
    let connection = json!({
        "serverURL": "http://127.0.0.1:10820",
        "token": "private-token_123",
        "realmID": "local-test-1"
    });
    assert!(
        validate_server_options(&json!({
            "tls": {"cert": "/etc/server.pem", "key": "/etc/server-key.pem"},
            "realm": {"connection": connection, "stunServers": ["stun.example:3478"]}
        }))
        .is_ok()
    );
    assert!(
        validate_server_options(&json!({
            "realm": {"connection": {
                "serverURL": "http://127.0.0.1:10820",
                "token": "private-token_123",
                "realmID": "local-test-1"
            }}
        }))
        .is_err()
    );
    let url_with_credentials = format!("https://user:secret{}rendezvous.example/path", '@');
    assert!(
        validate_server_options(&json!({"tls": {}, "realm": {"connection": {
            "serverURL": url_with_credentials,
            "token": "private-token_123",
            "realmID": "local-test-1"
        }}}))
        .is_err()
    );
    assert!(
        validate_server_options(&json!({"realm": {"connection": {
            "serverURL": "https://rendezvous.example",
            "token": "token/with/slashes",
            "realmID": "local-test-1"
        }}}))
        .is_err()
    );
}

#[test]
fn realm_connection_supplies_the_hysteria_default_stun_servers() {
    let config = json!({
        "tls": {"cert": "/etc/server.pem", "key": "/etc/server-key.pem"},
        "realm": {"connection": {
            "serverURL": "https://rendezvous.example",
            "token": "realm-token",
            "realmID": "realm-test-1"
        }}
    });
    let connection = realm_connection(&config).unwrap().unwrap();
    assert_eq!(
        realm_client_uri(&connection).unwrap(),
        "realm://realm-token@rendezvous.example/realm-test-1"
    );
    assert_eq!(
        connection.stun_servers,
        DEFAULT_REALM_STUN_SERVERS
            .iter()
            .map(|server| (*server).to_owned())
            .collect::<Vec<_>>()
    );
}

#[test]
fn realm_connection_config_renders_as_a_realm_listener() {
    let options = json!({
        "tls": {"cert": "/etc/server.pem", "key": "/etc/server-key.pem"},
        "realm": {
            "connection": {
                "serverURL": "http://127.0.0.1:10820",
                "token": "private-token_123",
                "realmID": "local-test-1"
            },
            "stunServers": ["stun.example:3478"],
            "punchTimeout": "10s"
        }
    });
    let yaml = render_server_yaml(
        &options,
        ":443",
        "node-id",
        "node-secret",
        "stats-secret",
        "https://management.example",
    )
    .unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(
        value["listen"].as_str(),
        Some("realm+http://private-token_123@127.0.0.1:10820/local-test-1?lport=443")
    );
    assert!(value["realm"].get("connection").is_none());
    assert_eq!(
        value["realm"]["stunServers"][0].as_str(),
        Some("stun.example:3478")
    );
    let preview = render_server_yaml_preview(
        &options,
        ":443",
        "node-id",
        "node-secret",
        "stats-secret",
        "https://management.example",
    )
    .unwrap();
    assert!(preview.contains("realm+http://REDACTED_TOKEN@127.0.0.1:10820/local-test-1?lport=443"));
    assert!(!preview.contains("private-token_123"));
}

#[test]
fn validates_common_nested_sections() {
    assert!(validate_server_options(&json!({"acme": {"domains": []}})).is_err());
    assert!(validate_server_options(&json!({"resolver": {"type": "dns"}})).is_err());
    assert!(validate_server_options(&json!({"acl": {"file": "rules.txt", "inline": []}})).is_err());
    assert!(
        validate_server_options(&json!({"outbounds": [
            {"name": "same", "type": "direct"},
            {"name": "same", "type": "direct"}
        ]}))
        .is_err()
    );
    assert!(validate_server_options(&json!({"bandwidth": {"up": "fast Mbps"}})).is_err());
}

#[test]
fn validates_pinned_hysteria_bandwidth_format_and_minimum() {
    assert!(validate_server_options(&json!({"bandwidth": {"up": "100 Mbps"}})).is_ok());
    assert!(validate_server_options(&json!({"bandwidth": {"up": "525 kbps"}})).is_ok());
    assert!(validate_server_options(&json!({"bandwidth": {"up": "0 bps"}})).is_ok());
    assert!(validate_server_options(&json!({"bandwidth": {"up": "524 kbps"}})).is_err());
    assert!(validate_server_options(&json!({"bandwidth": {"up": "1 bps"}})).is_err());
    assert!(validate_server_options(&json!({"bandwidth": {"up": "1.5 Mbps"}})).is_err());
    assert!(validate_server_options(&json!({"bandwidth": {"up": "100"}})).is_err());
}

#[test]
fn validates_pinned_hysteria_quic_window_and_stream_constraints() {
    assert!(
        validate_server_options(&json!({"quic": {
            "initStreamReceiveWindow": 16384,
            "maxStreamReceiveWindow": 32768,
            "initConnReceiveWindow": 32768,
            "maxConnReceiveWindow": 65536,
            "maxIncomingStreams": 8,
            "maxIdleTimeout": "4s"
        }}))
        .is_ok()
    );
    assert!(
        validate_server_options(&json!({"quic": {
            "initStreamReceiveWindow": 0,
            "maxStreamReceiveWindow": 0,
            "maxIncomingStreams": 0,
            "maxIdleTimeout": "0s"
        }}))
        .is_ok()
    );
    assert!(validate_server_options(&json!({"quic": {"initStreamReceiveWindow": 16383}})).is_err());
    assert!(
        validate_server_options(&json!({"quic": {
            "initStreamReceiveWindow": 16777216,
            "maxStreamReceiveWindow": 8388608
        }}))
        .is_err()
    );
    assert!(
        validate_server_options(&json!({"quic": {
            "initConnReceiveWindow": 33554432,
            "maxConnReceiveWindow": 20971520
        }}))
        .is_err()
    );
    assert!(validate_server_options(&json!({"quic": {"maxIncomingStreams": 7}})).is_err());
    assert!(validate_server_options(&json!({"quic": {"maxIncomingStreams": -1}})).is_err());
    assert!(validate_server_options(&json!({"quic": {"maxIdleTimeout": "3s"}})).is_err());
    assert!(validate_server_options(&json!({"quic": {"maxIdleTimeout": "121s"}})).is_err());
    assert!(validate_server_options(&json!({"quic": {"maxIdleTimeout": "120s"}})).is_ok());
    assert!(validate_server_options(&json!({"quic": {"maxIdleTimeout": "1m30s"}})).is_ok());
    assert!(validate_server_options(&json!({"quic": {"maxIdleTimeout": "3s999ms"}})).is_err());
    assert!(validate_server_options(&json!({"quic": {"maxIdleTimeout": "1m30"}})).is_err());
    assert!(validate_server_options(&json!({"quic": {"maxStreamReceiveWindow": 16384}})).is_err());
}

#[test]
fn validates_pinned_hysteria_udp_idle_timeout_range() {
    assert!(validate_server_options(&json!({"udpIdleTimeout": "2s"})).is_ok());
    assert!(validate_server_options(&json!({"udpIdleTimeout": "600s"})).is_ok());
    assert!(validate_server_options(&json!({"udpIdleTimeout": "2.5s"})).is_ok());
    assert!(validate_server_options(&json!({"udpIdleTimeout": "1s"})).is_err());
    assert!(validate_server_options(&json!({"udpIdleTimeout": "601s"})).is_err());
    assert!(validate_server_options(&json!({"udpIdleTimeout": "0s"})).is_ok());
    assert!(validate_server_options(&json!({"udpIdleTimeout": "2m30s"})).is_ok());
}

#[test]
fn validates_pinned_acme_provider_and_legacy_field_combinations() {
    assert!(
        validate_server_options(&json!({"acme": {
            "domains": ["proxy.example"],
            "ca": "LE",
            "type": "DNS",
            "dns": {"name": "Cloudflare", "config": {"cloudflare_api_token": "token"}}
        }}))
        .is_ok()
    );
    assert!(
        validate_server_options(&json!({"acme": {
            "domains": ["proxy.example"],
            "type": "dns",
            "dns": {"name": "namedotcom", "config": {"token": "token"}}
        }}))
        .is_err()
    );
    assert!(
        validate_server_options(&json!({"acme": {
            "domains": ["proxy.example"],
            "type": "http",
            "disableHTTP": true
        }}))
        .is_err()
    );
    assert!(
        validate_server_options(&json!({"acme": {
            "domains": ["proxy.example"],
            "http": {"altPort": 8080}
        }}))
        .is_err()
    );
    assert!(
        validate_server_options(&json!({"acme": {
            "domains": ["proxy.example"],
            "disableHTTP": false,
            "altHTTPPort": 8080
        }}))
        .is_ok()
    );
    assert!(
        validate_server_options(&json!({"acme": {
            "domains": ["proxy.example"],
            "type": "",
            "ca": "",
            "disableHTTP": false
        }}))
        .is_ok()
    );
}

#[test]
fn validates_acme_dns_provider_credentials() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/acme-dns-providers.json"
    ))
    .unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let config = fixture["config"].clone();
        let node = |config: Value| {
            json!({"acme": {
                "domains": ["proxy.example"], "type": "dns",
                "dns": {"name": fixture["provider"], "config": config}
            }})
        };
        assert!(validate_server_options(&node(config.clone())).is_ok());
        for key in fixture["required"].as_array().unwrap() {
            let key = key.as_str().unwrap();
            for value in [None, Some(json!("")), Some(json!(" \n\t"))] {
                let mut invalid = config.clone();
                invalid.as_object_mut().unwrap().remove(key);
                if let Some(value) = value {
                    invalid[key] = value;
                }
                let error = validate_server_options(&node(invalid)).unwrap_err();
                assert!(error.to_string().contains(key));
                assert!(!error.to_string().contains("fixture-"));
            }
        }
        let mut optional = config.clone();
        for key in fixture["optional"].as_array().unwrap() {
            optional
                .as_object_mut()
                .unwrap()
                .remove(key.as_str().unwrap());
        }
        assert!(validate_server_options(&node(optional.clone())).is_ok());
        optional["legacy_unknown"] = json!("preserved-secret");
        assert!(validate_server_options(&node(optional)).is_ok());
        assert!(validate_server_options(&node(json!({}))).is_err());
    }
}

#[test]
fn validates_namecheap_optional_ip_and_endpoint() {
    let node = |extra: Value| {
        let mut config = json!({"namecheap_api_key": "secret-key", "namecheap_api_user": "user"});
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"acme": {
            "domains": ["proxy.example"], "type": "dns",
            "dns": {"name": "namecheap", "config": config}
        }})
    };
    for ip in ["127.0.0.1", "203.0.113.1", "", " \n"] {
        assert!(validate_server_options(&node(json!({"namecheap_client_ip": ip}))).is_ok());
    }
    for ip in ["999.1.1.1", "::1", "invalid", " 203.0.113.1", "1.2.3"] {
        assert!(validate_server_options(&node(json!({"namecheap_client_ip": ip}))).is_err());
    }
    for endpoint in [
        "https://api.namecheap.com/xml.response",
        "http://localhost/xml.response",
        "",
        " \n",
    ] {
        assert!(
            validate_server_options(&node(json!({"namecheap_api_endpoint": endpoint}))).is_ok()
        );
    }
    for endpoint in [
        "ftp://example.com",
        "example.com",
        "https://",
        "https://example.com/a b",
    ] {
        let error = validate_server_options(&node(json!({"namecheap_api_endpoint": endpoint})))
            .unwrap_err();
        assert!(!error.to_string().contains("secret-key"));
    }
}

#[test]
fn validates_supported_server_field_shapes() {
    let config = json!({
        "tls": {"cert": "/etc/cert.pem", "key": "/etc/key.pem", "sniGuard": "strict"},
        "obfs": {"type": "gecko", "gecko": {"password": "secret", "minPacketSize": 512, "maxPacketSize": 1200}},
        "bandwidth": {"up": "100 Mbps", "down": "500 Mbps", "disableLossCompensation": false},
        "ignoreClientBandwidth": false,
        "congestion": {"type": "bbr", "bbrProfile": "standard"},
        "speedTest": false,
        "disableUDP": false,
        "udpIdleTimeout": "30s",
        "quic": {
            "initStreamReceiveWindow": 65536,
            "maxStreamReceiveWindow": 1048576,
            "initConnReceiveWindow": 65536,
            "maxConnReceiveWindow": 2097152,
            "maxIdleTimeout": "30s",
            "maxIncomingStreams": 128,
            "disablePathMTUDiscovery": false,
            "disableStatelessReset": false
        },
        "resolver": {"type": "https", "https": {"addr": "https://dns.example/dns-query", "timeout": "4s", "sni": "dns.example", "insecure": false}},
        "sniff": {"enable": true, "timeout": "2s", "rewriteDomain": false, "tcpPorts": "80,443", "udpPorts": "all"},
        "acl": {"inline": ["reject(all, udp/443)"], "geoUpdateInterval": "24h"},
        "outbounds": [{"name": "direct", "type": "direct", "direct": {"mode": "auto", "bindIPv4": "0.0.0.0", "fastOpen": true}}],
        "masquerade": {"type": "string", "string": {"content": "hello", "headers": {"x-test": "ok"}, "statusCode": 200}, "listenHTTP": ":80", "forceHTTPS": false}
    });
    assert!(validate_server_options(&config).is_ok());
    assert_yaml_preserves_options(&config);
    assert!(
        validate_server_options(&json!({"acme": {
            "domains": ["proxy.example"],
            "type": "dns",
            "dns": {"name": "cloudflare", "config": {"cloudflare_api_token": "value"}}
        }}))
        .is_ok()
    );
}

#[test]
fn validates_and_serializes_each_supported_field_family() {
    let cases = [
        (
            "Realm tuning",
            json!({"realm": {
                "stunServers": ["stun.example:3478"],
                "stunTimeout": "4s",
                "punchTimeout": "8s",
                "heartbeatInterval": "30s",
                "insecure": false,
                "ipMode": "dual",
                "portMapping": {"enabled": true, "timeout": "10s", "lifetime": "10m"}
            }}),
        ),
        (
            "Mimic disabled settings",
            json!({"mimic": {
                "enabled": false,
                "interface": "eth0",
                "xdpMode": "skb",
                "path": "/usr/bin/mimic",
                "extraArgs": ["--verbose"]
            }}),
        ),
        (
            "TLS and ECH",
            json!({
                "tls": {
                    "cert": "resource://certificate-id",
                    "key": "resource://private-key-id",
                    "sniGuard": "dns-san",
                    "clientCA": "resource://client-ca-id"
                },
                "ech": {"keyPath": "resource://ech-key-id"}
            }),
        ),
        (
            "ACME HTTP",
            json!({"acme": {
                "domains": ["proxy.example"],
                "email": "ops@example.com",
                "ca": "letsencrypt",
                "listenHost": "0.0.0.0",
                "dir": "/var/lib/hysteria/acme",
                "type": "http",
                "http": {"altPort": 8080}
            }}),
        ),
        (
            "ACME TLS",
            json!({"acme": {
                "domains": ["proxy.example"],
                "type": "tls",
                "tls": {"altPort": 444}
            }}),
        ),
        (
            "ACME legacy fields",
            json!({"acme": {
                "domains": ["proxy.example"],
                "disableHTTP": false,
                "disableTLSALPN": true,
                "altHTTPPort": 8080,
                "altTLSALPNPort": 444
            }}),
        ),
        (
            "ACME DNS",
            json!({"acme": {
                "domains": ["proxy.example"],
                "email": "ops@example.com",
                "ca": "zerossl",
                "listenHost": "127.0.0.1",
                "dir": "/var/lib/hysteria/acme",
                "type": "dns",
                "dns": {"name": "cloudflare", "config": {"cloudflare_api_token": "secret"}}
            }}),
        ),
        (
            "Salamander obfuscation",
            json!({"obfs": {"type": "salamander", "salamander": {"password": "secret"}}}),
        ),
        (
            "Gecko obfuscation",
            json!({"obfs": {"type": "gecko", "gecko": {
                "password": "secret", "minPacketSize": 600, "maxPacketSize": 1400
            }}}),
        ),
        (
            "TCP resolver",
            json!({"resolver": {"type": "tcp", "tcp": {"addr": "1.1.1.1:53", "timeout": "4s"}}}),
        ),
        (
            "UDP resolver",
            json!({"resolver": {"type": "udp", "udp": {"addr": "1.1.1.1:53", "timeout": "4s"}}}),
        ),
        (
            "TLS resolver",
            json!({"resolver": {"type": "tls", "tls": {
                "addr": "1.1.1.1:853", "timeout": "4s", "sni": "dns.example", "insecure": false
            }}}),
        ),
        (
            "ACL file and Geo resources",
            json!({"acl": {
                "file": "resource://acl-id",
                "geoip": "resource://geoip-id",
                "geosite": "resource://geosite-id",
                "geoUpdateInterval": "168h"
            }}),
        ),
        (
            "Inline ACL rules",
            json!({"acl": {"inline": ["reject(all, udp/443)"], "geoUpdateInterval": "24h"}}),
        ),
        (
            "All outbound kinds",
            json!({"outbounds": [
                {"name": "direct", "type": "direct", "direct": {
                    "mode": "auto", "bindIPv4": "192.0.2.10", "bindIPv6": "2001:db8::10",
                    "bindDevice": "eth0", "fastOpen": true
                }},
                {"name": "socks", "type": "socks5", "socks5": {
                    "addr": "127.0.0.1:1080", "username": "user", "password": "secret"
                }},
                {"name": "http", "type": "http", "http": {
                    "url": "https://proxy.example", "insecure": false
                }}
            ]}),
        ),
        (
            "File masquerade",
            json!({"masquerade": {"type": "file", "file": {"dir": "/srv/www"}}}),
        ),
        (
            "String masquerade",
            json!({"masquerade": {
                "type": "string",
                "string": {"content": "hello", "headers": {"x-test": "ok"}, "statusCode": 202},
                "listenHTTP": ":8080",
                "listenHTTPS": ":8443",
                "forceHTTPS": true
            }}),
        ),
        (
            "Proxy masquerade",
            json!({"masquerade": {"type": "proxy", "proxy": {
                    "url": "https://site.example", "rewriteHost": true, "xForwarded": false, "insecure": false
                }, "listenHTTP": ":8080", "listenHTTPS": ":8443", "forceHTTPS": true}}),
        ),
    ];

    for (field_family, config) in cases {
        assert!(
            validate_server_options(&config).is_ok(),
            "{field_family} should accept its supported field shape: {config}"
        );
        assert_yaml_preserves_options(&config);
    }
}

#[test]
fn rejects_unknown_nested_fields_and_wrong_duration_types() {
    assert!(validate_server_options(&json!({"unknownServerOption": true})).is_err());
    assert!(
        validate_server_options(&json!({"quic": {"maxIdleTimeout": "30s", "typo": true}})).is_err()
    );
    assert!(
        validate_server_options(
            &json!({"masquerade": {"type": "string", "string": {"content": "hello", "header": {}}}})
        )
        .is_err()
    );
    assert!(validate_server_options(&json!({"outbounds": [{"name": "proxy", "type": "socks5", "socks5": {"addr": "127.0.0.1:1080", "secret": "x"}}]})).is_err());
    assert!(validate_server_options(&json!({"udpIdleTimeout": 30})).is_err());
    assert!(
        validate_server_options(
            &json!({"acme": {"domains": ["proxy.example"], "http": {"altPor": 8080}}})
        )
        .is_err()
    );
}

#[test]
fn renders_managed_auth_stats_and_node_listener_as_yaml() {
    let yaml = render_server_yaml(
        &json!({"disableUDP": false}),
        ":8443",
        "node-id",
        "node-secret",
        "stats-secret",
        "https://management.example",
    )
    .unwrap();
    let config: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(config["listen"].as_str(), Some(":8443"));
    assert_eq!(
        config["trafficStats"]["listen"].as_str(),
        Some("127.0.0.1:9780")
    );
    assert_eq!(config["auth"]["type"].as_str(), Some("http"));
    assert!(
        config["auth"]["http"]["url"]
            .as_str()
            .unwrap()
            .contains("node-secret")
    );
}
