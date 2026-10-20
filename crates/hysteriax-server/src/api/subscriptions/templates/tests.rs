use super::render::seed;
use super::*;
use document::*;
use serde_json::json;
#[test]
fn seed_templates_validate_and_render_every_platform() {
    for format in ["mihomo", "singbox"] {
        let document = seed(format);
        let diagnostics = validate(format, &document).unwrap();
        let failures: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == "error")
            .map(|d| format!("{} {} {}", d.layer, d.path, d.message))
            .collect();
        assert!(failures.is_empty(), "{format}: {failures:?}");
        verify_rendering(format, &document).unwrap();
    }
}
#[test]
fn overlay_preserves_explicit_values_and_replaces_arrays() {
    let mut d = Document {
        schema_version: 1,
        common: json!({"flag":true,"n":3,"list":[1,2],"nested":{"a":1,"b":2},"unknown":{"x":true}}),
        platforms: Default::default(),
        injections: vec![],
    };
    d.platforms.insert(
        "windows".into(),
        Overlay {
            set: json!({"flag":false,"n":0,"list":[],"nested":{"a":null}}),
            remove: vec!["/nested/b".into()],
        },
    );
    let value = d.effective("windows").unwrap();
    assert_eq!(
        value,
        json!({"flag":false,"n":0,"list":[],"nested":{"a":null},"unknown":{"x":true}})
    );
    for format in ["mihomo", "singbox"] {
        assert_eq!(
            parse(format, &serialize(format, &value).unwrap()).unwrap(),
            value
        );
    }
    d.platforms
        .get_mut("windows")
        .unwrap()
        .remove
        .push("/flag".into());
    assert!(d.shape().is_err());
}
#[test]
fn invalid_platform_and_references_are_locatable() {
    let mut d = seed("mihomo");
    d.common["tproxy-port"] = json!(0);
    d.common["proxy-groups"][0]["proxies"] = json!(["missing"]);
    let diagnostics = validate("mihomo", &d).unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.path == "/tproxy-port" && d.layer == "common")
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.path == "/proxy-groups/0/proxies/0")
    );
    d.common["proxy-groups"][0]["proxies"] = json!(["PROXY"]);
    assert!(
        validate("mihomo", &d)
            .unwrap()
            .iter()
            .any(|d| d.message.contains("cycle"))
    );
}
#[test]
fn unknown_fields_warn_and_known_types_fail() {
    let mut d = seed("mihomo");
    d.common["future-setting"] = json!({"v":[false,null,0]});
    assert!(
        validate("mihomo", &d)
            .unwrap()
            .iter()
            .any(|d| d.path == "/future-setting" && d.severity == "warning")
    );
    d.common["allow-lan"] = json!("yes");
    assert!(
        validate("mihomo", &d)
            .unwrap()
            .iter()
            .any(|d| d.path == "/allow-lan" && d.severity == "error")
    );
}

