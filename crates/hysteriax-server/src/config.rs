use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use url::{Host, Url};

pub const DEFAULT_REALM_STUN_SERVERS: &[&str] = &[
    "stun.nextcloud.com:3478",
    "stun.sip.us:3478",
    "global.stun.twilio.com:3478",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealmConnection {
    pub server_url: String,
    pub token: String,
    pub realm_id: String,
    pub stun_servers: Vec<String>,
    pub insecure: bool,
}

const SERVER_FIELDS: &[&str] = &[
    "realm",
    "obfs",
    "tls",
    "acme",
    "ech",
    "quic",
    "mimic",
    "bandwidth",
    "ignoreClientBandwidth",
    "congestion",
    "speedTest",
    "disableUDP",
    "udpIdleTimeout",
    "resolver",
    "sniff",
    "acl",
    "outbounds",
    "masquerade",
];

const DEFAULT_STREAM_RECEIVE_WINDOW: u64 = 8_388_608;
const DEFAULT_CONNECTION_RECEIVE_WINDOW: u64 = 20_971_520;
const MIN_QUIC_RECEIVE_WINDOW: u64 = 16_384;
const MIN_BANDWIDTH_BYTES_PER_SECOND: u64 = 65_536;

pub fn validate_server_options(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("server config must be a JSON object")?;
    for key in object.keys() {
        if key == "auth" || key == "trafficStats" {
            bail!("{key} is managed by HysteriaX and cannot be overridden");
        }
        if !SERVER_FIELDS.contains(&key.as_str()) {
            bail!("unsupported Hysteria 2 v2.12.3 server field: {key}");
        }
    }
    if object.contains_key("tls") && object.contains_key("acme") {
        bail!("tls and acme are mutually exclusive");
    }
    if let Some(realm) = object.get("realm") {
        validate_realm(realm)?;
        if realm_connection(value)?.is_some() {
            if !object.contains_key("tls") && !object.contains_key("acme") {
                bail!("Realm mode requires tls or acme certificate configuration");
            }
            bail!(
                "Hysteria Realms remain disabled: Mihomo v1.19.31 did not complete the live rendezvous connection check"
            );
        }
    }
    if let Some(ech) = object.get("ech") {
        validate_ech(ech)?;
        if !object.contains_key("tls") && !object.contains_key("acme") {
            bail!("ech requires tls or acme certificate configuration");
        }
    }
    if let Some(mimic) = object.get("mimic") {
        let mimic = mimic.as_object().context("mimic must be an object")?;
        allow_fields(
            mimic,
            "mimic",
            &["enabled", "interface", "xdpMode", "path", "extraArgs"],
        )?;
        if let Some(enabled) = mimic.get("enabled") {
            require_bool(enabled, "mimic.enabled")?;
            if enabled.as_bool() == Some(true) {
                bail!("Mimic is disabled in the initial Mihomo compatibility profile");
            }
        }
        for field in ["interface", "xdpMode", "path"] {
            optional_string(mimic, field, &format!("mimic.{field}"))?;
        }
        if let Some(args) = mimic.get("extraArgs")
            && args
                .as_array()
                .is_none_or(|values| values.iter().any(|value| !value.is_string()))
        {
            bail!("mimic.extraArgs must be a list of strings");
        }
    }
    if let Some(obfs) = object.get("obfs").and_then(Value::as_object) {
        let kind = obfs.get("type").and_then(Value::as_str).unwrap_or("");
        if !kind.is_empty() && !matches!(kind, "salamander" | "gecko") {
            bail!("obfs.type must be salamander or gecko");
        }
    }
    if let Some(tls) = object.get("tls") {
        validate_tls(tls)?;
    }
    if let Some(acme) = object.get("acme") {
        validate_acme(acme)?;
    }
    if let Some(obfs) = object.get("obfs") {
        validate_obfs(obfs)?;
    }
    if let Some(bandwidth) = object.get("bandwidth") {
        validate_bandwidth_config(bandwidth)?;
    }
    if let Some(congestion) = object.get("congestion") {
        validate_congestion(congestion)?;
    }
    if let Some(timeout) = object.get("udpIdleTimeout") {
        let timeout = timeout
            .as_str()
            .context("udpIdleTimeout must be a duration string")?;
        validate_duration_range(timeout, "udpIdleTimeout", 2_000.0, 600_000.0)?;
    }
    if let Some(quic) = object.get("quic") {
        validate_quic(quic)?;
    }
    if let Some(resolver) = object.get("resolver") {
        validate_resolver(resolver)?;
    }
    if let Some(sniff) = object.get("sniff") {
        validate_sniff(sniff)?;
    }
    if let Some(acl) = object.get("acl") {
        validate_acl(acl)?;
    }
    if let Some(outbounds) = object.get("outbounds") {
        validate_outbounds(outbounds)?;
    }
    if let Some(masquerade) = object.get("masquerade") {
        validate_masquerade(masquerade)?;
    }
    for flag in ["speedTest", "disableUDP", "ignoreClientBandwidth"] {
        if let Some(value) = object.get(flag) {
            require_bool(value, flag)?;
        }
    }
    Ok(())
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str, field: &str) -> Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{field} is required"))
}

