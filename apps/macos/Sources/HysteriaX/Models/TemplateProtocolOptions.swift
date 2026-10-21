import Foundation

/// Versioned presentation rules. Hidden controls never erase existing configuration.
enum TemplateProtocolOptions {
    static func visible(_ key: String, name: String, kind: String, value: JSONValue) -> Bool {
        if name == "Inbound", kind == "tun" {
            let autoRoute = value["auto_route"].boolValue == true
            let redirect = autoRoute && value["auto_redirect"].boolValue == true
            if key == "dns_address", value["dns_mode"].stringValue == "disabled" { return false }
            if key == "auto_redirect" || key.hasPrefix("iproute2_") || key == "strict_route"
                || key.hasPrefix("route_") || key.hasPrefix("include_") || key.hasPrefix("exclude_") {
                if !autoRoute { return false }
            }
            if key.hasPrefix("auto_redirect_") || ["exclude_mptcp", "include_mac_address", "exclude_mac_address"].contains(key) {
                return redirect
            }
        }
        if name == "Endpoint", kind == "tailscale" {
            if key.hasPrefix("system_interface_"), value["system_interface"].boolValue != true { return false }
            if key == "exit_node_allow_lan_access", (value["exit_node"].stringValue ?? "").isEmpty { return false }
            if key == "relay_server_static_endpoints", (value["relay_server_port"].integerValue ?? 0) == 0 { return false }
        }
        if name == "Endpoint", ["openvpn-client", "openvpn-server"].contains(kind) {
            let staticKey = value["mode"].stringValue == "static_key"
            let tlsOnly = ["tls", "username", "password", "auth_retry", "static_challenge", "static_challenge_echo", "users", "duplicate_cn", "push", "route_no_pull", "pull_filters", "data_ciphers", "data_ciphers_fallback", "tls_timeout", "handshake_window"]
            if staticKey, tlsOnly.contains(key) || key.hasPrefix("renegotiate_") { return false }
            if !staticKey, ["static_key", "static_key_path", "key_direction", "cipher"].contains(key) { return false }
            if ["mss_fix", "mss_fix_mode"].contains(key), value["mss_fix_disabled"].boolValue == true { return false }
            if key == "mss_fix_mode", (value["mss_fix"].integerValue ?? 0) == 0 { return false }
            if key == "ping_restart", value["ping_restart_disabled"].boolValue == true { return false }
            if key == "renegotiate_interval", value["renegotiate_disabled"].boolValue == true { return false }
            if key == "static_challenge_echo", (value["static_challenge"].stringValue ?? "").isEmpty { return false }
        }
        if ["OpenVPNControlWrapOptions", "OpenVPNInboundControlWrapOptions"].contains(name) {
            if key == "direction", value["type"].stringValue != "tls_auth" { return false }
            if key == "force_cookie", value["type"].stringValue != "tls_crypt_v2" { return false }
        }
        if name == "OpenVPNPushOptions", key == "redirect_gateway_flags", value["redirect_gateway"].boolValue != true { return false }
        if name == "Inbound", kind == "cloudflared", key == "datagram_version", value["protocol"].stringValue != "quic" { return false }
        if name == "Service", kind == "usbip-client",
           key == "protect_path" || TemplateFieldLayout.dialerFieldOrder.filter({ $0 != "detour" }).contains(key) { return false }
        if ["CertificateProvider", "CertificateProviderReference"].contains(name), kind == "acme" {
            let dnsChallenge = !(value["dns01_challenge"].objectValue ?? [:]).isEmpty
            if ["disable_http_challenge", "disable_tls_alpn_challenge", "alternative_http_port", "alternative_tls_port"].contains(key), dnsChallenge { return false }
            if key == "alternative_http_port", value["disable_http_challenge"].boolValue == true { return false }
            if key == "alternative_tls_port", value["disable_tls_alpn_challenge"].boolValue == true { return false }
        }
        if name == "OutboundMultiplexOptions", ["max_connections", "min_streams"].contains(key), (value["max_streams"].integerValue ?? 0) > 0 { return false }
        if name == "OutboundTLSOptions", key == "spoof_method", (value["spoof"].stringValue ?? "").isEmpty { return false }
        return true
    }

