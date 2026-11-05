//! Subscription negotiation and renderers. Never log this module's node model: it contains secrets.
use super::{ClashEchOptions, ClashProxy, ClashRealmOptions};
use crate::error::ApiError;
use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubscriptionFormat {
    Mihomo,
    Singbox,
    Base64,
    Uri,
}

pub(super) fn requested_format(
    query: Option<&str>,
) -> Result<Option<SubscriptionFormat>, ApiError> {
    let formats: Vec<_> = url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .filter(|(key, _)| key == "format")
        .map(|(_, value)| value.into_owned())
        .collect();
    if formats.len() > 1 {
        return Err(ApiError::bad_request("format must occur once"));
    }
    match formats.first().map(String::as_str).unwrap_or("auto") {
        "auto" => Ok(None),
        "mihomo" => Ok(Some(SubscriptionFormat::Mihomo)),
        "singbox" => Ok(Some(SubscriptionFormat::Singbox)),
        "base64" => Ok(Some(SubscriptionFormat::Base64)),
        "uri" => Ok(Some(SubscriptionFormat::Uri)),
        _ => Err(ApiError::bad_request(
            "format must be auto, mihomo, singbox, base64, or uri",
        )),
    }
}

/// Target runtime, independent of the subscription format and kernel version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Platform {
    #[default]
    Unknown,
    Linux,
    Windows,
    Macos,
    Android,
    Ios,
}

impl Platform {
    fn detect(ua: &str) -> Self {
        let tokens: Vec<_> = ua.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        let has = |names: &[&str]| tokens.iter().any(|token| names.contains(token));
        // Mobile UAs also contain Linux / Mac OS X; match the specific OS first.
        if has(&["android"]) {
            Self::Android
        } else if has(&["ios", "iphone", "ipad"]) {
            Self::Ios
        } else if has(&["windows", "win32", "win64"]) {
            Self::Windows
        } else if has(&["macos", "macintosh", "darwin"]) {
            Self::Macos
        } else if has(&["linux", "openwrt"]) {
            Self::Linux
        } else {
            Self::Unknown
        }
    }