fn require_bool(value: &Value, field: &str) -> Result<()> {
    if value.is_boolean() {
        Ok(())
    } else {
        bail!("{field} must be a boolean")
    }
}

fn allow_fields(object: &Map<String, Value>, section: &str, fields: &[&str]) -> Result<()> {
    if let Some(field) = object
        .keys()
        .find(|field| !fields.contains(&field.as_str()))
    {
        bail!("unsupported {section} field: {field}");
    }
    Ok(())
}

fn optional_string(object: &Map<String, Value>, key: &str, field: &str) -> Result<Option<String>> {
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("{field} must be a string"))
        })
        .transpose()
}

fn optional_duration(object: &Map<String, Value>, key: &str, field: &str) -> Result<()> {
    if let Some(value) = optional_string(object, key, field)? {
        validate_duration(&value, field)?;
    }
    Ok(())
}

fn validate_tls(value: &Value) -> Result<()> {
    let tls = value.as_object().context("tls must be an object")?;
    allow_fields(tls, "tls", &["cert", "key", "sniGuard", "clientCA"])?;
    required_string(tls, "cert", "tls.cert")?;
    required_string(tls, "key", "tls.key")?;
    if let Some(guard) = optional_string(tls, "sniGuard", "tls.sniGuard")?.as_deref()
        && !matches!(guard, "strict" | "disable" | "dns-san")
    {
        bail!("tls.sniGuard must be strict, disable, or dns-san");
    }
    optional_string(tls, "clientCA", "tls.clientCA")?;
    Ok(())
}

fn validate_ech(value: &Value) -> Result<()> {
    let ech = value.as_object().context("ech must be an object")?;
    allow_fields(ech, "ech", &["keyPath"])?;
    let key_path = required_string(ech, "keyPath", "ech.keyPath")?;
    let resource_reference = key_path
        .strip_prefix("resource://")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'));
    let resolved_resource_path = key_path
        .strip_prefix("/etc/hysteriax/resources/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'));
    if !resource_reference && !resolved_resource_path {
        bail!("ech.keyPath must reference an uploaded ech_key resource");
    }
    Ok(())
}

fn validate_realm(value: &Value) -> Result<()> {
    let realm = value.as_object().context("realm must be an object")?;
    allow_fields(
        realm,
        "realm",
        &[
            "connection",
            "stunServers",
            "stunTimeout",
            "punchTimeout",
            "heartbeatInterval",
            "insecure",
            "ipMode",
            "portMapping",
        ],
    )?;
    if let Some(connection) = realm.get("connection") {
        parse_realm_connection(connection)?;
    }
    if let Some(servers) = realm.get("stunServers") {
        let servers = servers
            .as_array()
            .context("realm.stunServers must be a list of host:port strings")?;
        if servers.is_empty()
            || servers
                .iter()
                .any(|server| server.as_str().is_none_or(|value| value.trim().is_empty()))
        {
            bail!("realm.stunServers must contain at least one host:port string");
        }
    }
    for field in ["stunTimeout", "punchTimeout", "heartbeatInterval"] {
        if let Some(value) = realm.get(field) {
            let value = value
                .as_str()
                .with_context(|| format!("realm.{field} must be a duration string"))?;
            validate_duration(value, &format!("realm.{field}"))?;
        }
    }
    if let Some(insecure) = realm.get("insecure") {
        require_bool(insecure, "realm.insecure")?;
    }
    if let Some(ip_mode) = optional_string(realm, "ipMode", "realm.ipMode")?
        && !matches!(ip_mode.as_str(), "dual" | "v4" | "v6")
    {
        bail!("realm.ipMode must be dual, v4, or v6");
    }
    if let Some(mapping) = realm.get("portMapping") {
        let mapping = mapping
            .as_object()
            .context("realm.portMapping must be an object")?;
        allow_fields(
            mapping,
            "realm.portMapping",
            &["enabled", "timeout", "lifetime"],
        )?;
        if let Some(enabled) = mapping.get("enabled") {
            require_bool(enabled, "realm.portMapping.enabled")?;
        }
        for field in ["timeout", "lifetime"] {
            if let Some(value) = mapping.get(field) {
                let value = value.as_str().with_context(|| {
                    format!("realm.portMapping.{field} must be a duration string")
                })?;
                validate_duration(value, &format!("realm.portMapping.{field}"))?;
            }
        }
    }
    Ok(())
}

pub fn realm_connection(options: &Value) -> Result<Option<RealmConnection>> {
    let Some(realm) = options.get("realm").and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(connection) = realm.get("connection") else {
        return Ok(None);
    };
    let mut connection = parse_realm_connection(connection)?;
    connection.stun_servers = realm
        .get("stunServers")
        .and_then(Value::as_array)
        .map(|servers| {
            servers
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .filter(|servers: &Vec<String>| !servers.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_REALM_STUN_SERVERS
                .iter()
                .map(|server| (*server).to_owned())
                .collect()
        });
    connection.insecure = realm
        .get("insecure")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Some(connection))
}

fn parse_realm_connection(value: &Value) -> Result<RealmConnection> {
    let connection = value
        .as_object()
        .context("realm.connection must be an object")?;
    allow_fields(
        connection,
        "realm.connection",
        &["serverURL", "token", "realmID"],
    )?;
    let server_url = required_string(connection, "serverURL", "realm.connection.serverURL")?;
    let token = required_string(connection, "token", "realm.connection.token")?;
    let realm_id = required_string(connection, "realmID", "realm.connection.realmID")?;
    if token.len() > 256
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
    {
        bail!("realm.connection.token must use URL-safe ASCII characters and be at most 256 bytes");
    }
    let id_bytes = realm_id.as_bytes();
    if !(1..=64).contains(&id_bytes.len())
        || !id_bytes[0].is_ascii_alphanumeric()
        || !id_bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!(
            "realm.connection.realmID must start with a letter or digit and use up to 64 letters, digits, '-' or '_'"
        );
    }
    let url =
        Url::parse(server_url).context("realm.connection.serverURL must be an http(s) base URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!(
            "realm.connection.serverURL must be an http(s) origin without credentials, path, query, or fragment"
        );
    }
    let server_url = url.as_str().trim_end_matches('/').to_owned();
    Ok(RealmConnection {
        server_url,
        token: token.to_owned(),
        realm_id: realm_id.to_owned(),
        stun_servers: Vec::new(),
        insecure: false,
    })
}

