use super::*;

pub(super) fn source_value(input: Source) -> Result<Value, ApiError> {
    let config = match (&input.source, &input.config) {
        (Some(source), None) => parse(&input.format, source)?,
        (None, Some(config)) if config.is_object() => config.clone(),
        _ => {
            return Err(ApiError::bad_request(
                "supply either source or config object",
            ));
        }
    };
    Ok(json!({"source":serialize(&input.format,&config)?,"config":config}))
}

pub(super) fn import_value(input: Import) -> Result<Value, ApiError> {
    if !["mihomo", "singbox"].contains(&input.format.as_str()) {
        return Err(ApiError::bad_request("unsupported format"));
    }
    if input.source.is_some() && input.package.is_some() {
        return Err(ApiError::bad_request(
            "supply native source or a template package, not both",
        ));
    }
    let inferred = input
        .source
        .as_deref()
        .and_then(|source| super::super::native_source::jsonc(source).ok())
        .filter(|value| {
            value.get("package_version").is_some()
                && value.get("document").is_some()
                && value.get("format").is_some()
        });
    if let Some(package) = input.package.or(inferred) {
        if package["package_version"] != 1 || package["format"] != input.format {
            return Err(ApiError::bad_request("invalid template package or format"));
        }
        let document: Document = serde_json::from_value(package["document"].clone())
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        document.shape()?;
        return Ok(
            json!({"diagnostics":validate(&input.format,&document)?,"document":document,"removed_nodes":[],"reference_mapping":[]}),
        );
    }
    let mut common = parse(
        &input.format,
        input
            .source
            .as_deref()
            .ok_or_else(|| ApiError::bad_request("source or package is required"))?,
    )?;
    let mut removed = Vec::new();
    let mut mapping = Vec::new();
    let mut injections = Vec::new();
    let mut removed_providers = Vec::new();
    if input.format == "mihomo" {
        removed = common
            .as_object_mut()
            .unwrap()
            .remove("proxies")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        common["proxies"] = json!([]);
        if let Some(providers) = common
            .as_object_mut()
            .unwrap()
            .remove("proxy-providers")
            .and_then(|v| v.as_object().cloned())
        {
            removed_providers = providers.keys().cloned().collect();
        }
    } else if let Some(outbounds) = common["outbounds"].as_array_mut() {
        outbounds.retain(|v| {
            let keep = ["direct", "selector", "urltest", "block", "dns"]
                .contains(&v["type"].as_str().unwrap_or(""));
            if !keep {
                removed.push(v.clone());
            }
            keep
        });
    }
    if input.format == "singbox"
        && let Some(endpoints) = common
            .as_object_mut()
            .unwrap()
            .remove("endpoints")
            .and_then(|v| v.as_array().cloned())
    {
        removed.extend(endpoints);
    }
    let (groups_key, name_key, entries_key) = if input.format == "mihomo" {
        ("proxy-groups", "name", "proxies")
    } else {
        ("outbounds", "tag", "outbounds")
    };
    let names: Vec<String> = removed
        .iter()
        .filter_map(|n| n[name_key].as_str().map(str::to_owned))
        .collect();
    if let Some(groups) = common[groups_key].as_array_mut() {
        for group in groups {
            let mut provider_refs = Vec::new();
            if input.format == "mihomo"
                && let Some(map) = group.as_object_mut()
            {
                if let Some(uses) = map.remove("use").and_then(|v| v.as_array().cloned()) {
                    provider_refs.extend(uses.iter().filter_map(Value::as_str).map(str::to_owned));
                }
                for flag in [
                    "include-all",
                    "include-all-proxies",
                    "include-all-providers",
                ] {
                    if map.remove(flag) == Some(json!(true)) {
                        provider_refs.push(flag.into());
                    }
                }
                if !provider_refs.is_empty() {
                    map.entry(entries_key).or_insert(json!([]));
                }
            }
            let mut refs = Vec::new();
            if let Some(entries) = group.get_mut(entries_key).and_then(Value::as_array_mut) {
                refs = entries
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| names.iter().any(|n| n == s))
                    .map(str::to_owned)
                    .collect();
                entries.retain(|v| !v.as_str().is_some_and(|s| names.iter().any(|n| n == s)));
            }
            if !refs.is_empty() || !provider_refs.is_empty() {
                let name = group[name_key].as_str().unwrap_or("").to_string();
                mapping.push(json!({"group":name,"removed_references":refs,"removed_provider_references":provider_refs,"replacement":"authorized dynamic nodes"}));
                injections.push(Injection {
                    group: name,
                    ids: vec![],
                    contains: String::new(),
                    prefix: String::new(),
                    exclude: vec![],
                    descending: false,
                    preferred_prefix: String::new(),
                    on_empty: EmptyPolicy::Direct,
                });
            }
        }
    }
    let document = Document {
        schema_version: 2,
        common,
        platforms: Default::default(),
        injections,
    };
    let platform_moves: Vec<Value> = vec![];
    // Do not return credentials from removed nodes in import analysis.
    let removed: Vec<_> = removed
        .iter()
        .map(|n| json!({"name":n[name_key],"type":n["type"]}))
        .collect();
    Ok(
        json!({"diagnostics":validate(&input.format,&document)?,"document":document,"removed_nodes":removed,"reference_mapping":mapping,"platform_moves":platform_moves,"removed_providers":removed_providers}),
    )
}