    pub fn requested(query: Option<&str>) -> Result<Option<Self>, ApiError> {
        let values: Vec<_> = url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
            .filter(|(key, _)| key == "platform")
            .map(|(_, value)| value.into_owned())
            .collect();
        if values.len() > 1 {
            return Err(ApiError::bad_request("platform must occur once"));
        }
        match values.first().map(String::as_str).unwrap_or("auto") {
            "auto" => Ok(None),
            "linux" | "openwrt" => Ok(Some(Self::Linux)),
            "windows" => Ok(Some(Self::Windows)),
            "macos" => Ok(Some(Self::Macos)),
            "android" => Ok(Some(Self::Android)),
            "ios" => Ok(Some(Self::Ios)),
            _ => Err(ApiError::bad_request(
                "platform must be auto, linux, openwrt, windows, macos, android, or ios",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Version(pub(super) u32, pub(super) u32, pub(super) u32);

#[derive(Default, Clone, Copy)]
pub(super) struct ClientInfo {
    pub name: &'static str,
    pub format: Option<SubscriptionFormat>,
    pub version: Option<Version>,
    pub platform: Platform,
}

impl ClientInfo {
    pub fn detect(ua: &str) -> Self {
        let ua = ua.to_ascii_lowercase();
        let platform = Platform::detect(&ua);
        // Match specific families before generic names, with token boundaries (not "notmihomo").
        for (alias, name, format) in [
            ("clash.meta", "mihomo", SubscriptionFormat::Mihomo),
            ("clashmeta", "mihomo", SubscriptionFormat::Mihomo),
            ("mihomo", "mihomo", SubscriptionFormat::Mihomo),
            ("sing-box", "singbox", SubscriptionFormat::Singbox),
            ("singbox", "singbox", SubscriptionFormat::Singbox),
            ("shadowrocket", "shadowrocket", SubscriptionFormat::Base64),
            ("v2rayng", "v2rayng", SubscriptionFormat::Base64),
            ("v2rayn", "v2rayn", SubscriptionFormat::Base64),
        ] {
            for (index, _) in ua.match_indices(alias) {
                let end = index + alias.len();
                if ua[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                    || ua[end..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    continue;
                }
                let tail = &ua[end..];
                let version = if tail.starts_with('/') || tail.starts_with(char::is_whitespace) {
                    parse_version(tail.trim_start_matches(|c: char| c == '/' || c.is_whitespace()))
                } else {
                    None
                };
                return Self {
                    name,
                    format: Some(format),
                    version,
                    platform,
                };
            }
        }
        Self {
            name: "unknown",
            platform,
            ..Self::default()
        }
    }

    fn at_least(&self, format: SubscriptionFormat, minimum: Version) -> bool {
        // A wrapper application's version is not the target kernel's version.
        self.format == Some(format) && self.version.is_some_and(|v| v >= minimum)
    }
}

fn parse_version(value: &str) -> Option<Version> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let digits = value
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()?;
    let mut parts = digits.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    // Pre-release feature support must be validated independently.
    if value[digits.len()..].starts_with('-') {
        return None;
    }
    Some(Version(major, minor, patch))
}

#[derive(Clone)]
pub(super) struct SubscriptionRealm {
    pub server_url: String,
    pub token: String,
    pub realm_id: String,
    pub stun_servers: Option<Vec<String>>,
    pub skip_cert_verify: Option<bool>,
}

impl SubscriptionRealm {
    pub fn to_mihomo(&self) -> ClashRealmOptions {
        ClashRealmOptions {
            enable: true,
            server_url: self.server_url.clone(),
            token: self.token.clone(),
            realm_id: self.realm_id.clone(),
            stun_servers: self.stun_servers.clone(),
            skip_cert_verify: self.skip_cert_verify,
        }
    }
}

/// Protocol-neutral connection data, loaded from the deployed configuration and assignment.
/// Deliberately does not implement Debug or Serialize.
#[derive(Clone)]
pub(super) struct SubscriptionNode {
    pub id: String,
    pub display_name: String,
    pub name: String,
    pub server: String,
    pub port: u16,
    pub ports: Option<String>,
    pub hop_interval: Option<u64>,
    pub password: String,
    pub sni: Option<String>,
    pub certificate: Option<String>,
    pub private_key: Option<String>,
    pub skip_cert_verify: bool,
    pub up: Option<String>,
    pub down: Option<String>,
    pub obfs: Option<String>,
    pub obfs_password: Option<String>,
    pub obfs_min_packet_size: Option<u64>,
    pub obfs_max_packet_size: Option<u64>,
    pub ech_config: Option<String>,
    pub realm: Option<SubscriptionRealm>,
}

impl SubscriptionNode {
    pub fn to_mihomo(&self) -> ClashProxy {
        ClashProxy {
            name: self.name.clone(),
            protocol: "hysteria2".into(),
            server: self.server.clone(),
            port: self.port,
            ports: self.ports.clone(),
            hop_interval: self.hop_interval,
            password: self.password.clone(),
            sni: self.sni.clone(),
            certificate: self.certificate.clone(),
            private_key: self.private_key.clone(),
            skip_cert_verify: self.skip_cert_verify,
            udp: true,
            up: self.up.clone(),
            down: self.down.clone(),
            obfs: self.obfs.clone(),
            obfs_password: self.obfs_password.clone(),
            obfs_min_packet_size: self.obfs_min_packet_size,
            obfs_max_packet_size: self.obfs_max_packet_size,
            ech_opts: self.ech_config.clone().map(|config| ClashEchOptions {
                enable: true,
                config,
            }),
            realm_opts: self.realm.as_ref().map(SubscriptionRealm::to_mihomo),
            handshake_timeout: self.realm.as_ref().map(|_| 30),
        }
    }

    pub(super) fn incompatible(
        &self,
        format: SubscriptionFormat,
        client: &ClientInfo,
    ) -> Option<&'static str> {
        if format == SubscriptionFormat::Mihomo {
            // The fixed legacy path retains its established v1.19.31 output contract.
            // For a known older kernel, only enable advanced fields once our pinned validator supports them.
            if client.format == Some(format)
                && client.version.is_some_and(|v| v < Version(1, 19, 31))
                && (self.certificate.is_some()
                    || self.ech_config.is_some()
                    || self.realm.is_some()
                    || self.obfs.as_deref() == Some("gecko"))
            {
                return Some("mihomo_advanced_requires_1_19_31");
            }
            return None;
        }
        if format == SubscriptionFormat::Singbox {
            // Generic downloaders do not advertise the destination kernel.
            // Unknown versions retain every representable feature; only a known
            // sing-box kernel can establish a lower compatibility ceiling.
            let known_kernel = client.format == Some(format) && client.version.is_some();
            if client.format == Some(format)
                && client.version.is_some_and(|v| v < Version(1, 14, 0))
            {
                return Some("singbox_requires_1_14");
            }
            // ECH is enabled only for the fixture validated with our pinned 1.14.2 binary.
            if self.ech_config.is_some()
                && known_kernel
                && !client.at_least(format, Version(1, 14, 2))
            {
                return Some("ech_requires_singbox_1_14_2");
            }
            if self
                .obfs
                .as_deref()
                .is_some_and(|v| v != "salamander" && v != "gecko")
            {
                return Some("unsupported_obfuscation");
            }
            if self
                .up
                .as_deref()
                .is_some_and(|v| bandwidth_mbps(v).is_none())
                || self
                    .down
                    .as_deref()
                    .is_some_and(|v| bandwidth_mbps(v).is_none())
            {
                return Some("bandwidth_not_representable_in_whole_mbps");
            }
            return None;
        }
        // URI parsers in the named apps have not been verified for these extensions.
        if self.certificate.is_some() || self.private_key.is_some() {
            return Some("uri_cannot_embed_mtls");
        }
        if self.ech_config.is_some() {
            return Some("uri_ech_client_unverified");
        }
        if self.realm.is_some() {
            return Some("uri_realm_client_unverified");
        }
        if self.ports.is_some() {
            return Some("uri_port_hopping_client_unverified");
        }
        if self.obfs.as_deref().is_some_and(|v| v != "salamander") {
            return Some("uri_obfuscation_client_unverified");
        }
        None
    }

    pub(super) fn singbox(&self) -> Value {
        let mut tls = json!({"enabled": true, "insecure": self.skip_cert_verify});
        if let Some(sni) = &self.sni {
            tls["server_name"] = json!(sni);
        }
        if let Some(cert) = &self.certificate {
            tls["client_certificate"] = json!(cert.lines().collect::<Vec<_>>());
        }
        if let Some(key) = &self.private_key {
            tls["client_key"] = json!(key.lines().collect::<Vec<_>>());
        }
        if let Some(ech) = &self.ech_config {
            tls["ech"] = json!({"enabled": true, "config": [format!("-----BEGIN ECH CONFIGS-----\n{ech}\n-----END ECH CONFIGS-----")]});
        }
        let mut outbound =
            json!({"type": "hysteria2", "tag": self.name, "password": self.password});
        if let Some(realm) = &self.realm {
            let mut options = json!({"server_url": realm.server_url, "token": realm.token, "realm_id": realm.realm_id});
            if let Some(stun) = &realm.stun_servers {
                options["stun_servers"] = json!(stun);
            }
            if realm.skip_cert_verify == Some(true) {
                options["http_client"] = json!({"tls": {"enabled": true, "insecure": true}});
            }
            outbound["realm"] = options;
            tls["handshake_timeout"] = json!("30s");
        } else {
            outbound["server"] = json!(self.server.trim_start_matches('[').trim_end_matches(']'));
            if let Some(ports) = &self.ports {
                outbound["server_ports"] = json!(
                    ports
                        .split(',')
                        .map(|p| if p.contains('-') {
                            p.replace('-', ":")
                        } else {
                            format!("{p}:{p}")
                        })
                        .collect::<Vec<_>>()
                );
                outbound["hop_interval"] = json!(format!("{}s", self.hop_interval.unwrap_or(30)));
            } else {
                outbound["server_port"] = json!(self.port);
            }
        }
        outbound["tls"] = tls;
        if self.certificate.is_some() {
            // sing-box 1.14+ uses ChromeParrot (a Chrome-like QUIC ClientHello) for
            // Hysteria2 by default; that path of its QUIC stack refuses a TLS config
            // carrying a client certificate with
            // "quic: server-side tls.Config fields are not supported with ChromeParrot".
            // mTLS nodes therefore have to ask for the standard handshake.
            outbound["disable_chrome_parrot"] = json!(true);
        }
        for (name, bandwidth) in [("up_mbps", &self.up), ("down_mbps", &self.down)] {
            if let Some(value) = bandwidth.as_deref().and_then(bandwidth_mbps) {
                outbound[name] = json!(value);
            }
        }
        if let Some(kind) = &self.obfs {
            let mut obfs = json!({"type": kind, "password": self.obfs_password});
            if kind == "gecko" {
                if let Some(size) = self.obfs_min_packet_size {
                    obfs["min_packet_size"] = json!(size);
                }
                if let Some(size) = self.obfs_max_packet_size {
                    obfs["max_packet_size"] = json!(size);
                }
            }
            outbound["obfs"] = obfs;
        }
        outbound
    }

    fn uri(&self) -> String {
        let server = self.server.trim_start_matches('[').trim_end_matches(']');
        let host = if server.contains(':') {
            format!("[{server}]")
        } else {
            server.to_owned()
        };
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if let Some(sni) = &self.sni {
            query.append_pair("sni", sni);
        }
        query.append_pair("insecure", if self.skip_cert_verify { "1" } else { "0" });
        if let Some(obfs) = &self.obfs {
            query.append_pair("obfs", obfs);
            if let Some(password) = &self.obfs_password {
                query.append_pair("obfs-password", password);
            }
        }
        format!(
            "hysteria2://{}@{host}:{}/?{}#{}",
            percent_encode(&self.password),
            self.port,
            query.finish(),
            percent_encode(&self.name)
        )
    }
}

// Hysteria bandwidth units are decimal bits/second. sing-box accepts integral Mbps only.
fn bandwidth_mbps(value: &str) -> Option<u64> {
    let value = value.trim().to_ascii_lowercase();
    let index = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let amount: u64 = value[..index].parse().ok()?;
    let multiplier = match value[index..].trim() {
        "b" | "bps" => 1,
        "k" | "kb" | "kbps" => 1_000,
        "m" | "mb" | "mbps" => 1_000_000,
        "g" | "gb" | "gbps" => 1_000_000_000,
        "t" | "tb" | "tbps" => 1_000_000_000_000,
        _ => return None,
    };
    let bits = amount.checked_mul(multiplier)?;
    (bits % 1_000_000 == 0 && bits / 1_000_000 <= i32::MAX as u64).then_some(bits / 1_000_000)
}

fn percent_encode(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(result, "%{byte:02X}").unwrap();
        }
    }
    result
}

pub(super) fn render_subscription(
    nodes: &[SubscriptionNode],
    format: SubscriptionFormat,
    client: &ClientInfo,
) -> Result<Response, ApiError> {
    // Reject the whole template, including empty subscriptions, before node filtering.
    if format == SubscriptionFormat::Singbox
        && client.format == Some(format)
        && client.version.is_some_and(|v| v < Version(1, 14, 0))
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "subscription_incompatible",
            "sing-box 1.14.0 or newer is required.",
        ));
    }
    let mut compatible = Vec::new();
    for node in nodes {
        if let Some(reason) = node.incompatible(format, client) {
            tracing::info!(format = ?format, reason, "subscription node filtered");
        } else {
            compatible.push(node);
        }
    }
    let filtered = nodes.len() - compatible.len();
    if !nodes.is_empty() && compatible.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "subscription_incompatible",
            format!(
                "All {filtered} nodes are incompatible with the selected format/client version. Use a supported client or the Mihomo subscription."
            ),
        ));
    }
    let (body, content_type) = match format {
        SubscriptionFormat::Mihomo | SubscriptionFormat::Singbox => {
            return Err(ApiError::bad_request(
                "native subscriptions require a published template and explicit platform",
            ));
        }
        SubscriptionFormat::Uri | SubscriptionFormat::Base64 => {
            let mut body = compatible
                .iter()
                .map(|n| n.uri())
                .collect::<Vec<_>>()
                .join("\n");
            if !body.is_empty() {
                body.push('\n');
            }
            if format == SubscriptionFormat::Base64 {
                body = STANDARD.encode(body.as_bytes());
            }
            (body, "text/plain; charset=utf-8")
        }
    };
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        "x-hysteriax-filtered-nodes",
        HeaderValue::from_str(&filtered.to_string()).unwrap(),
    );
    Ok(response)
}

#[cfg(test)]
#[path = "formats_tests.rs"]
mod tests;