fn realm_listen_uri(connection: &RealmConnection) -> Result<String> {
    let url = Url::parse(&connection.server_url)
        .context("validated Realm rendezvous URL became invalid")?;
    let (scheme, default_port) = if url.scheme() == "https" {
        ("realm", 443)
    } else {
        ("realm+http", 80)
    };
    let host = match url
        .host()
        .context("Realm rendezvous URL is missing a host")?
    {
        Host::Ipv6(address) => format!("[{address}]"),
        host => host.to_string(),
    };
    let authority = match url.port() {
        Some(port) if port != default_port => format!("{host}:{port}"),
        Some(_) | None => host,
    };
    Ok(format!(
        "{scheme}://{}@{authority}/{}",
        connection.token, connection.realm_id
    ))
}

fn validate_acme(value: &Value) -> Result<()> {
    let acme = value.as_object().context("acme must be an object")?;
    allow_fields(
        acme,
        "acme",
        &[
            "domains",
            "email",
            "ca",
            "listenHost",
            "dir",
            "type",
            "http",
            "tls",
            "dns",
            "disableHTTP",
            "disableTLSALPN",
            "altHTTPPort",
            "altTLSALPNPort",
        ],
    )?;
    let domains = acme
        .get("domains")
        .and_then(Value::as_array)
        .context("acme.domains must be a list")?;
    if domains.is_empty()
        || domains
            .iter()
            .any(|domain| domain.as_str().is_none_or(|value| value.trim().is_empty()))
    {
        bail!("acme.domains must contain at least one domain name");
    }
    optional_string(acme, "email", "acme.email")?;
    optional_string(acme, "listenHost", "acme.listenHost")?;
    optional_string(acme, "dir", "acme.dir")?;
    let kind = optional_string(acme, "type", "acme.type")?
        .map(|kind| kind.to_ascii_lowercase())
        .filter(|kind| !kind.is_empty());
    if let Some(kind) = kind.as_deref()
        && !matches!(kind, "http" | "tls" | "dns")
    {
        bail!("acme.type must be http, tls, or dns");
    }
    if let Some(ca) = optional_string(acme, "ca", "acme.ca")?
        .filter(|ca| !ca.is_empty())
        .map(|ca| ca.to_ascii_lowercase())
        .as_deref()
        && !matches!(ca, "letsencrypt" | "le" | "zerossl" | "zero")
    {
        bail!("acme.ca must be letsencrypt or zerossl");
    }
    for (section, fields) in [("http", &["altPort"][..]), ("tls", &["altPort"][..])] {
        if let Some(settings) = acme.get(section) {
            let settings = settings
                .as_object()
                .with_context(|| format!("acme.{section} must be an object"))?;
            allow_fields(settings, &format!("acme.{section}"), fields)?;
            if let Some(port) = settings.get("altPort")
                && !port
                    .as_u64()
                    .is_some_and(|port| (1..=65535).contains(&port))
            {
                bail!("acme.{section}.altPort must be between 1 and 65535");
            }
        }
    }
    if let Some(dns) = acme.get("dns") {
        let dns = dns.as_object().context("acme.dns must be an object")?;
        allow_fields(dns, "acme.dns", &["name", "config"])?;
        let provider = required_string(dns, "name", "acme.dns.name")?.to_ascii_lowercase();
        if !matches!(
            provider.as_str(),
            "cloudflare"
                | "duckdns"
                | "gandi"
                | "godaddy"
                | "namecheap"
                | "njalla"
                | "porkbun"
                | "vultr"
        ) {
            bail!("unsupported Hysteria v2.12.3 ACME DNS provider: {provider}");
        }
        let config = dns
            .get("config")
            .and_then(Value::as_object)
            .context("acme.dns.config must be an object")?;
        if config.values().any(|value| !value.is_string()) {
            bail!("acme.dns.config values must be strings");
        }
    } else if kind.as_deref() == Some("dns") {
        bail!("acme.dns is required when acme.type is dns");
    }
    if let Some(kind) = kind.as_deref() {
        for variant in ["http", "tls", "dns"] {
            if variant != kind && acme.contains_key(variant) {
                bail!("acme.{variant} cannot be set when acme.type is {kind}");
            }
        }
        for field in [
            "disableHTTP",
            "disableTLSALPN",
            "altHTTPPort",
            "altTLSALPNPort",
        ] {
            if acme.contains_key(field) {
                bail!("acme.{field} is only supported when acme.type is omitted");
            }
        }
    } else if ["http", "tls", "dns"]
        .iter()
        .any(|field| acme.contains_key(*field))
    {
        bail!("acme.type is required when a nested HTTP, TLS, or DNS challenge is configured");
    }
    for field in ["disableHTTP", "disableTLSALPN"] {
        if let Some(value) = acme.get(field) {
            require_bool(value, &format!("acme.{field}"))?;
        }
    }
    for field in ["altHTTPPort", "altTLSALPNPort"] {
        if let Some(value) = acme.get(field)
            && !value
                .as_u64()
                .is_some_and(|port| (1..=65535).contains(&port))
        {
            bail!("acme.{field} must be between 1 and 65535");
        }
    }
    Ok(())
}