pub(super) async fn validate_draft_value(
    state: &AppState,
    id: &str,
    input: Validate,
) -> Result<Value, ApiError> {
    let row = row(state, id).await?;
    let document = input
        .document
        .unwrap_or(decode(&state.secrets, &row.get::<String, _>("draft_enc"))?);
    Ok(json!({"diagnostics":validate(&row.get::<String,_>("format"),&document)?}))
}

pub(super) async fn preview_value(
    state: &AppState,
    id: &str,
    input: Preview,
) -> Result<Value, ApiError> {
    let current = row(state, id).await?;
    let format: String = current.get("format");
    let document = input.document.unwrap_or(decode(
        &state.secrets,
        &current.get::<String, _>("draft_enc"),
    )?);
    crate::api::subscriptions::validate_user(state, &input.user_id).await?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("platform", &input.platform)
        .finish();
    let platform = crate::api::subscriptions::formats::Platform::requested(Some(&query))?
        .ok_or_else(|| ApiError::bad_request("explicit preview platform required"))?;
    let client = crate::api::subscriptions::formats::ClientInfo::detect(&format!(
        "{}/{}",
        if format == "singbox" {
            "sing-box"
        } else {
            "mihomo"
        },
        input.kernel_version.as_deref().unwrap_or("")
    ));
    if input.kernel_version.is_some() && client.version.is_none() {
        return Err(ApiError::bad_request("invalid stable kernel version"));
    }
    let client = crate::api::subscriptions::formats::ClientInfo { platform, ..client };
    let eligible = crate::api::subscriptions::eligible_nodes(state, &input.user_id).await?;
    let nodes = crate::api::subscriptions::load_subscription_nodes_eligible(
        state,
        &input.user_id,
        Some(&eligible),
    )
    .await?;
    let mut output = render::render(&format, &document, &nodes, &client)?;
    if !input.reveal_secrets {
        redact(&mut output["config"]);
    }
    output["source"] = json!(serialize(&format, &output["config"])?);
    output["redacted"] = json!(!input.reveal_secrets);
    Ok(output)
}

pub(super) async fn refactor_value(
    state: &AppState,
    id: &str,
    mut input: Refactor,
) -> Result<Value, ApiError> {
    let current = row(state, id).await?;
    if current.get::<i64, _>("revision") != input.expected_revision {
        return Err(ApiError::conflict(
            "template changed; reload metadata before refactoring references",
        ));
    }
    let format: String = current.get("format");
    rename(
        &mut input.document,
        &format,
        &input.kind,
        &input.old_name,
        &input.new_name,
    )?;
    Ok(json!({"document":input.document,"diagnostics":validate(&format,&input.document)?}))
}
