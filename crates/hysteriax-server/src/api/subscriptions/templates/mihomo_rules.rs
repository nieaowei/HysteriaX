//! Structural Mihomo rule grammar, including logical predicates and named sub-rule references.
//! This follows the pinned ParseRulePayload contract; regex execution and Geo assets remain kernel concerns.
use super::document::{Diagnostic, error, escape};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Rule {
    kind: String,
    payload: String,
    target: String,
    params: Vec<String>,
}
impl Rule {
    fn parse(raw: &str, with_target: bool) -> Result<Self, String> {
        let parts: Vec<_> = raw.split(',').map(str::trim).collect();
        let kind = parts[0].to_uppercase();
        if !super::catalog::schema("mihomo")["x-rule-types"]
            .as_array()
            .is_some_and(|types| types.iter().any(|t| t == &kind))
        {
            return Err("unsupported Mihomo rule type".into());
        }
        if kind == "MATCH" {
            if !with_target {
                return Err("MATCH cannot be a logical predicate".into());
            }
            return Ok(Self {
                kind,
                payload: String::new(),
                target: parts.get(1).unwrap_or(&"").to_string(),
                params: parts.iter().skip(2).map(|s| s.to_string()).collect(),
            });
        }
        let trailing_target = [
            "AND",
            "OR",
            "NOT",
            "SUB-RULE",
            "DOMAIN-REGEX",
            "PROCESS-NAME-REGEX",
            "PROCESS-PATH-REGEX",
        ]
        .contains(&kind.as_str());
        if parts.len() < if with_target { 3 } else { 2 } {
            return Err("rule requires a payload and target".into());
        }
        let (payload, target, params) = if trailing_target {
            let end = parts.len() - usize::from(with_target);
            (
                parts[1..end].join(","),
                if with_target {
                    parts[end].to_owned()
                } else {
                    String::new()
                },
                vec![],
            )
        } else {
            (
                parts[1].to_owned(),
                if with_target {
                    parts[2].to_owned()
                } else {
                    String::new()
                },
                parts[(if with_target { 3 } else { 2 })..]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            )
        };
        if payload.is_empty() {
            return Err("rule payload must be nonempty".into());
        }
        Ok(Self {
            kind,
            payload,
            target,
            params,
        })
    }
    fn native(&self, with_target: bool) -> String {
        let mut parts = vec![self.kind.clone()];
        if self.kind != "MATCH" {
            parts.push(self.payload.clone());
        }
        if with_target {
            parts.push(self.target.clone());
        }
        parts.extend(self.params.iter().cloned());
        parts.join(",")
    }
}
fn split_conditions(value: &str) -> Result<Vec<&str>, String> {
    let inner = value
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or("logical payload requires parentheses")?;
    let mut depth = 0_i32;
    let mut start = 0;
    let mut result = Vec::new();
    for (i, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return Err("unbalanced logical parentheses".into());
                }
            }
            ',' if depth == 0 => {
                result.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => (),
        }
    }
    if depth != 0 {
        return Err("unbalanced logical parentheses".into());
    }
    if !inner.is_empty() {
        result.push(inner[start..].trim());
    }
    Ok(result)
}
fn predicate(raw: &str, providers: &Value, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err("logical rule nesting limit exceeded".into());
    }
    let content = raw
        .trim()
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or("each logical predicate requires parentheses")?;
    let rule = Rule::parse(content, false)?;
    if ["MATCH", "SUB-RULE"].contains(&rule.kind.as_str()) {
        return Err("MATCH and SUB-RULE cannot be logical predicates".into());
    }
    validate_payload(&rule, providers, depth + 1)
}
fn validate_payload(rule: &Rule, providers: &Value, depth: usize) -> Result<(), String> {
    match rule.kind.as_str() {
        "RULE-SET" => {
            if providers.get(&rule.payload).is_none() {
                return Err(format!("missing rule provider {}", rule.payload));
            }
        }
        "AND" | "OR" | "NOT" => {
            let children = split_conditions(&rule.payload)?;
            if children.is_empty() || (rule.kind == "NOT" && children.len() != 1) {
                return Err("logical expression has an invalid predicate count".into());
            }
            for child in children {
                predicate(child, providers, depth + 1)?;
            }
        }
        "SUB-RULE" => predicate(&rule.payload, providers, depth + 1)?,
        "IP-CIDR" | "IP-CIDR6" | "SRC-IP-CIDR" => {
            let (address, bits) = rule
                .payload
                .split_once('/')
                .ok_or("CIDR rule requires an address and prefix length")?;
            let address: std::net::IpAddr = address.parse().map_err(|_| "invalid CIDR address")?;
            let bits: u8 = bits.parse().map_err(|_| "invalid CIDR prefix length")?;
            if bits > if address.is_ipv4() { 32 } else { 128 } {
                return Err("CIDR prefix length is out of range".into());
            }
        }
        "NETWORK" if !["tcp", "udp"].contains(&rule.payload.to_ascii_lowercase().as_str()) => {
            return Err("NETWORK must be TCP or UDP".into());
        }
        _ => (),
    }
    Ok(())
}
pub fn validate(config: &Value, names: &BTreeSet<String>, layer: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut arrays = vec![("/rules".to_owned(), &config["rules"], None)];
    for (name, rules) in config["sub-rules"].as_object().into_iter().flatten() {
        arrays.push((format!("/sub-rules/{}", escape(name)), rules, Some(name)));
    }
    for (path, rules, source) in arrays {
        for (i, value) in rules.as_array().into_iter().flatten().enumerate() {
            let Some(raw) = value.as_str() else {
                continue;
            };
            let p = format!("{path}/{i}");
            let rule = match Rule::parse(raw, true) {
                Ok(rule) => rule,
                Err(message) => {
                    diagnostics.push(error(layer, &p, message));
                    continue;
                }
            };
            if let Err(message) = validate_payload(&rule, &config["rule-providers"], 0) {
                diagnostics.push(error(layer, &p, message));
            }
            if rule.kind == "SUB-RULE" {
                if config["sub-rules"].get(&rule.target).is_none() {
                    diagnostics.push(error(
                        layer,
                        &p,
                        format!("missing named sub-rule {}", rule.target),
                    ));
                }
                if let Some(source) = source {
                    edges
                        .entry(source.into())
                        .or_default()
                        .push(rule.target.clone());
                }
            } else if !names.contains(&rule.target) {
                diagnostics.push(error(
                    layer,
                    &p,
                    format!("unknown rule action {}", rule.target),
                ));
            }
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
        for next in edges.get(node).into_iter().flatten() {
            if cycle(next, edges, visiting, done) {
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
            diagnostics.push(error(
                layer,
                "/sub-rules",
                format!("named sub-rule cycle involving {name}"),
            ));
            break;
        }
    }
    diagnostics
}
fn rename_payload(rule: &mut Rule, old: &str, new: &str) -> Result<(), String> {
    if rule.kind == "RULE-SET" && rule.payload == old {
        rule.payload = new.into();
    }
    if ["AND", "OR", "NOT"].contains(&rule.kind.as_str()) {
        let mut parts = Vec::new();
        for child in split_conditions(&rule.payload)? {
            let content = child
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .ok_or("invalid predicate")?;
            let mut nested = Rule::parse(content, false)?;
            rename_payload(&mut nested, old, new)?;
            parts.push(format!("({})", nested.native(false)));
        }
        rule.payload = format!("({})", parts.join(","));
    } else if rule.kind == "SUB-RULE" {
        let content = rule
            .payload
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .ok_or("invalid predicate")?;
        let mut nested = Rule::parse(content, false)?;
        rename_payload(&mut nested, old, new)?;
        rule.payload = format!("({})", nested.native(false));
    }
    Ok(())
}
pub fn rename(config: &mut Value, kind: &str, old: &str, new: &str) {
    fn rewrite(array: &mut Value, kind: &str, old: &str, new: &str) {
        for value in array.as_array_mut().into_iter().flatten() {
            let Some(raw) = value.as_str() else {
                continue;
            };
            if let Ok(mut rule) = Rule::parse(raw, true) {
                if kind == "group" && rule.kind != "SUB-RULE" && rule.target == old {
                    rule.target = new.into();
                }
                if kind == "rule_set" && rename_payload(&mut rule, old, new).is_err() {
                    continue;
                }
                *value = json!(rule.native(true));
            }
        }
    }
    if let Some(rules) = config.get_mut("rules") {
        rewrite(rules, kind, old, new);
    }
    if let Some(subrules) = config.get_mut("sub-rules").and_then(Value::as_object_mut) {
        for rules in subrules.values_mut() {
            rewrite(rules, kind, old, new);
        }
    }
}