fn validate_obfs(value: &Value) -> Result<()> {
    let obfs = value.as_object().context("obfs must be an object")?;
    allow_fields(obfs, "obfs", &["type", "salamander", "gecko"])?;
    let kind = required_string(obfs, "type", "obfs.type")?;
    if !matches!(kind, "salamander" | "gecko") {
        bail!("obfs.type must be salamander or gecko");
    }
    for variant in ["salamander", "gecko"] {
        if variant != kind && obfs.contains_key(variant) {
            bail!("obfs.{variant} cannot be set when obfs.type is {kind}");
        }
    }
    let details = obfs
        .get(kind)
        .and_then(Value::as_object)
        .with_context(|| format!("obfs.{kind} must be an object"))?;
    if kind == "gecko" {
        allow_fields(
            details,
            "obfs.gecko",
            &["password", "minPacketSize", "maxPacketSize"],
        )?;
    } else {
        allow_fields(details, "obfs.salamander", &["password"])?;
    }
    required_string(details, "password", &format!("obfs.{kind}.password"))?;
    if kind == "gecko" {
        let min = details
            .get("minPacketSize")
            .and_then(Value::as_u64)
            .unwrap_or(512);
        let max = details
            .get("maxPacketSize")
            .and_then(Value::as_u64)
            .unwrap_or(1200);
        if min < 512 || max < min || max > 2048 {
            bail!("Gecko packet sizes must satisfy 512 <= minPacketSize <= maxPacketSize <= 2048");
        }
    }
    Ok(())
}

fn validate_bandwidth_config(value: &Value) -> Result<()> {
    let bandwidth = value.as_object().context("bandwidth must be an object")?;
    allow_fields(
        bandwidth,
        "bandwidth",
        &["up", "down", "disableLossCompensation"],
    )?;
    for field in ["up", "down"] {
        if let Some(value) = bandwidth.get(field) {
            let value = value
                .as_str()
                .with_context(|| format!("bandwidth.{field} must be a string"))?;
            validate_bandwidth_value(value)?;
        }
    }
    if let Some(value) = bandwidth.get("disableLossCompensation") {
        require_bool(value, "bandwidth.disableLossCompensation")?;
    }
    Ok(())
}

