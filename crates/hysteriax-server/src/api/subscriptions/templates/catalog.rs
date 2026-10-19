//! Versioned upstream field metadata shared by validation and the Mac form renderer.
use super::document::{Diagnostic, diagnostic, error, escape};
use serde_json::{Value, json};
use std::sync::LazyLock;
static MIHOMO: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("mihomo.schema.json")).unwrap());
static SINGBOX: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("singbox.schema.json")).unwrap());
pub fn schema(format: &str) -> &'static Value {
    if format == "mihomo" {
        &MIHOMO
    } else {
        &SINGBOX
    }
}
static CAPABILITIES: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("platform-capabilities.json")).unwrap());
fn field_metadata(value: &mut Value, version: &str) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if ["type", "properties", "$ref", "oneOf", "anyOf", "allOf"]
        .iter()
        .any(|key| map.contains_key(*key))
    {
        map.insert("x-validation-version".into(), json!(version));
        let behavior = if map.contains_key("default") {
            "literal"
        } else {
            "native_when_unset"
        };
        map.insert("x-default-behavior".into(), json!(behavior));
    }
    for key in ["properties", "$defs"] {
        if let Some(fields) = map.get_mut(key).and_then(Value::as_object_mut) {
            for field in fields.values_mut() {
                field_metadata(field, version);
            }
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(field) = map.get_mut(key) {
            field_metadata(field, version);
        }
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(fields) = map.get_mut(key).and_then(Value::as_array_mut) {
            for field in fields {
                field_metadata(field, version);
            }
        }
    }
}
pub fn catalog(format: &str) -> Value {
    let mut editable = schema(format).clone();
    field_metadata(
        &mut editable,
        if format == "mihomo" {
            "1.19.31"
        } else {
            "1.14.2"
        },
    );
    if format == "singbox"
        && let Some(variants) = editable
            .pointer_mut("/$defs/Outbound/oneOf")
            .and_then(Value::as_array_mut)
    {
        variants.retain(|v| {
            v["properties"]["type"]["const"]
                .as_str()
                .is_some_and(|kind| ["direct", "selector", "urltest"].contains(&kind))
        });
    }
    json!({"version":1,"format":format,"kernel_version":if format=="mihomo" {"1.19.31"} else {"1.14.2"},"schema":editable,
        "platforms":["linux","macos","windows","android","ios"],
        "limitations":"Structure and documented platform checks do not prove runtime compatibility. Unknown fields and graphical-client requirements remain explicitly unverified.",
        "platform_rules":CAPABILITIES[format]})
}
fn matches(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", tail)) => {
            matches(tail, path) || (!path.is_empty() && matches(pattern, &path[1..]))
        }
        Some((part, tail)) => {
            !path.is_empty() && (*part == "*" || *part == path[0]) && matches(tail, &path[1..])
        }
    }
}
pub fn platform_diagnostics(format: &str, config: &Value, platform: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    fn active(value: &Value) -> bool {
        match value {
            Value::Null | Value::Bool(false) => false,
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        }
    }
    fn walk(
        config: &Value,
        value: &Value,
        path: &str,
        rule: &Value,
        platform: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let pattern = rule["path"].as_str().unwrap_or("");
        if matches(
            &pattern.split('/').collect::<Vec<_>>(),
            &path.split('/').collect::<Vec<_>>(),
        ) && rule.get("value").is_none_or(|expected| expected == value)
        {
            let allowed = rule["platforms"]
                .as_array()
                .is_some_and(|items| items.iter().any(|p| p == platform));
            if !allowed {
                out.push(error(
                    platform,
                    path,
                    rule["message"]
                        .as_str()
                        .unwrap_or("unsupported platform field"),
                ));
            } else {
                if active(value) {
                    for required in rule["requires"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                        if config.pointer(&format!("{parent}/{required}"))
                            != Some(&Value::Bool(true))
                        {
                            out.push(error(
                                platform,
                                path,
                                format!("requires {parent}/{required}=true"),
                            ));
                        }
                    }
                }
                if active(value) {
                    for conflict in rule["conflicts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                        if config
                            .pointer(&format!("{parent}/{conflict}"))
                            .is_some_and(active)
                        {
                            out.push(error(
                                platform,
                                path,
                                format!("conflicts with {parent}/{conflict}"),
                            ));
                        }
                    }
                }
                if let Some(requirement) = rule["client_requirement"].as_str() {
                    out.push(diagnostic(
                        "warning",
                        platform,
                        path,
                        format!("requires {requirement}; not verified by platform selection"),
                    ));
                }
            }
        }
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    walk(
                        config,
                        v,
                        &format!("{path}/{}", escape(k)),
                        rule,
                        platform,
                        out,
                    );
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    walk(config, v, &format!("{path}/{i}"), rule, platform, out);
                }
            }
            _ => (),
        }
    }
    for rule in CAPABILITIES[format].as_array().into_iter().flatten() {
        walk(config, config, "", rule, platform, &mut diagnostics);
    }
    diagnostics
}
pub fn validate(format: &str, config: &Value, layer: &str) -> Vec<Diagnostic> {
    let format_id = if format == "mihomo" {
        hysteriax_template_validator::Format::Mihomo
    } else {
        hysteriax_template_validator::Format::Singbox
    };
    let mut diagnostics: Vec<_> = hysteriax_template_validator::error_paths(format_id, config)
        .into_iter()
        .map(|path| error(layer, &path, "value does not satisfy the versioned field schema (type, enumeration, range, or required fields)"))
        .collect();
    warnings(
        schema(format),
        schema(format),
        config,
        "",
        layer,
        &mut diagnostics,
        0,
    );
    diagnostics
}
fn warnings(
    root: &Value,
    schema: &Value,
    value: &Value,
    path: &str,
    layer: &str,
    d: &mut Vec<Diagnostic>,
    depth: usize,
) {
    if depth > 100 {
        return;
    }
    if let Some(reference) = schema["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|r| root.pointer(r))
    {
        warnings(root, reference, value, path, layer, d, depth + 1);
        return;
    }
    if schema["x-unverified"] == true {
        d.push(diagnostic(
            "warning",
            layer,
            path,
            "field type is not verified by the current catalog",
        ));
        return;
    }
    fn properties(root: &Value, s: &Value, depth: usize) -> serde_json::Map<String, Value> {
        if depth > 50 {
            return Default::default();
        }
        if let Some(target) = s["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|r| root.pointer(r))
        {
            return properties(root, target, depth + 1);
        }
        let mut result = s["properties"].as_object().cloned().unwrap_or_default();
        for key in ["allOf", "anyOf", "oneOf"] {
            for part in s[key].as_array().into_iter().flatten() {
                result.extend(properties(root, part, depth + 1));
            }
        }
        result
    }
    if let Some(map) = value.as_object() {
        let props = properties(root, schema, 0);
        for (key, value) in map {
            let p = format!("{path}/{}", escape(key));
            if let Some(field) = props.get(key) {
                warnings(root, field, value, &p, layer, d, depth + 1);
            } else if schema["additionalProperties"].is_object() {
                warnings(
                    root,
                    &schema["additionalProperties"],
                    value,
                    &p,
                    layer,
                    d,
                    depth + 1,
                );
            } else {
                d.push(diagnostic(
                    "warning",
                    layer,
                    &p,
                    "unknown field preserved; platform compatibility is unverified",
                ));
            }
        }
    } else if let Some(items) = value.as_array() {
        if let Some(item_schema) = schema.get("items") {
            for (i, item) in items.iter().enumerate() {
                warnings(
                    root,
                    item_schema,
                    item,
                    &format!("{path}/{i}"),
                    layer,
                    d,
                    depth + 1,
                );
            }
        } else {
            for union in ["anyOf", "oneOf"] {
                if let Some(parts) = schema[union].as_array()
                    && let Some(array) = parts.iter().find(|s| s["type"] == "array")
                {
                    warnings(root, array, value, path, layer, d, depth + 1);
                    break;
                }
            }
        }
    }
}

/// Resolve reference metadata across unions without interpreting hostnames as outbound names.
pub fn references(format: &str, config: &Value, layer: &str) -> Vec<Diagnostic> {
    use std::collections::{BTreeMap, BTreeSet};
    fn expanded(root: &Value, s: &Value, depth: usize) -> Value {
        if depth > 50 {
            return json!({});
        }
        if let Some(target) = s["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|r| root.pointer(r))
        {
            return expanded(root, target, depth + 1);
        }
        let mut result = s.clone();
        for key in ["allOf", "anyOf", "oneOf"] {
            for part in s[key].as_array().into_iter().flatten() {
                let p = expanded(root, part, depth + 1);
                for (k, v) in p.as_object().into_iter().flatten() {
                    if k == "properties" {
                        let mut props = result[k].as_object().cloned().unwrap_or_default();
                        props.extend(v.as_object().cloned().unwrap_or_default());
                        result[k] = json!(props);
                    } else if ["x-tag-reference", "items", "additionalProperties"]
                        .contains(&k.as_str())
                    {
                        result[k] = v.clone();
                    }
                }
            }
        }
        result
    }
    struct Context<'a> {
        root: &'a Value,
        layer: &'a str,
        targets: BTreeMap<&'static str, BTreeSet<String>>,
        owners: Vec<(String, Vec<String>)>,
        diagnostics: Vec<Diagnostic>,
        edges: BTreeMap<String, Vec<(String, String)>>,
    }
    impl Context<'_> {
        fn walk(&mut self, definition: &Value, value: &Value, path: &str, depth: usize) {
            if depth > 100 {
                return;
            }
            let definition = expanded(self.root, definition, 0);
            if let Some(kind) = definition["x-tag-reference"].as_str() {
                for tag in value.as_str().into_iter().chain(
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                ) {
                    if tag.is_empty() {
                        continue;
                    }
                    if self
                        .targets
                        .get(kind)
                        .is_some_and(|tags| tags.contains(tag))
                    {
                        if let Some((_, owners)) = self
                            .owners
                            .iter()
                            .find(|(prefix, _)| path.starts_with(&(prefix.clone() + "/")))
                        {
                            for owner in owners {
                                self.edges
                                    .entry(owner.clone())
                                    .or_default()
                                    .push((format!("{kind}:{tag}"), path.into()));
                            }
                        }
                    } else if kind == "network_namespace" {
                        self.diagnostics.push(diagnostic("warning",self.layer,path,"external operating-system namespace name/path is preserved and cannot be resolved by the template validator"));
                    } else {
                        self.diagnostics.push(error(
                            self.layer,
                            path,
                            format!("unresolved {kind} reference {tag}"),
                        ));
                    }
                }
            }
            if let Some(map) = value.as_object() {
                for (key, value) in map {
                    if let Some(field) = definition["properties"].get(key) {
                        self.walk(field, value, &format!("{path}/{}", escape(key)), depth + 1);
                    } else if definition["additionalProperties"].is_object() {
                        self.walk(
                            &definition["additionalProperties"],
                            value,
                            &format!("{path}/{}", escape(key)),
                            depth + 1,
                        );
                    }
                }
            } else if let Some(items) = value.as_array() {
                for (i, item) in items.iter().enumerate() {
                    self.walk(
                        &definition["items"],
                        item,
                        &format!("{path}/{i}"),
                        depth + 1,
                    );
                }
            }
        }
    }
    let root = schema(format);
    let mut context = Context {
        root,
        layer,
        targets: BTreeMap::new(),
        owners: Vec::new(),
        diagnostics: Vec::new(),
        edges: BTreeMap::new(),
    };
    for (kind, pointer) in [
        ("outbound", "/outbounds"),
        ("inbound", "/inbounds"),
        ("dns_server", "/dns/servers"),
        ("rule_set", "/route/rule_set"),
        ("certificate_provider", "/certificate_providers"),
        ("http_client", "/http_clients"),
        ("network_namespace", "/network_namespaces"),
    ] {
        let mut tags = BTreeSet::new();
        for (i, item) in config
            .pointer(pointer)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let mut owners = Vec::new();
            for tag in item["tag"].as_str().into_iter().chain(
                item["tag"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            ) {
                if tag.is_empty() {
                    continue;
                }
                if !tags.insert(tag.to_owned()) {
                    context.diagnostics.push(error(
                        layer,
                        &format!("{pointer}/{i}/tag"),
                        format!("duplicate {kind} tag {tag}"),
                    ));
                }
                owners.push(format!("{kind}:{tag}"));
            }
            context.owners.push((format!("{pointer}/{i}"), owners));
        }
        if kind == "outbound" {
            for endpoint in config["endpoints"].as_array().into_iter().flatten() {
                if let Some(tag) = endpoint["tag"].as_str() {
                    tags.insert(tag.into());
                }
            }
        }
        context.targets.insert(kind, tags);
    }
    if format == "mihomo" {
        let mut outbounds: BTreeSet<String> =
            ["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"]
                .into_iter()
                .map(str::to_owned)
                .collect();
        for key in ["proxy-groups", "proxies"] {
            outbounds.extend(
                config[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|g| g["name"].as_str().map(str::to_owned)),
            );
        }
        context.targets.insert("outbound", outbounds);
        context.targets.insert(
            "rule_set",
            config["rule-providers"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys().cloned())
                .collect(),
        );
        context.targets.insert(
            "sub_rule",
            config["sub-rules"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys().cloned())
                .collect(),
        );
    }
    context.walk(root, config, "", 0);
    fn cycle(
        node: &str,
        edges: &BTreeMap<String, Vec<(String, String)>>,
        visiting: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Option<String> {
        if done.contains(node) {
            return None;
        }
        visiting.insert(node.into());
        for (target, path) in edges.get(node).into_iter().flatten() {
            if visiting.contains(target) {
                return Some(path.clone());
            }
            if let Some(path) = cycle(target, edges, visiting, done) {
                return Some(path);
            }
        }
        visiting.remove(node);
        done.insert(node.into());
        None
    }
    let mut done = BTreeSet::new();
    for node in context.edges.keys() {
        if let Some(path) = cycle(node, &context.edges, &mut BTreeSet::new(), &mut done) {
            context.diagnostics.push(error(
                layer,
                &path,
                "cyclic configuration reference dependency",
            ));
            break;
        }
    }
    context.diagnostics
}