    static func section(_ key: String, name: String, kind: String) -> String? {
        if name == "Inbound", kind == "tun" {
            if ["interface_name", "netns", "mtu", "address", "stack"].contains(key) { return "TUN 接口" }
            if ["dns_mode", "dns_address"].contains(key) { return "TUN DNS" }
            if key == "auto_redirect" || key.hasPrefix("auto_redirect_") || key == "exclude_mptcp" { return "自动重定向" }
            if key.hasPrefix("include_") || key.hasPrefix("exclude_") { return "流量范围" }
            if key == "auto_route" || key.hasPrefix("iproute2_") || key.hasPrefix("route_") || ["strict_route", "loopback_address"].contains(key) { return "路由接管" }
            if key == "platform" { return "系统代理" }
        }
        if name == "Endpoint", kind == "tailscale" {
            if ["auth_key", "control_url"].contains(key) { return "登录与控制服务器" }
            if ["hostname", "ephemeral", "advertise_tags"].contains(key) { return "节点身份" }
            if ["accept_routes", "exit_node", "exit_node_allow_lan_access", "advertise_routes", "advertise_exit_node"].contains(key) { return "子网与出口节点" }
            if key.hasPrefix("relay_server_") || key == "listen_port" { return "中继与监听" }
            if key.hasPrefix("system_interface") { return "系统接口" }
            if ["state_directory", "taildrop_directory"].contains(key) { return "状态与文件" }
            if key == "ssh_server" { return "SSH 服务" }
        }
        if name == "Endpoint", ["openvpn-client", "openvpn-server"].contains(kind) {
            if ["system", "name", "mtu", "address", "peer_address", "peer_address_ipv6", "topology"].contains(key) { return "接口设置" }
            if ["server", "server_port", "servers", "remote", "remote_port", "remote_random", "network"].contains(key) { return "远端地址" }
            if ["username", "password", "users", "auth_retry", "static_challenge", "static_challenge_echo"].contains(key) { return "认证信息" }
            if key == "tls" || key.hasPrefix("static_key") || ["key_direction", "cipher", "data_ciphers", "data_ciphers_fallback", "auth"].contains(key) { return "加密与密钥" }
            if key.hasPrefix("mss_fix") || ["fragment", "replay_window", "replay_window_time", "compression", "compression_lzo", "allow_compression"].contains(key) { return "数据通道" }
            if key.hasPrefix("ping_") || key.hasPrefix("renegotiate_") || ["tls_timeout", "handshake_window", "explicit_exit_notify"].contains(key) { return "保活与重协商" }
            if key.hasPrefix("route") || key.hasPrefix("redirect_") || ["pull_filters", "push", "block_ipv6"].contains(key) { return "路由与推送" }
        }
        if name == "Endpoint", kind == "openconnect" {
            if ["system", "name", "mtu", "base_mtu"].contains(key) { return "接口设置" }
            if ["username", "password", "auth_group", "cookie", "token", "form_entries"].contains(key) { return "认证信息" }
            if ["reported_os", "user_agent", "version", "local_hostname", "mobile"].contains(key) { return "客户端身份" }
            if ["csd", "hip", "tncc", "fortinet_host_check"].contains(key) { return "主机检查" }
            if ["dpd_interval", "reconnect_timeout", "trojan_interval", "queue_length"].contains(key) { return "会话与重连" }
        }
        if ["CertificateProvider", "CertificateProviderReference"].contains(name) {
            if ["domain", "default_server_name", "key_type", "profile", "request_type", "requested_validity"].contains(key) { return "证书签发" }
            if ["email", "provider", "account_key", "external_account", "api_token", "origin_ca_key"].contains(key) { return "签发账户" }
            if key.contains("challenge") || key.hasPrefix("alternative_") { return "域名验证" }
            if key == "data_directory" { return "状态与文件" }
        }
        if ["udp_timeout", "udp_mapping", "udp_filtering", "udp_nat_max"].contains(key) { return "UDP NAT" }
        if TemplateHTTPClientLayout.http2.contains(key), !(name == "Outbound" && kind == "urltest") { return "连接与流控制" }
        if TemplateHTTPClientLayout.quic.contains(key) { return "QUIC 参数" }
        return ["tls": "TLS 设置", "transport": "传输层", "multiplex": "多路复用", "udp_over_tcp": "UDP over TCP", "brutal": "TCP Brutal", "http_client": "HTTP 客户端", "headers": "请求标头"][key]
    }

    static func defaultValue(_ key: String, name: String, kind: String, object: JSONValue = .null) -> JSONValue? {
        TemplateDefaultCatalog.value(key, name: name, kind: kind, object: object)
    }
}