fn validate_bandwidth_value(value: &str) -> Result<()> {
    let value = value.trim().to_ascii_lowercase();
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    if split == 0 {
        bail!("bandwidth values must start with an integer");
    }
    let amount: u64 = value[..split]
        .parse()
        .context("bandwidth values must start with an integer")?;
    let unit = value[split..].trim();
    let multiplier: u128 = match unit {
        "b" | "bps" => 1,
        "k" | "kb" | "kbps" => 1_000,
        "m" | "mb" | "mbps" => 1_000_000,
        "g" | "gb" | "gbps" => 1_000_000_000,
        "t" | "tb" | "tbps" => 1_000_000_000_000,
        _ => bail!("unsupported bandwidth unit: {unit}"),
    };
    let bytes_per_second = (u128::from(amount) * multiplier) / 8;
    if bytes_per_second > u128::from(u64::MAX) {
        bail!("bandwidth value is too large");
    }
    if amount != 0 && bytes_per_second < u128::from(MIN_BANDWIDTH_BYTES_PER_SECOND) {
        bail!("non-zero bandwidth limits must be at least 65536 bytes per second");
    }
    Ok(())
}

fn validate_congestion(value: &Value) -> Result<()> {
    let congestion = value.as_object().context("congestion must be an object")?;
    allow_fields(congestion, "congestion", &["type", "bbrProfile"])?;
    let kind = required_string(congestion, "type", "congestion.type")?;
    if !matches!(kind, "bbr" | "reno") {
        bail!("congestion.type must be bbr or reno");
    }
    if let Some(profile) =
        optional_string(congestion, "bbrProfile", "congestion.bbrProfile")?.as_deref()
        && !matches!(profile, "standard" | "conservative" | "aggressive")
    {
        bail!("congestion.bbrProfile must be standard, conservative, or aggressive");
    }
    Ok(())
}

fn validate_duration(value: &str, field: &str) -> Result<()> {
    duration_milliseconds(value, field)?;
    Ok(())
}

fn duration_milliseconds(value: &str, field: &str) -> Result<f64> {
    let mut remaining = value.trim();
    if let Some(positive) = remaining.strip_prefix('+') {
        remaining = positive;
    }
    if remaining.is_empty() || remaining.starts_with('-') {
        bail!("{field} must be a non-negative duration");
    }

    let mut total_milliseconds = 0.0;
    while !remaining.is_empty() {
        let mut number_end = 0;
        let mut saw_decimal = false;
        let mut saw_digit = false;
        for (index, character) in remaining.char_indices() {
            if character.is_ascii_digit() {
                saw_digit = true;
                number_end = index + character.len_utf8();
            } else if character == '.' && !saw_decimal {
                saw_decimal = true;
                number_end = index + character.len_utf8();
            } else {
                break;
            }
        }
        if !saw_digit {
            bail!("{field} must be a duration such as 90s or 1m30s");
        }
        let number = &remaining[..number_end];
        let normalized_number = if number.starts_with('.') {
            format!("0{number}")
        } else if number.ends_with('.') {
            format!("{number}0")
        } else {
            number.to_owned()
        };
        let amount: f64 = normalized_number
            .parse()
            .with_context(|| format!("{field} must be a duration"))?;
        let unit_start = &remaining[number_end..];
        let (unit, multiplier) = [
            ("ns", 0.000_001),
            ("us", 0.001),
            ("µs", 0.001),
            ("μs", 0.001),
            ("ms", 1.0),
            ("s", 1_000.0),
            ("m", 60_000.0),
            ("h", 3_600_000.0),
        ]
        .into_iter()
        .find(|(unit, _)| unit_start.starts_with(unit))
        .context("duration uses an unsupported unit")?;
        total_milliseconds += amount * multiplier;
        if !total_milliseconds.is_finite() || total_milliseconds > i64::MAX as f64 / 1_000_000.0 {
            bail!("{field} exceeds the supported duration range");
        }
        remaining = &unit_start[unit.len()..];
    }
    Ok(total_milliseconds)
}

fn validate_duration_range(
    value: &str,
    field: &str,
    minimum_ms: f64,
    maximum_ms: f64,
) -> Result<()> {
    let milliseconds = duration_milliseconds(value, field)?;
    if milliseconds != 0.0 && !(minimum_ms..=maximum_ms).contains(&milliseconds) {
        anyhow::bail!(
            "{field} must be zero for the default or between {}ms and {}ms",
            minimum_ms as u64,
            maximum_ms as u64
        );
    }
    Ok(())
}

