use crate::error::ApiError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const PLATFORMS: [&str; 5] = ["linux", "macos", "windows", "android", "ios"];
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub schema_version: u32,
    pub common: Value,
    #[serde(default)]
    pub platforms: BTreeMap<String, Overlay>,
    #[serde(default)]
    pub injections: Vec<Injection>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    #[serde(default = "object")]
    pub set: Value,
    #[serde(default)]
    pub remove: Vec<String>,
}
fn object() -> Value {
    json!({})
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Injection {
    pub group: String,
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub contains: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub descending: bool,
    #[serde(default)]
    pub preferred_prefix: String,
    #[serde(default)]
    pub on_empty: EmptyPolicy,
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmptyPolicy {
    #[default]
    Direct,
    Error,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: String,
    pub layer: String,
    pub path: String,
    pub message: String,
}
pub fn diagnostic(
    severity: &str,
    layer: &str,
    path: &str,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        severity: severity.into(),
        layer: layer.into(),
        path: path.into(),
        message: message.into(),
    }
}
pub fn error(layer: &str, path: &str, message: impl Into<String>) -> Diagnostic {
    diagnostic("error", layer, path, message)
}
pub fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}
pub fn merge(base: &mut Value, overlay: &Value) {
    if let (Some(a), Some(b)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in b {
            if let Some(previous) = a.get_mut(key) {
                merge(previous, value);
            } else {
                a.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = overlay.clone();
    }
}
pub fn remove(config: &mut Value, path: &str) -> Result<(), ApiError> {
    let (parent, key) = path
        .rsplit_once('/')
        .filter(|(_, k)| !k.is_empty())
        .ok_or_else(|| ApiError::bad_request("removal paths must be non-root JSON pointers"))?;
    let key = key.replace("~1", "/").replace("~0", "~");
    if let Some(target) = config.pointer_mut(parent) {
        let map = target.as_object_mut().ok_or_else(|| {
            ApiError::bad_request("removal paths cannot address array items or scalar children")
        })?;
        map.remove(&key);
    }
    Ok(())
}
impl Document {
    pub fn shape(&self) -> Result<(), ApiError> {
        if ![1, 2].contains(&self.schema_version) || !self.common.is_object() {
            return Err(ApiError::bad_request(
                "schema_version must be 1 or 2; common must be an object",
            ));
        }
        if self.schema_version == 2 && !self.platforms.is_empty() {
            return Err(ApiError::bad_request(
                "single-configuration templates cannot contain platform overlays",
            ));
        }
        for (platform, overlay) in &self.platforms {
            if !PLATFORMS.contains(&platform.as_str()) || !overlay.set.is_object() {
                return Err(ApiError::bad_request(
                    "invalid platform or platform set is not an object",
                ));
            }
            let mut paths = BTreeSet::new();
            for path in &overlay.remove {
                if !path.starts_with('/')
                    || path.as_bytes().iter().enumerate().any(|(i, b)| {
                        *b == b'~' && !matches!(path.as_bytes().get(i + 1), Some(b'0' | b'1'))
                    })
                    || !paths.insert(path)
                {
                    return Err(ApiError::bad_request(
                        "invalid or duplicate removal pointer",
                    ));
                }
                let mut prefix = String::new();
                for part in path.trim_start_matches('/').split('/') {
                    prefix.push('/');
                    prefix.push_str(part);
                    if let Some(value) = overlay.set.pointer(&prefix)
                        && (!value.is_object() || prefix == *path)
                    {
                        return Err(ApiError::bad_request(format!(
                            "set/remove conflict at {path}"
                        )));
                    }
                }
                let mut copy = self.common.clone();
                merge(&mut copy, &overlay.set);
                remove(&mut copy, path)?;
            }
        }
        Ok(())
    }
    pub fn configuration(
        &self,
        format: &str,
        platform: &str,
    ) -> Result<(Value, Vec<Value>), ApiError> {
        let config = self.effective(platform)?;
        if self.schema_version == 2 {
            project_configuration(format, &config, platform)
        } else {
            Ok((config, vec![]))
        }
    }
    pub fn effective(&self, platform: &str) -> Result<Value, ApiError> {
        self.shape()?;
        if !PLATFORMS.contains(&platform) {
            return Err(ApiError::bad_request("select a target platform"));
        }
        let mut config = self.common.clone();
        if let Some(overlay) = self.platforms.get(platform) {
            merge(&mut config, &overlay.set);
            for path in &overlay.remove {
                remove(&mut config, path)?;
            }
        }
        Ok(config)
    }
}
pub fn parse(format: &str, source: &str) -> Result<Value, ApiError> {
    let value = match format {
        "mihomo" => super::native_source::yaml(source)?,
        "singbox" => super::native_source::jsonc(source)?,
        _ => return Err(ApiError::bad_request("format must be mihomo or singbox")),
    };
    if !value.is_object() {
        return Err(ApiError::bad_request("configuration must be an object"));
    }
    Ok(value)
}
pub fn serialize(format: &str, config: &Value) -> Result<String, ApiError> {
    match format {
        "mihomo" => serde_yaml::to_string(config).map_err(|_| ApiError::internal()),
        "singbox" => serde_json::to_string_pretty(config).map_err(|_| ApiError::internal()),
        _ => Err(ApiError::bad_request("format must be mihomo or singbox")),
    }
}

/// These restrictions preserve the previously supported client capability model.
/// Restrictions beyond this baseline are recorded in the versioned field catalog.
pub fn platform_diagnostics(format: &str, config: &Value, platform: &str) -> Vec<Diagnostic> {
    super::catalog::platform_diagnostics(format, config, platform)
}

pub fn validate(format: &str, document: &Document) -> Result<Vec<Diagnostic>, ApiError> {
    document.shape()?;
    let mut diagnostics = Vec::new();
    if document.schema_version == 2 {
        diagnostics.extend(super::catalog::validate(format, &document.common, "common"));
        diagnostics.extend(super::catalog::references(
            format,
            &document.common,
            "common",
        ));
        diagnostics.extend(references(
            format,
            &document.common,
            "common",
            &document.injections,
            false,
        ));
    }
    // Legacy documents retain their original overlay validation semantics.
    for platform in PLATFORMS {
        if document.schema_version == 1 {
            diagnostics.extend(
                platform_diagnostics(format, &document.common, platform)
                    .into_iter()
                    .map(|mut d| {
                        d.message = format!(
                            "{}: move this field to an appropriate platform overlay",
                            d.message
                        );
                        d.layer = "common".into();
                        d
                    }),
            );
        }
        let (config, _) = document.configuration(format, platform)?;
        diagnostics.extend(platform_diagnostics(format, &config, platform));
        diagnostics.extend(super::catalog::validate(format, &config, platform));
        diagnostics.extend(super::catalog::references(format, &config, platform));
        diagnostics.extend(references(
            format,
            &config,
            platform,
            &document.injections,
            false,
        ));
    }
    Ok(diagnostics)
}

pub fn references(
    format: &str,
    config: &Value,
    layer: &str,
    injections: &[Injection],
    rendered: bool,
) -> Vec<Diagnostic> {
    let mut d = Vec::new();
    let mihomo = format == "mihomo";
    let (groups_key, name_key, entries_key) = if mihomo {
        ("proxy-groups", "name", "proxies")
    } else {
        ("outbounds", "tag", "outbounds")
    };
    let groups = config[groups_key].as_array().cloned().unwrap_or_default();
    let mut names: BTreeSet<String> = if mihomo {
        ["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        BTreeSet::new()
    };
    if rendered && mihomo {
        for node in config["proxies"].as_array().into_iter().flatten() {
            if let Some(name) = node["name"].as_str() {
                names.insert(name.into());
            }
        }
    }
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (i, group) in groups.iter().enumerate() {
        let path = format!("/{groups_key}/{i}/{name_key}");
        let Some(name) = group[name_key].as_str().filter(|s| !s.is_empty()) else {
            d.push(error(layer, &path, "a unique name is required"));
            continue;
        };
        if (!rendered && name.starts_with("hx:")) || !names.insert(name.into()) {
            d.push(error(
                layer,
                &path,
                "duplicate name or reserved hx: namespace",
            ));
        }
    }
    for (i, group) in groups.iter().enumerate() {
        let name = group[name_key].as_str().unwrap_or("");
        let path = format!("/{groups_key}/{i}/{entries_key}");
        if let Some(entries) = group[entries_key].as_array() {
            if entries.is_empty() && !injections.iter().any(|r| r.group == name) {
                d.push(error(layer, &path, "group requires at least one member"));
            }
            let mut refs = Vec::new();
            for (j, entry) in entries.iter().enumerate() {
                if let Some(target) = entry.as_str() {
                    refs.push(target.to_owned());
                    if !names.contains(target) {
                        d.push(error(
                            layer,
                            &format!("{path}/{j}"),
                            format!("unknown outbound {target}"),
                        ));
                    }
                }
            }
            if let Some(default) = group["default"].as_str()
                && !entries.iter().any(|v| v == default)
            {
                d.push(error(
                    layer,
                    &format!("/{groups_key}/{i}/default"),
                    "default must be a member of this group",
                ));
            }
            edges.insert(name.into(), refs);
        }
        if !mihomo
            && !rendered
            && !["selector", "urltest", "direct", "block", "dns"]
                .contains(&group["type"].as_str().unwrap_or(""))
        {
            d.push(error(
                layer,
                &format!("/{groups_key}/{i}/type"),
                "manual proxy nodes are forbidden; connections are dynamically injected",
            ));
        }
    }
    fn cycle(
        node: &str,
        edges: &BTreeMap<String, Vec<String>>,
        visiting: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> bool {
        if done.contains(node) {
            return false;
        }
        if !visiting.insert(node.into()) {
            return true;
        }
        for target in edges.get(node).into_iter().flatten() {
            if cycle(target, edges, visiting, done) {
                return true;
            }
        }
        visiting.remove(node);
        done.insert(node.into());
        false
    }
    let mut done = BTreeSet::new();
    for name in edges.keys() {
        if cycle(name, &edges, &mut BTreeSet::new(), &mut done) {
            d.push(error(
                layer,
                &format!("/{groups_key}"),
                format!("outbound cycle involving {name}"),
            ));
            break;
        }
    }
    let mut injection_targets = BTreeSet::new();
    for (i, injection) in injections.iter().enumerate() {
        if !injection_targets.insert(&injection.group) {
            d.push(error(
                layer,
                &format!("/injections/{i}/group"),
                "injection targets must be unique",
            ));
        }
        if !groups
            .iter()
            .any(|g| g[name_key] == injection.group && g[entries_key].is_array())
        {
            d.push(error(
                layer,
                &format!("/injections/{i}/group"),
                format!("missing injection target {}", injection.group),
            ));
        }
    }
    if mihomo {
        if !rendered && config["proxies"].as_array().is_some_and(|v| !v.is_empty()) {
            d.push(error(layer, "/proxies", "manual nodes are forbidden"));
        }
        d.extend(super::mihomo_rules::validate(config, &names, layer));
    } else {
        if config["endpoints"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
        {
            d.push(error(
                layer,
                "/endpoints",
                "manual endpoint nodes are forbidden; use authorized dynamic Hysteria2 nodes",
            ));
        }
        let sets: BTreeSet<&str> = config["route"]["rule_set"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|v| {
                v["tag"].as_str().into_iter().chain(
                    v["tag"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
            })
            .collect();
        let dns: BTreeSet<&str> = config["dns"]["servers"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|v| {
                v["tag"].as_str().into_iter().chain(
                    v["tag"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
            })
            .collect();
        fn walk(
            value: &Value,
            path: &str,
            names: &BTreeSet<String>,
            sets: &BTreeSet<&str>,
            dns: &BTreeSet<&str>,
            layer: &str,
            d: &mut Vec<Diagnostic>,
        ) {
            match value {
                Value::Object(map) => {
                    for (key, value) in map {
                        let p = format!("{path}/{}", escape(key));
                        let targets: Vec<&str> = value
                            .as_str()
                            .into_iter()
                            .chain(
                                value
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str),
                            )
                            .collect();
                        for target in targets {
                            let valid = match key.as_str() {
                                "outbound" | "detour" | "download_detour" => names.contains(target),
                                "rule_set" if !path.is_empty() => sets.contains(target),
                                "server" if path.starts_with("/dns/rules/") => dns.contains(target),
                                "final" if path == "/route" => names.contains(target),
                                "final" if path == "/dns" => dns.contains(target),
                                _ => true,
                            };
                            if !valid {
                                d.push(error(layer, &p, format!("unresolved reference {target}")));
                            }
                        }
                        walk(value, &p, names, sets, dns, layer, d);
                    }
                }
                Value::Array(items) => {
                    for (i, v) in items.iter().enumerate() {
                        walk(v, &format!("{path}/{i}"), names, sets, dns, layer, d);
                    }
                }
                _ => (),
            }
        }
        walk(config, "", &names, &sets, &dns, layer, &mut d);
    }
    d
}
pub fn ensure_valid(diagnostics: &[Diagnostic]) -> Result<(), ApiError> {
    if diagnostics.iter().any(|d| d.severity == "error") {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "template_invalid",
            serde_json::to_string(diagnostics).unwrap_or_default(),
        ));
    }
    Ok(())
}

pub fn rename(
    document: &mut Document,
    format: &str,
    kind: &str,
    old: &str,
    new: &str,
) -> Result<(), ApiError> {
    if old.is_empty()
        || new.is_empty()
        || new.starts_with("hx:")
        || !["group", "rule_set"].contains(&kind)
    {
        return Err(ApiError::bad_request("invalid reference rename"));
    }
    let group_key = if format == "mihomo" {
        "proxy-groups"
    } else {
        "outbounds"
    };
    let name_key = if format == "mihomo" { "name" } else { "tag" };
    fn definition_exists(
        config: &Value,
        format: &str,
        kind: &str,
        key: &str,
        name_key: &str,
        name: &str,
    ) -> bool {
        if kind == "group" {
            config[key]
                .as_array()
                .into_iter()
                .flatten()
                .any(|g| g[name_key] == name)
        } else if format == "mihomo" {
            config["rule-providers"].get(name).is_some()
        } else {
            config["route"]["rule_set"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|g| {
                    g["tag"] == name
                        || g["tag"]
                            .as_array()
                            .is_some_and(|tags| tags.iter().any(|t| t == name))
                })
        }
    }
    let layers =
        std::iter::once(&document.common).chain(document.platforms.values().map(|o| &o.set));
    let mut exists = false;
    for config in layers {
        exists |= definition_exists(config, format, kind, group_key, name_key, old);
        if definition_exists(config, format, kind, group_key, name_key, new) {
            return Err(ApiError::bad_request(
                "the replacement name already exists in a configuration layer",
            ));
        }
    }
    if !exists {
        return Err(ApiError::bad_request("reference definition was not found"));
    }
    fn walk(value: &mut Value, path: &str, format: &str, kind: &str, old: &str, new: &str) {
        match value {
            Value::Object(map) => {
                if format == "mihomo"
                    && kind == "rule_set"
                    && path == "/rule-providers"
                    && let Some(v) = map.remove(old)
                {
                    map.insert(new.into(), v);
                }
                for (key, value) in map {
                    let reference = if kind == "group" {
                        if format == "mihomo" {
                            (key == "name" && path.starts_with("/proxy-groups/"))
                                || ["proxies", "proxy", "dialer-proxy"].contains(&key.as_str())
                        } else {
                            (key == "tag" && path.starts_with("/outbounds/"))
                                || [
                                    "outbounds",
                                    "default",
                                    "outbound",
                                    "detour",
                                    "download_detour",
                                ]
                                .contains(&key.as_str())
                                || (key == "final" && path == "/route")
                        }
                    } else {
                        key == "rule_set"
                            || (format == "singbox"
                                && key == "tag"
                                && path.starts_with("/route/rule_set/"))
                    };
                    if reference {
                        if value == old {
                            *value = json!(new);
                        } else if let Some(values) = value.as_array_mut() {
                            for value in values {
                                if value == old {
                                    *value = json!(new);
                                }
                            }
                        }
                    }
                    walk(
                        value,
                        &format!("{path}/{}", escape(key)),
                        format,
                        kind,
                        old,
                        new,
                    );
                }
            }
            Value::Array(items) => {
                for (i, value) in items.iter_mut().enumerate() {
                    walk(value, &format!("{path}/{i}"), format, kind, old, new);
                }
            }
            _ => (),
        }
    }
    if format == "mihomo" {
        super::mihomo_rules::rename(&mut document.common, kind, old, new);
    }
    walk(&mut document.common, "", format, kind, old, new);
    for overlay in document.platforms.values_mut() {
        if format == "mihomo" {
            super::mihomo_rules::rename(&mut overlay.set, kind, old, new);
        }
        walk(&mut overlay.set, "", format, kind, old, new);
        if format == "mihomo" && kind == "rule_set" {
            for path in &mut overlay.remove {
                let prefix = format!("/rule-providers/{}", escape(old));
                if *path == prefix || path.starts_with(&(prefix.clone() + "/")) {
                    *path = format!("/rule-providers/{}{}", escape(new), &path[prefix.len()..]);
                }
            }
        }
    }
    if kind == "group" {
        for injection in &mut document.injections {
            if injection.group == old {
                injection.group = new.into();
            }
        }
    }
    document.shape()
}

/// Produce a target configuration without changing the single editable source.
/// Platform-specific listener types and match conditions remove their complete unit.
pub fn project_configuration(
    format: &str,
    source: &Value,
    platform: &str,
) -> Result<(Value, Vec<Value>), ApiError> {
    let mut moves = Vec::new();
    let mut config = source.clone();
    let mut removals = BTreeSet::new();
    for d in platform_diagnostics(format, &config, platform) {
        if d.severity != "error"
            || (d.message.starts_with("requires ") || d.message.starts_with("conflicts with "))
        {
            continue;
        }
        let mut path = d.path.clone();
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() >= 5 && ["route", "dns"].contains(&parts[1]) && parts[2] == "rules" {
            path = format!("/{}/rules/{}", parts[1], parts[3]);
        } else if parts.len() == 4 && parts[1] == "inbounds" && parts[3] == "type" {
            path = format!("/inbounds/{}", parts[2]);
        } else if parts.len() == 4 && parts[1] == "listeners" && parts[3] == "type" {
            path = format!("/listeners/{}", parts[2]);
        }
        moves.push(
            json!({"platform":platform,"path":d.path,"relocated_unit":path,"reason":d.message}),
        );
        removals.insert(path);
    }
    let mut removals: Vec<_> = removals.into_iter().collect();
    removals.sort_by(|a, b| {
        let aa: Vec<_> = a.split('/').collect();
        let bb: Vec<_> = b.split('/').collect();
        for (a, b) in aa.iter().zip(&bb) {
            let order = match (a.parse::<usize>(), b.parse::<usize>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => a.cmp(b),
            };
            if !order.is_eq() {
                return order;
            }
        }
        aa.len().cmp(&bb.len())
    });
    for path in removals.into_iter().rev() {
        let (parent, key) = path
            .rsplit_once('/')
            .ok_or_else(|| ApiError::bad_request("invalid import relocation"))?;
        if let Some(target) = config.pointer_mut(parent) {
            if let Some(items) = target.as_array_mut() {
                if let Ok(i) = key.parse::<usize>()
                    && i < items.len()
                {
                    items.remove(i);
                }
            } else if let Some(map) = target.as_object_mut() {
                map.remove(&key.replace("~1", "/").replace("~0", "~"));
            }
        }
    }
    Ok((config, moves))
}

/// Legacy partitioning retained to exercise version 1 compatibility in tests.
#[cfg(test)]
pub fn partition_import(format: &str, document: &mut Document) -> Result<Vec<Value>, ApiError> {
    let mut configs = BTreeMap::new();
    let mut moves = Vec::new();
    for platform in PLATFORMS {
        let (config, omitted) = project_configuration(format, &document.common, platform)?;
        configs.insert(platform.to_owned(), config);
        moves.extend(omitted);
    }
    fn common(values: &[Option<&Value>]) -> Option<Value> {
        let first = values.first().copied().flatten()?;
        if values.iter().all(|v| *v == Some(first)) {
            return Some(first.clone());
        }
        if values.iter().all(|v| v.is_some_and(Value::is_object)) {
            let mut map = serde_json::Map::new();
            for key in first.as_object().unwrap().keys() {
                if let Some(value) = common(
                    &values
                        .iter()
                        .map(|v| v.and_then(|v| v.get(key)))
                        .collect::<Vec<_>>(),
                ) {
                    map.insert(key.clone(), value);
                }
            }
            return Some(Value::Object(map));
        }
        None
    }
    let portable = common(&configs.values().map(Some).collect::<Vec<_>>()).unwrap_or(json!({}));
    fn delta(base: &Value, target: &Value) -> Value {
        if let (Some(base), Some(target)) = (base.as_object(), target.as_object()) {
            let mut result = serde_json::Map::new();
            for (key, value) in target {
                if base.get(key) != Some(value) {
                    let next = base
                        .get(key)
                        .map(|b| delta(b, value))
                        .unwrap_or_else(|| value.clone());
                    result.insert(key.clone(), next);
                }
            }
            Value::Object(result)
        } else {
            target.clone()
        }
    }
    document.common = portable;
    for (platform, config) in configs {
        let set = delta(&document.common, &config);
        if set.as_object().is_some_and(|o| !o.is_empty()) {
            document.platforms.insert(
                platform,
                Overlay {
                    set,
                    remove: vec![],
                },
            );
        }
    }
    document.shape()?;
    Ok(moves)
}
