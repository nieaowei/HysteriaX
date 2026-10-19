use super::super::formats::{ClientInfo, SubscriptionFormat, SubscriptionNode};
use super::document::*;
use crate::error::ApiError;
use serde_json::{Value, json};

fn selection_reasons(rule: &Injection, node: &SubscriptionNode) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !rule.ids.is_empty() && !rule.ids.contains(&node.id) {
        reasons.push("node_id_not_selected");
    }
    if !node.display_name.contains(&rule.contains) {
        reasons.push("name_contains_mismatch");
    }
    if !rule.prefix.is_empty() && !node.display_name.starts_with(&rule.prefix) {
        reasons.push("name_prefix_mismatch");
    }
    if rule
        .exclude
        .iter()
        .any(|value| node.display_name.contains(value))
    {
        reasons.push("excluded_by_name");
    }
    reasons
}
pub(super) fn render(
    format: &str,
    document: &Document,
    nodes: &[SubscriptionNode],
    client: &ClientInfo,
) -> Result<Value, ApiError> {
    let platform = match client.platform {
        super::super::formats::Platform::Linux => "linux",
        super::super::formats::Platform::Macos => "macos",
        super::super::formats::Platform::Windows => "windows",
        super::super::formats::Platform::Android => "android",
        super::super::formats::Platform::Ios => "ios",
        _ => return Err(ApiError::bad_request("select a target platform")),
    };
    let kind = if format == "mihomo" {
        SubscriptionFormat::Mihomo
    } else {
        SubscriptionFormat::Singbox
    };
    if format == "singbox"
        && client.format == Some(kind)
        && client
            .version
            .is_some_and(|v| v < super::super::formats::Version(1, 14, 0))
    {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "subscription_incompatible",
            "sing-box 1.14.0 or newer is required",
        ));
    }
    let mut compatible = Vec::new();
    let mut filtered = Vec::new();
    for node in nodes {
        if let Some(reason) = node.incompatible(kind, client) {
            filtered.push(json!({"id":node.id,"reason":reason}));
        } else {
            compatible.push(node);
        }
    }
    if !nodes.is_empty() && compatible.is_empty() {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "subscription_incompatible",
            "all authorized nodes are incompatible",
        ));
    }
    let mut config = document.effective(platform)?;
    let direct_name = config["outbounds"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["type"] == "direct")
        .and_then(|o| o["tag"].as_str())
        .unwrap_or("hx:direct")
        .to_owned();
    let (groups_key, name_key, entries_key, direct) = if format == "mihomo" {
        ("proxy-groups", "name", "proxies", "DIRECT")
    } else {
        ("outbounds", "tag", "outbounds", direct_name.as_str())
    };
    let mut diagnostics = platform_diagnostics(format, &config, platform);
    diagnostics.extend(super::catalog::validate(format, &config, platform));
    diagnostics.extend(references(
        format,
        &config,
        platform,
        &document.injections,
        false,
    ));
    ensure_valid(&diagnostics)?;
    let mut selections = Vec::new();
    for rule in &document.injections {
        let mut selected: Vec<_> = compatible
            .iter()
            .copied()
            .filter(|node| selection_reasons(rule, node).is_empty())
            .collect();
        selected.sort_by(|a, b| a.display_name.cmp(&b.display_name).then(a.id.cmp(&b.id)));
        if rule.descending {
            selected.reverse();
        }
        let names: Vec<Value> = selected.iter().map(|node| json!(node.name)).collect();
        let groups = config[groups_key]
            .as_array_mut()
            .ok_or_else(|| ApiError::bad_request("group array is missing"))?;
        let group = groups
            .iter_mut()
            .find(|g| g[name_key] == rule.group)
            .ok_or_else(|| ApiError::bad_request("injection target missing"))?;
        let entries = group[entries_key]
            .as_array_mut()
            .ok_or_else(|| ApiError::bad_request("group has no member array"))?;
        entries.extend(names.clone());
        // Mihomo selects the first member; sing-box has an explicit default tag.
        // Promote only the preferred injected node, preserving all other member order.
        if format == "mihomo"
            && !rule.preferred_prefix.is_empty()
            && let Some(preferred) = selected
                .iter()
                .find(|node| node.display_name.starts_with(&rule.preferred_prefix))
            && let Some(index) = entries.iter().position(|entry| entry == &preferred.name)
        {
            let entry = entries.remove(index);
            entries.insert(0, entry);
        }
        if names.is_empty() {
            if rule.on_empty == EmptyPolicy::Error {
                return Err(ApiError::new(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "template_empty_selection",
                    format!("/injections/{}: no matching nodes", rule.group),
                ));
            }
            if !entries.iter().any(|v| v == direct) {
                entries.push(json!(direct));
            }
        }
        if !rule.preferred_prefix.is_empty() && format == "singbox" {
            group["default"] = selected
                .iter()
                .find(|node| node.display_name.starts_with(&rule.preferred_prefix))
                .map(|n| json!(n.name))
                .or_else(|| names.first().cloned())
                .unwrap_or(json!(direct));
        }
        selections.push(json!({"group":rule.group,"selected_ids":selected.iter().map(|n|&n.id).collect::<Vec<_>>(),"excluded_ids":compatible.iter().filter(|n|!selected.iter().any(|s|s.id==n.id)).map(|n|&n.id).collect::<Vec<_>>(),"excluded":compatible.iter().filter_map(|node|{let reasons=selection_reasons(rule,node);if reasons.is_empty(){None}else{Some(json!({"id":node.id,"reasons":reasons}))}}).collect::<Vec<_>>()}));
    }
    if format == "mihomo" {
        config["proxies"] = json!(compatible.iter().map(|n| n.to_mihomo()).collect::<Vec<_>>());
    } else {
        let groups = config["outbounds"]
            .as_array_mut()
            .ok_or_else(|| ApiError::bad_request("outbounds must be an array"))?;
        if !groups.iter().any(|g| g["tag"] == direct) {
            groups.push(json!({"type":"direct","tag":direct}));
        }
        groups.extend(compatible.iter().map(|node| node.singbox()));
    }
    let mut final_diagnostics = references(format, &config, platform, &[], true);
    final_diagnostics.extend(platform_diagnostics(format, &config, platform));
    final_diagnostics.extend(super::catalog::validate(format, &config, platform));
    final_diagnostics.extend(super::catalog::references(format, &config, platform));
    ensure_valid(&final_diagnostics)?;
    diagnostics.extend(final_diagnostics);
    let mut unique = std::collections::BTreeSet::new();
    diagnostics.retain(|d| {
        unique.insert((
            d.severity.clone(),
            d.layer.clone(),
            d.path.clone(),
            d.message.clone(),
        ))
    });
    let mut origins = Vec::new();
    fn walk(
        value: &Value,
        path: &str,
        platform: &str,
        overlay: Option<&Overlay>,
        format: &str,
        out: &mut Vec<Value>,
    ) {
        if let Some(map) = value.as_object() {
            for (k, v) in map {
                walk(
                    v,
                    &format!("{path}/{}", escape(k)),
                    platform,
                    overlay,
                    format,
                    out,
                );
            }
        } else {
            let source = if overlay.is_some_and(|o| o.set.pointer(path).is_some()) {
                platform
            } else {
                "common"
            };
            let source = if path == "/proxies" {
                "dynamic".to_owned()
            } else if format == "singbox" && path == "/outbounds" {
                format!("{source} + dynamic")
            } else {
                source.to_owned()
            };
            out.push(json!({"path":path,"source":source}));
        }
    }
    walk(
        &config,
        "",
        platform,
        document.platforms.get(platform),
        format,
        &mut origins,
    );
    Ok(
        json!({"config":config,"diagnostics":diagnostics,"origins":origins,"differences":document.platforms.get(platform),"node_selection":selections,"filtered_nodes":filtered}),
    )
}