fn validate_quic(value: &Value) -> Result<()> {
    let quic = value.as_object().context("quic must be an object")?;
    allow_fields(
        quic,
        "quic",
        &[
            "initStreamReceiveWindow",
            "maxStreamReceiveWindow",
            "initConnReceiveWindow",
            "maxConnReceiveWindow",
            "maxIdleTimeout",
            "maxIncomingStreams",
            "disablePathMTUDiscovery",
            "disableStatelessReset",
        ],
    )?;
    let window_fields = [
        "initStreamReceiveWindow",
        "maxStreamReceiveWindow",
        "initConnReceiveWindow",
        "maxConnReceiveWindow",
    ];
    for field in window_fields {
        if let Some(value) = quic.get(field) {
            let window = value
                .as_u64()
                .with_context(|| format!("quic.{field} must be a non-negative integer"))?;
            if window != 0 && window < MIN_QUIC_RECEIVE_WINDOW {
                bail!(
                    "quic.{field} must be zero for the default or at least {MIN_QUIC_RECEIVE_WINDOW} bytes"
                );
            }
        }
    }
    let initial_stream = quic
        .get("initStreamReceiveWindow")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_STREAM_RECEIVE_WINDOW);
    let maximum_stream = quic
        .get("maxStreamReceiveWindow")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_STREAM_RECEIVE_WINDOW);
    if initial_stream > maximum_stream {
        bail!("quic.initStreamReceiveWindow cannot exceed quic.maxStreamReceiveWindow");
    }
    let initial_connection = quic
        .get("initConnReceiveWindow")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CONNECTION_RECEIVE_WINDOW);
    let maximum_connection = quic
        .get("maxConnReceiveWindow")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CONNECTION_RECEIVE_WINDOW);
    if initial_connection > maximum_connection {
        bail!("quic.initConnReceiveWindow cannot exceed quic.maxConnReceiveWindow");
    }
    if let Some(streams) = quic.get("maxIncomingStreams") {
        let streams = streams
            .as_i64()
            .with_context(|| "quic.maxIncomingStreams must be a non-negative integer")?;
        if streams < 0 {
            bail!("quic.maxIncomingStreams must be a non-negative integer");
        }
        if streams > 0 && streams < 8 {
            bail!("quic.maxIncomingStreams must be zero for the default or at least 8");
        }
    }
    if let Some(timeout) = optional_string(quic, "maxIdleTimeout", "quic.maxIdleTimeout")? {
        validate_duration_range(&timeout, "quic.maxIdleTimeout", 4_000.0, 120_000.0)?;
    }
    for field in ["disablePathMTUDiscovery", "disableStatelessReset"] {
        if let Some(value) = quic.get(field) {
            require_bool(value, &format!("quic.{field}"))?;
        }
    }
    Ok(())
}

fn validate_resolver(value: &Value) -> Result<()> {
    let resolver = value.as_object().context("resolver must be an object")?;
    allow_fields(
        resolver,
        "resolver",
        &["type", "tcp", "udp", "tls", "https"],
    )?;
    let kind = required_string(resolver, "type", "resolver.type")?;
    if !matches!(kind, "udp" | "tcp" | "tls" | "https") {
        bail!("resolver.type must be udp, tcp, tls, or https");
    }
    for variant in ["tcp", "udp", "tls", "https"] {
        if variant != kind && resolver.contains_key(variant) {
            bail!("resolver.{variant} cannot be set when resolver.type is {kind}");
        }
    }
    let settings = resolver
        .get(kind)
        .and_then(Value::as_object)
        .with_context(|| format!("resolver.{kind} must be an object"))?;
    let fields = if matches!(kind, "tls" | "https") {
        &["addr", "timeout", "sni", "insecure"][..]
    } else {
        &["addr", "timeout"][..]
    };
    allow_fields(settings, &format!("resolver.{kind}"), fields)?;
    required_string(settings, "addr", &format!("resolver.{kind}.addr"))?;
    optional_duration(settings, "timeout", &format!("resolver.{kind}.timeout"))?;
    optional_string(settings, "sni", &format!("resolver.{kind}.sni"))?;
    if let Some(insecure) = settings.get("insecure") {
        require_bool(insecure, &format!("resolver.{kind}.insecure"))?;
    }
    Ok(())
}

fn validate_sniff(value: &Value) -> Result<()> {
    let sniff = value.as_object().context("sniff must be an object")?;
    allow_fields(
        sniff,
        "sniff",
        &["enable", "timeout", "rewriteDomain", "tcpPorts", "udpPorts"],
    )?;
    for field in ["enable", "rewriteDomain"] {
        if let Some(value) = sniff.get(field) {
            require_bool(value, &format!("sniff.{field}"))?;
        }
    }
    optional_duration(sniff, "timeout", "sniff.timeout")?;
    for field in ["tcpPorts", "udpPorts"] {
        optional_string(sniff, field, &format!("sniff.{field}"))?;
    }
    Ok(())
}

fn validate_acl(value: &Value) -> Result<()> {
    let acl = value.as_object().context("acl must be an object")?;
    allow_fields(
        acl,
        "acl",
        &["file", "inline", "geoip", "geosite", "geoUpdateInterval"],
    )?;
    if acl.contains_key("file") && acl.contains_key("inline") {
        bail!("acl.file and acl.inline cannot both be set");
    }
    if let Some(file) = acl.get("file")
        && file.as_str().is_none_or(|value| value.trim().is_empty())
    {
        bail!("acl.file must be a path or resource reference");
    }
    if let Some(inline) = acl.get("inline") {
        let rules = inline.as_array().context("acl.inline must be a list")?;
        if rules.iter().any(|rule| rule.as_str().is_none()) {
            bail!("acl.inline entries must be strings");
        }
    }
    for field in ["geoip", "geosite"] {
        if let Some(value) = acl.get(field)
            && value.as_str().is_none_or(|value| value.trim().is_empty())
        {
            bail!("acl.{field} must be a path or resource reference");
        }
    }
    optional_duration(acl, "geoUpdateInterval", "acl.geoUpdateInterval")?;
    Ok(())
}