use super::store::*;
use crate::{security::SecretBox, state::AppState};
use axum::{
    Json,
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use sqlx::Row;
async fn fixture() -> AppState {
    let pool = crate::db::test_pool_with_max_connections(5).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    initialize(&pool, &secrets).await.unwrap();
    AppState::new(pool, secrets)
}
async fn body(response: Response) -> serde_json::Value {
    assert_eq!(response.headers()["cache-control"], "no-store");
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 2_000_000)
            .await
            .unwrap(),
    )
    .unwrap()
}
#[tokio::test]
async fn lifecycle_keeps_drafts_private_versions_immutable_and_writes_serialized() {
    let state = fixture().await;
    let id = "default-mihomo".to_string();
    let initial = published(&state, Some(&id), None).await.unwrap();
    let mut document = initial.3.clone();
    document.common["mixed-port"] = json!(9999);
    let Json(saved) = save(
        State(state.clone()),
        Path(id.clone()),
        Json(Save {
            expected_revision: 1,
            document: document.clone(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(saved["revision"], 2);
    assert_eq!(
        published(&state, Some(&id), None).await.unwrap().3.common["mixed-port"],
        7888
    );
    let a = publish(
        State(state.clone()),
        Path(id.clone()),
        Json(Revision {
            expected_revision: 2,
        }),
    );
    let b = publish(
        State(state.clone()),
        Path(id.clone()),
        Json(Revision {
            expected_revision: 2,
        }),
    );
    let (a, b) = tokio::join!(a, b);
    assert_ne!(a.is_ok(), b.is_ok());
    let err = a
        .err()
        .or_else(|| b.err())
        .expect("one publication must conflict");
    assert_eq!(err.status, StatusCode::CONFLICT);
    let next = published(&state, Some(&id), None).await.unwrap();
    assert_eq!(next.1, 2);
    assert_eq!(next.3.common["mixed-port"], 9999);
    let Json(rolled) = rollback(
        State(state.clone()),
        Path(id.clone()),
        Json(Rollback {
            expected_revision: 3,
            version: 1,
        }),
    )
    .await
    .unwrap();
    assert_eq!(rolled["published_version"], 3);
    assert_eq!(
        published(&state, Some(&id), None).await.unwrap().3.common["mixed-port"],
        7888
    );
    let row=sqlx::query("SELECT document_enc FROM subscription_template_versions WHERE template_id=$1 AND version=2").bind(&id).fetch_one(&state.pool).await.unwrap();
    let cipher: String = row.get("document_enc");
    assert!(!cipher.contains("9999"));
    assert!(state.secrets.decrypt(&cipher).unwrap().contains("9999"));
    initialize(&state.pool, &state.secrets).await.unwrap();
    assert_eq!(published(&state, Some(&id), None).await.unwrap().1, 3);
    let audit:String=sqlx::query_scalar("SELECT string_agg(detail_json::text,'') FROM audit_records WHERE entity_type='subscription_template'").fetch_one(&state.pool).await.unwrap();
    assert!(!audit.contains("mixed-port"));
}
#[tokio::test]
async fn default_archival_requires_published_replacement_and_preserves_history() {
    let state = fixture().await;
    let (_, Json(created)) = create(
        State(state.clone()),
        Json(Create {
            expected_revision: 0,
            name: "Replacement".into(),
            format: "mihomo".into(),
            document: None,
        }),
    )
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    let input = |replacement: Option<String>, revision: Option<i64>| Patch {
        expected_revision: 1,
        name: None,
        is_default: None,
        archived: Some(true),
        replacement_id: replacement,
        replacement_revision: revision,
    };
    assert!(
        patch(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(input(None, None))
        )
        .await
        .is_err()
    );
    assert!(
        patch(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(input(Some(id.clone()), Some(1)))
        )
        .await
        .is_err()
    );
    let _ = publish(
        State(state.clone()),
        Path(id.clone()),
        Json(Revision {
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    let _ = patch(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(input(Some(id.clone()), Some(2))),
    )
    .await
    .unwrap();
    assert!(
        published(&state, Some("default-mihomo"), None)
            .await
            .is_err()
    );
    assert_eq!(published(&state, None, Some("mihomo")).await.unwrap().0, id);
    let Json(versions) = versions(
        State(state.clone()),
        Path("default-mihomo".into()),
        Query(ListQuery::default()),
    )
    .await
    .unwrap();
    assert_eq!(versions["total"], 1);
}
#[tokio::test]
async fn import_export_preserves_layers_unknown_fields_and_reports_node_removal() {
    let state = fixture().await;
    let document = render::single_seed("singbox");
    let exported = body(
        export(State(state.clone()), Path("default-singbox".into()))
            .await
            .unwrap(),
    )
    .await;
    let imported = body(
        import(Json(Import {
            format: "singbox".into(),
            source: None,
            package: Some(exported),
        }))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        imported["document"],
        serde_json::to_value(document).unwrap()
    );
    let legacy = seed("singbox");
    let legacy_imported = body(
        import(Json(Import {
            format: "singbox".into(),
            source: None,
            package: Some(json!({"package_version":1,"format":"singbox","document":legacy})),
        }))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        legacy_imported["document"],
        serde_json::to_value(legacy).unwrap()
    );
    let imported=body(import(Json(Import{format:"mihomo".into(),source:Some("proxies:\n - {name: imported-node, type: ss, password: secret}\nproxy-groups:\n - {name: Select, type: select, proxies: [imported-node]}\nrules: [MATCH,Select]\ntproxy-port: 8000\nfuture: {a: [false, null, 0]}".into()),package:None})).await.unwrap()).await;
    assert_eq!(imported["removed_nodes"][0]["name"], "imported-node");
    assert!(!imported.to_string().contains("secret"));
    assert_eq!(imported["document"]["common"]["tproxy-port"], 8000);
    assert_eq!(
        imported["document"]["common"]["future"],
        json!({"a":[false,null,0]})
    );
    assert_eq!(imported["reference_mapping"][0]["group"], "Select");
}
#[tokio::test]
async fn public_download_selects_platform_uses_latest_publication_and_revokes_live() {
    let state = fixture().await;
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at) VALUES('alice','Alice',now(),now())",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('sub','alice',$1,$2,now())").bind(crate::security::token_digest("token")).bind(state.secrets.encrypt("token").unwrap()).execute(&state.pool).await.unwrap();
    async fn download(state: &AppState, query: &str) -> Response {
        super::super::download_auto(
            State(state.clone()),
            Path("token".into()),
            RawQuery(Some(query.into())),
            HeaderMap::new(),
        )
        .await
    }
    let response = download(&state, "template=default-mihomo").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("text/html")
    );
    let response = download(&state, "template=default-mihomo&format=mihomo").await;
    let page = String::from_utf8(
        axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(page.contains("template=default-mihomo&amp;format=mihomo&amp;platform=windows"));
    let response = download(&state, "").await;
    let page = String::from_utf8(
        axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(page.contains("?format=uri") && page.contains("?format=base64"));
    for query in [
        "template=default-mihomo&format=singbox&platform=linux",
        "template=default-mihomo&format=uri",
        "template=x&template=y",
        "platform=linux&platform=ios",
        "platform=invalid",
    ] {
        assert_eq!(
            download(&state, query).await.status(),
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    for platform in ["linux", "openwrt", "macos", "windows", "android", "ios"] {
        let response = download(
            &state,
            &format!("template=default-mihomo&platform={platform}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let text = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        let config: serde_json::Value = serde_yaml::from_slice(&text).unwrap();
        assert_eq!(
            config.get("redir-port").is_some(),
            ["linux", "openwrt", "macos"].contains(&platform)
        );
    }
    let mut document = seed("mihomo");
    document.common["mixed-port"] = json!(9999);
    let _ = save(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Save {
            expected_revision: 1,
            document,
        }),
    )
    .await
    .unwrap();
    let _ = publish(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Revision {
            expected_revision: 2,
        }),
    )
    .await
    .unwrap();
    let response = download(&state, "template=default-mihomo&platform=linux").await;
    let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    let config: serde_json::Value = serde_yaml::from_slice(&bytes).unwrap();
    assert_eq!(config["mixed-port"], 9999);
    sqlx::query("UPDATE subscription_credentials SET revoked_at=now() WHERE id='sub'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        download(&state, "template=default-mihomo&platform=linux")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn renames_update_references_and_injection_targets_in_every_layer() {
    let mut document = seed("mihomo");
    document.platforms.entry("windows".into()).or_default().set = json!({"proxy-groups":[{"name":"SELECT","type":"select","proxies":[]},{"name":"PROXY","type":"select","proxies":["SELECT"]},{"name":"IPFake","type":"select","proxies":["PROXY","DIRECT"]}]});
    rename(&mut document, "mihomo", "group", "SELECT", "SelectNew").unwrap();
    assert_eq!(document.injections[0].group, "SelectNew");
    for platform in PLATFORMS {
        let config = document.effective(platform).unwrap();
        assert_eq!(config["proxy-groups"][0]["name"], "SelectNew");
        assert_eq!(config["proxy-groups"][1]["proxies"][0], "SelectNew");
    }
    assert!(rename(&mut document, "mihomo", "group", "SelectNew", "PROXY").is_err());
    rename(&mut document, "mihomo", "rule_set", "apple", "apple-new").unwrap();
    assert!(document.common["rule-providers"].get("apple-new").is_some());
    assert!(
        document.common["rules"]
            .as_array()
            .unwrap()
            .contains(&json!("RULE-SET,apple-new,DIRECT"))
    );
    ensure_valid(&validate("mihomo", &document).unwrap()).unwrap();
}
#[test]
fn empty_policy_and_actual_node_names_use_the_same_final_validation() {
    let mut document = seed("mihomo");
    document.injections[0].on_empty = EmptyPolicy::Error;
    let client = ClientInfo {
        platform: Platform::Linux,
        ..ClientInfo::default()
    };
    assert_eq!(
        super::render::render("mihomo", &document, &[], &client)
            .unwrap_err()
            .code,
        "template_empty_selection"
    );
    let node = super::super::build_node(
        "Same",
        "one-stable-id",
        "example.invalid",
        443,
        None,
        None,
        false,
        "password",
        None,
        None,
        None,
        None,
        &json!({}),
    );
    let other = super::super::build_node(
        "Same",
        "other-stable-id",
        "example.invalid",
        443,
        None,
        None,
        false,
        "password",
        None,
        None,
        None,
        None,
        &json!({}),
    );
    let result = super::render::render("mihomo", &document, &[node, other], &client).unwrap();
    assert_ne!(
        result["config"]["proxies"][0]["name"],
        result["config"]["proxies"][1]["name"]
    );
    assert_eq!(
        result["config"]["proxy-groups"][0]["proxies"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn platform_catalog_checks_nested_os_fields_and_routing_dependencies() {
    let mut document = seed("singbox");
    document.common["route"]["rules"] =
        json!([{"process_name":["example"],"action":"route","outbound":"直连"}]);
    assert!(
        validate("singbox", &document)
            .unwrap()
            .iter()
            .any(|d| d.severity == "error"
                && d.layer == "common"
                && d.path == "/route/rules/0/process_name")
    );
    let invalid = json!({"inbounds":[{"type":"tun","auto_redirect_input_mark":123}]});
    let diagnostics = platform_diagnostics("singbox", &invalid, "linux");
    assert!(diagnostics.iter().any(|d| d.message.contains("auto_route")));
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("auto_redirect"))
    );
    assert!(
        platform_diagnostics(
            "singbox",
            &json!({"dns":{"servers":[{"type":"https","routing_mark":2}]}}),
            "windows"
        )
        .iter()
        .any(|d| d.path == "/dns/servers/0/routing_mark")
    );
}
#[test]
fn known_fields_from_a_different_union_variant_are_errors_not_unknown_warnings() {
    let mut document = seed("singbox");
    let direct = document.common["outbounds"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|o| o["type"] == "direct")
        .unwrap();
    direct["outbounds"] = json!(null);
    assert!(
        validate("singbox", &document)
            .unwrap()
            .iter()
            .any(|d| d.severity == "error")
    );
}

#[tokio::test]
async fn import_keeps_one_configuration_and_removes_external_node_providers() {
    let analyzed=body(import(Json(Import{format:"singbox".into(),source:Some(json!({"outbounds":[{"type":"direct","tag":"Direct"}],"route":{"rules":[{"process_name":["example"],"action":"route","outbound":"Direct"}],"final":"Direct"}}).to_string()),package:None})).await.unwrap()).await;
    let document: Document = serde_json::from_value(analyzed["document"].clone()).unwrap();
    assert_eq!(
        document.effective("linux").unwrap()["route"]["rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        document.configuration("singbox", "android").unwrap().0["route"]["rules"],
        json!([])
    );
    ensure_valid(&validate("singbox", &document).unwrap()).unwrap();
    assert!(analyzed["platform_moves"].as_array().unwrap().is_empty());
    assert_eq!(document.schema_version, 2);
    assert!(document.platforms.is_empty());
    assert_eq!(
        document.common["route"]["rules"].as_array().unwrap().len(),
        1
    );
    let source = "proxy-providers:\n  External: {type: http, url: 'https://secret.invalid/token'}\nproxy-groups:\n - {name: Select, type: select, use: [External]}\nrules: ['MATCH,Select']\n";
    let analyzed = body(
        import(Json(Import {
            format: "mihomo".into(),
            source: Some(source.into()),
            package: None,
        }))
        .await
        .unwrap(),
    )
    .await;
    assert!(!analyzed.to_string().contains("secret.invalid"));
    assert_eq!(analyzed["removed_providers"], json!(["External"]));
    let document: Document = serde_json::from_value(analyzed["document"].clone()).unwrap();
    assert_eq!(document.injections[0].group, "Select");
    ensure_valid(&validate("mihomo", &document).unwrap()).unwrap();
}

#[tokio::test]
async fn unfinished_injection_draft_is_saved_but_cannot_be_published() {
    let state = fixture().await;
    let mut document = seed("mihomo");
    document.injections[0].group = String::new();
    let Json(saved) = save(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Save {
            expected_revision: 1,
            document,
        }),
    )
    .await
    .unwrap();
    assert_eq!(saved["revision"], 2);
    let error = publish(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Revision {
            expected_revision: 2,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
    let Json(metadata) = get(State(state.clone()), Path("default-mihomo".into()))
        .await
        .unwrap();
    assert_eq!(metadata["revision"], 2);
    assert_eq!(metadata["published_version"], 1);
    let saved = body(
        draft(State(state), Path("default-mihomo".into()))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(saved["document"]["injections"][0]["group"], "");
}
#[test]
fn mihomo_logical_rules_named_subrules_and_renames_are_validated_as_native_grammar() {
    let mut document = seed("mihomo");
    document.common["sub-rules"] =
        json!({"private":["IP-CIDR,192.168.0.0/16,DIRECT,src,no-resolve","MATCH,PROXY"]});
    document.common["rules"] = json!([
        "SUB-RULE,(NETWORK,tcp),private",
        "AND,((RULE-SET,apple),(DOMAIN-SUFFIX,example.test)),PROXY",
        "DOMAIN-REGEX,^(one,two|three)$,PROXY",
        "MATCH,PROXY"
    ]);
    ensure_valid(&validate("mihomo", &document).unwrap()).unwrap();
    rename(
        &mut document,
        "mihomo",
        "rule_set",
        "apple",
        "apple-renamed",
    )
    .unwrap();
    assert!(
        document.common["rules"][1]
            .as_str()
            .unwrap()
            .contains("RULE-SET,apple-renamed")
    );
    rename(&mut document, "mihomo", "group", "PROXY", "Policy").unwrap();
    assert_eq!(document.common["sub-rules"]["private"][1], "MATCH,Policy");
    assert_eq!(
        document.common["rules"][2],
        "DOMAIN-REGEX,^(one,two|three)$,Policy"
    );
    document.common["sub-rules"]["private"][0] = json!("SUB-RULE,(NETWORK,udp),loop");
    document.common["sub-rules"]["loop"] = json!(["SUB-RULE,(NETWORK,tcp),private"]);
    assert!(
        validate("mihomo", &document)
            .unwrap()
            .iter()
            .any(|d| d.path == "/sub-rules" && d.message.contains("cycle"))
    );
    document.common["rules"][0] = json!("NOT,((RULE-SET,missing)),DIRECT");
    assert!(
        validate("mihomo", &document)
            .unwrap()
            .iter()
            .any(|d| d.path == "/rules/0" && d.message.contains("missing rule provider"))
    );
}
#[test]
fn linux_external_namespace_is_preserved_without_claiming_os_resolution() {
    let mut document = seed("singbox");
    let tun = document.platforms.get_mut("linux").unwrap().set["inbounds"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|v| v["type"] == "tun")
        .unwrap();
    tun["netns"] = json!("/run/netns/external");
    let diagnostics = validate("singbox", &document).unwrap();
    ensure_valid(&diagnostics).unwrap();
    assert!(diagnostics.iter().any(|d| d.severity == "warning"
        && d.path.ends_with("/netns")
        && d.message.contains("cannot be resolved")));
}

#[test]
fn dns_tags_and_cross_outbound_dns_cycles_are_checked_with_reference_metadata() {
    let config = json!({"dns":{"servers":[{"type":"https","tag":"DNS","server":"1.1.1.1","detour":"Proxy"}]},"outbounds":[{"type":"direct","tag":"Proxy","domain_resolver":"DNS"}]});
    assert!(
        catalog::references("singbox", &config, "linux")
            .iter()
            .any(|d| d.message.contains("cyclic") && d.path.starts_with("/"))
    );
    let config =
        json!({"dns":{"servers":[{"type":"local","tag":"same"},{"type":"local","tag":"same"}]}});
    assert!(
        catalog::references("singbox", &config, "linux")
            .iter()
            .any(|d| d.path == "/dns/servers/1/tag" && d.message.contains("duplicate"))
    );
}

#[test]
fn catalog_records_native_defaults_validation_versions_and_editable_policy_scope() {
    let mihomo = catalog::catalog("mihomo");
    let root = &mihomo["schema"];
    assert_eq!(root["properties"]["allow-lan"]["default"], false);
    assert_eq!(root["properties"]["ipv6"]["default"], true);
    assert_eq!(
        root["properties"]["dns"]["properties"]["enhanced-mode"]["default"],
        "redir-host"
    );
    ensure_valid(&catalog::validate("mihomo", &root["default"], "common")).unwrap();
    assert!(!root.to_string().contains("processors-per-channel"));
    for format in ["mihomo", "singbox"] {
        let c = catalog::catalog(format);
        let version = c["kernel_version"].as_str().unwrap();
        for field in c["schema"]["properties"].as_object().unwrap().values() {
            assert_eq!(field["x-validation-version"], version);
            assert!(field["x-default-behavior"].is_string());
        }
    }
    let singbox = catalog::catalog("singbox");
    let kinds: Vec<_> = singbox["schema"]["$defs"]["Outbound"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["properties"]["type"]["const"].as_str())
        .collect();
    assert_eq!(kinds, vec!["direct", "selector", "urltest"]);
    let mut document = seed("mihomo");
    document.common["mixed-port"] = json!(65536);
    assert!(
        validate("mihomo", &document)
            .unwrap()
            .iter()
            .any(|d| d.severity == "error" && d.path == "/mixed-port")
    );
}

#[test]
fn native_source_normalizes_yaml_merges_and_json_comments_without_changing_values() {
    let config=parse("mihomo","defaults: &settings\n  type: select\n  proxies: [DIRECT]\nproxy-groups:\n - <<: *settings\n   name: Policy\nfuture: {flag: false, number: 0, entries: []}\n").unwrap();
    assert_eq!(
        config["proxy-groups"][0],
        json!({"name":"Policy","type":"select","proxies":["DIRECT"]})
    );
    assert_eq!(
        parse("mihomo", &serialize("mihomo", &config).unwrap()).unwrap(),
        config
    );
    let config=parse("singbox",r#"{
        // keep this URL and escaped quote intact
        "future": {"url":"https://example.invalid/a//b/*c*/", "text":"quoted \\\" value", "values":[false,0,null,],},
        /* source comments are normalized */ "outbounds":[{"type":"direct","tag":"Direct",},],
    }"#).unwrap();
    assert_eq!(config["future"]["url"], "https://example.invalid/a//b/*c*/");
    assert_eq!(config["future"]["values"], json!([false, 0, null]));
    assert_eq!(
        parse("singbox", &serialize("singbox", &config).unwrap()).unwrap(),
        config
    );
    assert!(parse("singbox", "{\"a\":1 /* unfinished").is_err());
    assert!(parse("singbox", "{\"a\":[1,,]}").is_err());
    assert!(parse("mihomo", "future: .nan").is_err());
    assert!(parse("mihomo", "future: {123: value}").is_err());
}
#[tokio::test]
async fn raw_import_package_detection_uses_the_server_parser_for_both_formats() {
    for format in ["mihomo", "singbox"] {
        let document = seed(format);
        let package =
            json!({"package_version":1,"name":"Roundtrip","format":format,"document":document});
        let source = format!("// exported package with an annotation\n{}", package);
        let analyzed = body(
            import(Json(Import {
                format: format.into(),
                source: Some(source),
                package: None,
            }))
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(
            analyzed["document"],
            serde_json::to_value(document).unwrap()
        );
    }
}

#[tokio::test]
async fn template_audit_records_success_and_failure_without_source_or_secret_values() {
    let state = fixture().await;
    let mut document = seed("mihomo");
    document.common["future"] = json!({"password":"must-never-be-audited"});
    let error = save(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Save {
            expected_revision: 0,
            document: document.clone(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    document.common["mixed-port"] = json!("invalid-port");
    let _ = save(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Save {
            expected_revision: 1,
            document,
        }),
    )
    .await
    .unwrap();
    let error = publish(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Revision {
            expected_revision: 2,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
    let rows = sqlx::query(
        "SELECT action,detail_json FROM audit_records WHERE entity_type='subscription_template'",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(
        |r| r.get::<String, _>("action") == "template.publish_failed"
            && r.get::<serde_json::Value, _>("detail_json")["result"] == "failure"
    ));
    assert!(
        rows.iter()
            .any(|r| r.get::<String, _>("action") == "template.draft_saved"
                && r.get::<serde_json::Value, _>("detail_json")["result"] == "success")
    );
    for row in rows {
        let detail = row.get::<serde_json::Value, _>("detail_json").to_string();
        assert!(
            !detail.contains("must-never-be-audited")
                && !detail.contains("mixed-port")
                && !detail.contains("invalid-port")
        );
    }
}

#[test]
fn mutual_tun_constraints_and_mihomo_os_limits_are_visible_without_import_repair() {
    let config = json!({"inbounds":[{"type":"tun","auto_route":true,"include_interface":["eth0"],"exclude_interface":["lo"]}]});
    assert!(
        platform_diagnostics("singbox", &config, "linux")
            .iter()
            .any(|d| d.message.starts_with("conflicts with"))
    );
    let mut document = Document {
        schema_version: 1,
        common: config,
        platforms: Default::default(),
        injections: vec![],
    };
    let _ = partition_import("singbox", &mut document).unwrap();
    assert!(
        platform_diagnostics("singbox", &document.effective("linux").unwrap(), "linux")
            .iter()
            .any(|d| d.message.starts_with("conflicts with")),
        "import must retain unresolved semantic conflicts rather than silently dropping predicates"
    );
    assert!(
        platform_diagnostics(
            "mihomo",
            &json!({"tun":{"auto-redirect":true,"auto-route":false}}),
            "linux"
        )
        .iter()
        .any(|d| d.message.contains("requires"))
    );
    assert!(
        platform_diagnostics("mihomo", &json!({"routing-mark":1}), "windows")
            .iter()
            .any(|d| d.severity == "error")
    );
}

#[test]
fn mihomo_preferred_injected_node_controls_initial_selection_without_reordering_other_members() {
    let mut document = seed("mihomo");
    document.common["proxy-groups"][0]["proxies"] = json!(["DIRECT"]);
    document.injections[0].preferred_prefix = "Preferred".into();
    let ordinary = super::super::build_node(
        "Alpha",
        "ordinary",
        "example.invalid",
        443,
        None,
        None,
        false,
        "fixture",
        None,
        None,
        None,
        None,
        &json!({}),
    );
    let preferred = super::super::build_node(
        "Preferred edge",
        "preferred",
        "example.invalid",
        443,
        None,
        None,
        false,
        "fixture",
        None,
        None,
        None,
        None,
        &json!({}),
    );
    let preferred_name = preferred.name.clone();
    let ordinary_name = ordinary.name.clone();
    let result = super::render::render(
        "mihomo",
        &document,
        &[ordinary, preferred],
        &ClientInfo {
            platform: Platform::Linux,
            ..ClientInfo::default()
        },
    )
    .unwrap();
    assert_eq!(
        result["config"]["proxy-groups"][0]["proxies"],
        json!([preferred_name, "DIRECT", ordinary_name])
    );
    assert_eq!(
        document.common["proxy-groups"][0]["proxies"],
        json!(["DIRECT"])
    );
}

#[test]
fn preview_explains_authorized_nodes_excluded_by_each_selector_condition() {
    let mut document = seed("mihomo");
    let rule = &mut document.injections[0];
    rule.ids = vec!["wanted".into()];
    rule.contains = "region".into();
    rule.prefix = "Preferred".into();
    rule.exclude = vec!["blocked".into()];
    let node = super::super::build_node(
        "blocked edge",
        "not-selected",
        "example.invalid",
        443,
        None,
        None,
        false,
        "fixture",
        None,
        None,
        None,
        None,
        &json!({}),
    );
    let output = super::render::render(
        "mihomo",
        &document,
        &[node],
        &ClientInfo {
            platform: Platform::Linux,
            ..ClientInfo::default()
        },
    )
    .unwrap();
    let reasons = output["node_selection"][0]["excluded"][0]["reasons"]
        .as_array()
        .unwrap();
    assert_eq!(
        reasons,
        &vec![
            json!("node_id_not_selected"),
            json!("name_contains_mismatch"),
            json!("name_prefix_mismatch"),
            json!("excluded_by_name")
        ]
    );
    assert_eq!(
        output["config"]["proxy-groups"][0]["proxies"],
        json!(["DIRECT"])
    );
}

#[test]
fn generated_mihomo_nodes_are_typed_without_unknown_field_warnings() {
    use super::super::formats::{SubscriptionFormat, SubscriptionRealm, Version};
    let client = ClientInfo {
        name: "mihomo",
        format: Some(SubscriptionFormat::Mihomo),
        version: Some(Version(1, 19, 31)),
        platform: Platform::Linux,
    };
    let node = super::super::build_node(
        "Typed dynamic node",
        "typed-node",
        "example.invalid",
        443,
        Some("443,5000-5100".into()),
        Some("example.invalid".into()),
        true,
        "fixture-password",
        Some("fixture-certificate".into()),
        Some("fixture-private-key".into()),
        Some("fixture-ech-config".into()),
        Some(SubscriptionRealm {
            server_url: "https://realm.example.invalid".into(),
            token: "fixture-realm-token".into(),
            realm_id: "fixture-realm".into(),
            stun_servers: Some(vec!["stun.example.invalid:3478".into()]),
            skip_cert_verify: Some(true),
        }),
        &json!({"bandwidth":{"up":"100 mbps","down":"200 mbps"},"obfs":{"type":"gecko","gecko":{"password":"fixture-obfs","minPacketSize":100,"maxPacketSize":1200}}}),
    );
    let result = super::render::render("mihomo", &seed("mihomo"), &[node], &client).unwrap();
    assert!(
        result["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| { !d["path"].as_str().unwrap_or("").starts_with("/proxies/") }),
        "Generated node fields must be recognized by the native catalog"
    );
    let mut config = result["config"].clone();
    // These fixtures must exercise the optional nested connection fields as well.
    for field in [
        "hop-interval",
        "certificate",
        "private-key",
        "ech-opts",
        "realm-opts",
        "handshake-timeout",
        "obfs",
    ] {
        assert!(
            config["proxies"][0].get(field).is_some(),
            "Missing fixture field: {field}"
        );
    }
    config["proxies"][0]["future-node-option"] = json!(true);
    config["proxies"][0]["ech-opts"]["future-ech-option"] = json!("preserved");
    let diagnostics = catalog::validate("mihomo", &config, "linux");
    for path in [
        "/proxies/0/future-node-option",
        "/proxies/0/ech-opts/future-ech-option",
    ] {
        assert!(diagnostics.iter().any(|d| d.path == path
            && d.severity == "warning"
            && d.message.contains("unknown field preserved")));
    }
    assert!(!diagnostics.iter().any(|d| d.severity == "error"));
    assert_eq!(config["proxies"][0]["future-node-option"], true);
    config["proxies"][0]["port"] = json!("invalid");
    assert!(
        catalog::validate("mihomo", &config, "linux")
            .iter()
            .any(|d| d.path == "/proxies/0/port" && d.severity == "error")
    );
}

#[test]
fn single_configuration_seeds_validate_and_render_without_overlays() {
    for format in ["mihomo", "singbox"] {
        let document = render::single_seed(format);
        assert_eq!(document.schema_version, 2);
        assert!(document.platforms.is_empty());
        let original = document.common.clone();
        let diagnostics = validate(format, &document).unwrap();
        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == "error")
            .map(|d| format!("{} {} {}", d.layer, d.path, d.message))
            .collect();
        assert!(errors.is_empty(), "{format}: {errors:?}");
        verify_rendering(format, &document).unwrap();
        assert_eq!(document.common, original);
    }
}

#[test]
fn single_configuration_projects_platform_fields_but_keeps_dependencies_and_types_strict() {
    let mut document = render::single_seed("mihomo");
    document.common["tproxy-port"] = json!(7893);
    let (windows, omitted) = document.configuration("mihomo", "windows").unwrap();
    assert!(windows.get("tproxy-port").is_none());
    assert!(windows.get("redir-port").is_none());
    assert!(omitted.iter().any(|item| item["path"] == "/tproxy-port"));
    assert_eq!(
        document.configuration("mihomo", "linux").unwrap().0["tproxy-port"],
        7893
    );
    assert_eq!(document.common["tproxy-port"], 7893);
    document.common["tproxy-port"] = json!("invalid");
    assert!(
        validate("mihomo", &document)
            .unwrap()
            .iter()
            .any(|d| d.severity == "error" && d.path == "/tproxy-port")
    );
    document
        .platforms
        .insert("windows".into(), Overlay::default());
    assert!(document.shape().is_err());
    let mut document = render::single_seed("singbox");
    let inbound = document.common["inbounds"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["type"] == "tun")
        .unwrap();
    inbound["auto_redirect"] = json!(true);
    inbound["auto_route"] = json!(false);
    inbound["iproute2_table_index"] = json!(1000);
    assert!(
        validate("singbox", &document)
            .unwrap()
            .iter()
            .any(|d| d.severity == "error" && d.message.starts_with("requires "))
    );
}

#[test]
fn platform_projection_removes_complete_array_units_in_numeric_order() {
    let source = json!({"inbounds": (0..13).map(|i| json!({"type":if i == 2 || i == 10 {"tproxy"} else {"mixed"}, "tag":format!("in-{i}")})).collect::<Vec<_>>(),
        "dns":{"rules":[{"process_name":["only-desktop"],"action":"route","server":"dns"},{"domain":["example.com"],"action":"route","server":"dns"}]}});
    let (config, _) = project_configuration("singbox", &source, "ios").unwrap();
    let tags: Vec<_> = config["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["tag"].as_str().unwrap())
        .collect();
    assert_eq!(tags.len(), 11);
    assert!(!tags.contains(&"in-2"));
    assert!(!tags.contains(&"in-10"));
    assert_eq!(config["dns"]["rules"].as_array().unwrap().len(), 1);
    assert_eq!(config["dns"]["rules"][0]["domain"], json!(["example.com"]));
    assert_eq!(source["inbounds"].as_array().unwrap().len(), 13);
}