pub fn seed(format: &str) -> Document {
    let source = if format == "mihomo" {
        include_str!("../mihomo-template.yaml")
    } else {
        include_str!("../singbox-template.json")
    };
    let mut common = parse(format, source).expect("valid built-in seed");
    let mut platforms = std::collections::BTreeMap::new();
    let mut injections = Vec::new();
    if format == "mihomo" {
        let redir = common
            .as_object_mut()
            .unwrap()
            .remove("redir-port")
            .unwrap();
        for platform in ["linux", "macos"] {
            platforms.insert(
                platform.into(),
                Overlay {
                    set: json!({"redir-port":redir}),
                    remove: vec![],
                },
            );
        }
        let target = common["proxy-groups"][0]["name"]
            .as_str()
            .unwrap()
            .to_string();
        injections.push(Injection {
            group: target,
            ids: vec![],
            contains: String::new(),
            prefix: String::new(),
            exclude: vec![],
            descending: false,
            preferred_prefix: String::new(),
            on_empty: EmptyPolicy::Direct,
        });
    } else {
        let original = common
            .as_object_mut()
            .unwrap()
            .remove("inbounds")
            .unwrap_or(json!([]));
        for platform in PLATFORMS {
            let mut inbounds = original.clone();
            if let Some(items) = inbounds.as_array_mut() {
                items.retain(|v| match v["type"].as_str() {
                    Some("tproxy") => platform == "linux",
                    Some("redirect") => ["linux", "macos"].contains(&platform),
                    _ => true,
                });
                for inbound in items {
                    if inbound["type"] == "tun" {
                        let map = inbound.as_object_mut().unwrap();
                        if platform != "linux" {
                            map.remove("auto_redirect");
                        }
                        if ["macos", "android", "ios"].contains(&platform) {
                            map.remove("interface_name");
                        }
                        if ["android", "ios"].contains(&platform) {
                            map.remove("auto_route");
                        }
                    }
                }
            }
            platforms.insert(
                platform.into(),
                Overlay {
                    set: json!({"inbounds":inbounds}),
                    remove: vec![],
                },
            );
        }
        if let Some(groups) = common["outbounds"].as_array_mut() {
            for group in groups {
                let Some(entries) = group.get_mut("outbounds").and_then(Value::as_array_mut) else {
                    continue;
                };
                let placeholders: Vec<_> = entries
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|s| s.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")))
                    .map(str::to_owned)
                    .collect();
                entries.retain(|entry| !entry.as_str().is_some_and(|s| s.starts_with("{{")));
                if let Some(selector) = placeholders.first() {
                    let preferred = group["default"]
                        .as_str()
                        .and_then(|s| s.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")))
                        .unwrap_or("")
                        .to_owned();
                    group.as_object_mut().unwrap().remove("default");
                    injections.push(Injection {
                        group: group["tag"].as_str().unwrap().into(),
                        ids: vec![],
                        contains: if selector == "nodes" {
                            String::new()
                        } else {
                            selector.clone()
                        },
                        prefix: String::new(),
                        exclude: vec!["新加坡".into()],
                        descending: false,
                        preferred_prefix: preferred,
                        on_empty: EmptyPolicy::Direct,
                    });
                }
            }
        }
    }
    Document {
        schema_version: 1,
        common,
        platforms,
        injections,
    }
}