fn validate_outbounds(value: &Value) -> Result<()> {
    let outbounds = value.as_array().context("outbounds must be a list")?;
    let mut names = BTreeSet::new();
    for outbound in outbounds {
        let outbound = outbound
            .as_object()
            .context("each outbound must be an object")?;
        allow_fields(
            outbound,
            "outbound",
            &["name", "type", "direct", "socks5", "http"],
        )?;
        let name = required_string(outbound, "name", "outbounds.name")?;
        if !names.insert(name.to_owned()) {
            bail!("outbound names must be unique");
        }
        let kind = required_string(outbound, "type", "outbounds.type")?;
        for variant in ["direct", "socks5", "http"] {
            if variant != kind && outbound.contains_key(variant) {
                bail!("outbounds.{variant} cannot be set when type is {kind}");
            }
        }
        match kind {
            "direct" => {
                if let Some(settings) = outbound.get("direct") {
                    let settings = settings
                        .as_object()
                        .context("outbounds.direct must be an object")?;
                    allow_fields(
                        settings,
                        "outbounds.direct",
                        &["mode", "bindIPv4", "bindIPv6", "bindDevice", "fastOpen"],
                    )?;
                    for field in ["mode", "bindIPv4", "bindIPv6", "bindDevice"] {
                        optional_string(settings, field, &format!("outbounds.direct.{field}"))?;
                    }
                    if let Some(fast_open) = settings.get("fastOpen") {
                        require_bool(fast_open, "outbounds.direct.fastOpen")?;
                    }
                }
            }
            "socks5" => {
                let settings = outbound
                    .get("socks5")
                    .and_then(Value::as_object)
                    .context("outbounds.socks5 must be an object")?;
                allow_fields(
                    settings,
                    "outbounds.socks5",
                    &["addr", "username", "password"],
                )?;
                required_string(settings, "addr", "outbounds.socks5.addr")?;
                optional_string(settings, "username", "outbounds.socks5.username")?;
                optional_string(settings, "password", "outbounds.socks5.password")?;
            }
            "http" => {
                let settings = outbound
                    .get("http")
                    .and_then(Value::as_object)
                    .context("outbounds.http must be an object")?;
                allow_fields(settings, "outbounds.http", &["url", "insecure"])?;
                required_string(settings, "url", "outbounds.http.url")?;
                if let Some(insecure) = settings.get("insecure") {
                    require_bool(insecure, "outbounds.http.insecure")?;
                }
            }
            _ => bail!("outbound type must be direct, socks5, or http"),
        }
    }
    Ok(())
}

fn validate_masquerade(value: &Value) -> Result<()> {
    let masquerade = value.as_object().context("masquerade must be an object")?;
    allow_fields(
        masquerade,
        "masquerade",
        &[
            "type",
            "file",
            "proxy",
            "string",
            "listenHTTP",
            "listenHTTPS",
            "forceHTTPS",
        ],
    )?;
    optional_string(masquerade, "listenHTTP", "masquerade.listenHTTP")?;
    optional_string(masquerade, "listenHTTPS", "masquerade.listenHTTPS")?;
    if let Some(force_https) = masquerade.get("forceHTTPS") {
        require_bool(force_https, "masquerade.forceHTTPS")?;
    }
    let kind = required_string(masquerade, "type", "masquerade.type")?;
    for variant in ["file", "proxy", "string"] {
        if variant != kind && masquerade.contains_key(variant) {
            bail!("masquerade.{variant} cannot be set when masquerade.type is {kind}");
        }
    }
    match kind {
        "file" => {
            let settings = masquerade
                .get("file")
                .and_then(Value::as_object)
                .context("masquerade.file must be an object")?;
            allow_fields(settings, "masquerade.file", &["dir"])?;
            required_string(settings, "dir", "masquerade.file.dir")?;
        }
        "proxy" => {
            let settings = masquerade
                .get("proxy")
                .and_then(Value::as_object)
                .context("masquerade.proxy must be an object")?;
            allow_fields(
                settings,
                "masquerade.proxy",
                &["url", "rewriteHost", "xForwarded", "insecure"],
            )?;
            required_string(settings, "url", "masquerade.proxy.url")?;
            for field in ["rewriteHost", "xForwarded", "insecure"] {
                if let Some(value) = settings.get(field) {
                    require_bool(value, &format!("masquerade.proxy.{field}"))?;
                }
            }
        }
        "string" => {
            let settings = masquerade
                .get("string")
                .and_then(Value::as_object)
                .context("masquerade.string must be an object")?;
            allow_fields(
                settings,
                "masquerade.string",
                &["content", "headers", "statusCode"],
            )?;
            required_string(settings, "content", "masquerade.string.content")?;
            if let Some(headers) = settings.get("headers") {
                let headers = headers
                    .as_object()
                    .context("masquerade.string.headers must be a string map")?;
                if headers.values().any(|value| !value.is_string()) {
                    bail!("masquerade.string.headers values must be strings");
                }
            }
            if let Some(status) = settings.get("statusCode")
                && !status
                    .as_u64()
                    .is_some_and(|status| (100..=599).contains(&status))
            {
                bail!("masquerade.string.statusCode must be between 100 and 599");
            }
        }
        _ => bail!("masquerade.type must be file, proxy, or string"),
    }
    Ok(())
}

pub fn render_server_yaml(
    options: &Value,
    listen_addr: &str,
    node_id: &str,
    node_token: &str,
    stats_secret: &str,
    management_base_url: &str,
) -> Result<String> {
    validate_server_options(options)?;
    let realm = realm_connection(options)?;
    let mut root: Map<String, Value> = options.as_object().cloned().unwrap_or_default();
    let remove_empty_realm =
        if let Some(realm) = root.get_mut("realm").and_then(Value::as_object_mut) {
            realm.remove("connection");
            realm.is_empty()
        } else {
            false
        };
    if remove_empty_realm {
        root.remove("realm");
    }
    let listen = realm
        .as_ref()
        .map(realm_listen_uri)
        .transpose()?
        .unwrap_or_else(|| listen_addr.to_owned());
    root.insert("listen".into(), Value::String(listen));
    let base = management_base_url.trim_end_matches('/');
    root.insert(
        "auth".into(),
        json!({
            "type": "http",
            "http": {"url": format!("{base}/hy2/auth/{node_id}/{node_token}"), "insecure": false}
        }),
    );
    root.insert(
        "trafficStats".into(),
        json!({
            "listen": "127.0.0.1:9780",
            "secret": stats_secret
        }),
    );
    serde_yaml::to_string(&Value::Object(root)).context("serialize Hysteria 2 server configuration")
}

pub fn render_server_yaml_preview(
    options: &Value,
    listen_addr: &str,
    node_id: &str,
    node_token: &str,
    stats_secret: &str,
    management_base_url: &str,
) -> Result<String> {
    let mut preview = options.clone();
    if let Some(token) = preview
        .get_mut("realm")
        .and_then(Value::as_object_mut)
        .and_then(|realm| realm.get_mut("connection"))
        .and_then(Value::as_object_mut)
        .and_then(|connection| connection.get_mut("token"))
    {
        *token = Value::String("REDACTED_TOKEN".to_owned());
    }
    render_server_yaml(
        &preview,
        listen_addr,
        node_id,
        node_token,
        stats_secret,
        management_base_url,
    )
}

pub fn listener_hop_ports(listen_addr: &str) -> Option<&str> {
    let (_, ports) = listen_addr.rsplit_once(':')?;
    (ports.contains(',') || ports.contains('-')).then_some(ports)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        DEFAULT_REALM_STUN_SERVERS, realm_connection, render_server_yaml,
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
        assert!(
            validate_server_options(&json!({"ech": {"keyPath": "resource://ech-id"}})).is_err()
        );
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
    fn blocks_realm_mode_after_pinned_client_live_failure() {
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
            .unwrap_err()
            .to_string()
            .contains("Mihomo v1.19.31")
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
            connection.stun_servers,
            DEFAULT_REALM_STUN_SERVERS
                .iter()
                .map(|server| (*server).to_owned())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn realm_connection_config_is_not_rendered_when_live_client_is_blocked() {
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
        assert!(
            render_server_yaml(
                &options,
                ":443",
                "node-id",
                "node-secret",
                "stats-secret",
                "https://management.example",
            )
            .is_err()
        );
        assert!(
            render_server_yaml_preview(
                &options,
                ":443",
                "node-id",
                "node-secret",
                "stats-secret",
                "https://management.example",
            )
            .is_err()
        );
    }

    #[test]
    fn validates_common_nested_sections() {
        assert!(validate_server_options(&json!({"acme": {"domains": []}})).is_err());
        assert!(validate_server_options(&json!({"resolver": {"type": "dns"}})).is_err());
        assert!(
            validate_server_options(&json!({"acl": {"file": "rules.txt", "inline": []}})).is_err()
        );
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
        assert!(
            validate_server_options(&json!({"quic": {"initStreamReceiveWindow": 16383}})).is_err()
        );
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
        assert!(
            validate_server_options(&json!({"quic": {"maxStreamReceiveWindow": 16384}})).is_err()
        );
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
            validate_server_options(&json!({"quic": {"maxIdleTimeout": "30s", "typo": true}}))
                .is_err()
        );
        assert!(validate_server_options(&json!({"masquerade": {"type": "string", "string": {"content": "hello", "header": {}}}})).is_err());
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
}
