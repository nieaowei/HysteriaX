import Foundation

/// Literal defaults checked against sing-box v1.14.2 docs/configuration and native options.
/// These are read-only previews; examples and environment-dependent values are not literals.
enum TemplateDefaultCatalog {
    static let literals: [String: [String: JSONValue]] = [
        "LogOptions": ["level": .string("trace")], // log/log.go: New, not the documentation's info example.
        "NetworkNamespace": ["type": .string("default")],
        "RouteOptions": ["default_network_strategy": .string("default")],
        "InboundTLSOptions": ["min_version": .string("1.2"), "max_version": .string("1.3"), "handshake_timeout": .string("15s"), "client_authentication": .string("no")],
        "OutboundTLSOptions": ["engine": .string("go"), "min_version": .string("1.2"), "max_version": .string("1.3"), "handshake_timeout": .string("15s"), "fragment_fallback_delay": .string("500ms"), "spoof_method": .string("wrong-sequence")],
        "OutboundUTLSOptions": ["fingerprint": .string("chrome")],
        "HTTPClient": ["engine": .string("go"), "version": .integer(2)],
        "HTTPClientReference": ["engine": .string("go"), "version": .integer(2)],
        "CacheFileOptions": ["path": .string("cache.db"), "rdrc_timeout": .string("7d")],
        "ClashAPIOptions": ["default_mode": .string("Rule"), "external_ui_download_url": .string("https://github.com/MetaCubeX/Yacd-meta/archive/gh-pages.zip"), "access_control_allow_origin": .array([.string("*")])],
        "Inbound:tun": ["dns_mode": .string("hijack"), "iproute2_table_index": .integer(2022), "iproute2_rule_index": .integer(9000), "auto_redirect_input_mark": .string("0x2023"), "auto_redirect_output_mark": .string("0x2024"), "auto_redirect_reset_mark": .string("0x2025"), "auto_redirect_nfqueue": .integer(100), "auto_redirect_iproute2_fallback_rule_index": .integer(32768)],
        "Inbound:tuic": ["congestion_control": .string("cubic"), "auth_timeout": .string("3s"), "heartbeat": .string("10s")],
        "Inbound:shadowtls": ["version": .integer(1), "wildcard_sni": .string("off")],
        "Inbound:hysteria2": ["bbr_profile": .string("standard")],
        "Inbound:naive": ["quic_congestion_control": .string("bbr")],
        "Outbound:anytls": ["idle_session_check_interval": .string("30s"), "idle_session_timeout": .string("30s"), "min_idle_session": .integer(0)],
        "Outbound:tuic": ["congestion_control": .string("cubic"), "udp_relay_mode": .string("native")],
        "Outbound:shadowtls": ["version": .integer(1)],
        "Outbound:hysteria": ["hop_interval": .string("30s")],
        "Outbound:hysteria2": ["hop_interval": .string("30s"), "bbr_profile": .string("standard")],
        "Outbound:naive": ["quic_congestion_control": .string("cubic")],
        "Outbound:socks": ["version": .string("5")],
        "Outbound:ssh": ["server_port": .integer(22), "user": .string("root")],
        "Outbound:vless": ["packet_encoding": .string("xudp")],
        "Outbound:bridge": ["bridge_name": .string("bridge"), "iproute2_rule_index": .integer(100)],
        "Endpoint:tailscale": ["state_directory": .string("tailscale"), "control_url": .string("https://controlplane.tailscale.com"), "taildrop_directory": .string("Taildrop")],
        "Endpoint:openconnect": ["flavor": .string("anyconnect"), "version": .string("v9.21"), "compression_mode": .string("stateless"), "base_mtu": .integer(1406), "reconnect_timeout": .string("300s"), "queue_length": .integer(32)],
        "OpenConnectTNCCOptions": ["user_agent": .string("Neoteris HC Http")],
        "V2RayTransport:http": ["idle_timeout": .string("0s"), "ping_timeout": .string("15s")],
        "OpenVPNOutboundTLSOptions": ["server_name_type": .string("name"), "remote_certificate_tls": .string("server"), "certificate_profile": .string("legacy"), "version_min": .string("1.2")],
        "OpenVPNInboundTLSOptions": ["verify_client_certificate": .string("require"), "client_name_type": .string("name"), "remote_certificate_tls": .string("client"), "certificate_profile": .string("legacy"), "version_min": .string("1.2")],
        "CertificateProvider:acme": ["provider": .string("letsencrypt")],
        "CertificateProviderReference:acme": ["provider": .string("letsencrypt")],
        "CertificateProvider:cloudflare-origin-ca": ["request_type": .string("origin-rsa"), "requested_validity": .integer(5475)],
        "CertificateProviderReference:cloudflare-origin-ca": ["request_type": .string("origin-rsa"), "requested_validity": .integer(5475)],
        "Service:resolved": ["listen": .string("127.0.0.53"), "listen_port": .integer(53)],
        "Service:api": ["access_control_allow_origin": .array([.string("*")])],
        "Service:usbip-client": ["server_port": .integer(3240)],
        "Service:usbip-server": ["listen_port": .integer(3240), "provider": .string("default")],
        "Hysteria2RealmPortMapping": ["timeout": .string("10s"), "lifetime": .string("10m")]
    ]

    static func value(_ key: String, name: String, kind: String, object: JSONValue) -> JSONValue? {
        if name == "Endpoint", ["openvpn-client", "openvpn-server"].contains(kind) {
            let isStatic = object["mode"].stringValue == "static_key"
            if key == "max_clients", kind == "openvpn-server" { return .integer(isStatic ? 1 : 1024) }
            if key == "topology", isStatic || kind == "openvpn-server" { return .string(isStatic ? "p2p" : "subnet") }
            if key == "cipher", isStatic { return .string("BF-CBC") }
            if key == "auth_retry", kind == "openvpn-client", !isStatic { return .string("none") }
            if key == "allow_compression", kind == "openvpn-client" { return .string("no") }
            let values: [String: JSONValue] = ["mode": .string("tls"), "network": .string("udp"), "mtu": .integer(1500), "auth": .string("SHA1"), "replay_window": .integer(64), "replay_window_time": .string("15s")]
            if let value = values[key] { return value }
            if !isStatic {
                return ["renegotiate_interval": .string("1h"), "handshake_window": .string("1m"), "tls_timeout": .string("2s"), "data_ciphers": .array([.string("AES-256-GCM"), .string("AES-128-GCM"), .string("CHACHA20-POLY1305")])][key]
            }
        }
        if ["Inbound", "Outbound"].contains(name), kind == "snell" {
            if key == "mode", object["version"].integerValue == 6 { return .string("default") }
            if key == "obfs_mode" { return .string("none") }
            if key == "obfs_host", object["version"].integerValue == 4 { return .string("bing.com") }
        }
        if ["udp_mapping", "udp_filtering"].contains(key) { return .string("endpoint_independent") }
        if name == "Outbound", key == "network", ["hysteria", "hysteria2", "socks", "trojan", "tuic", "vless", "vmess", "snell", "shadowsocks"].contains(kind) {
            return .array([.string("tcp"), .string("udp")])
        }
        return literals[name + ":" + kind]?[key] ?? literals[name]?[key]
    }

    /// Dynamic defaults are explained without pretending the editor's host is the target.
    static func explanation(_ key: String, name: String, kind: String) -> String? {
        if name == "Outbound", kind == "naive", key == "quic_session_receive_window" { return "未设置时由内核采用默认接收窗口（文档为 15 MB）；显式填写时使用整数字节数。" }
        if name == "Inbound", kind == "tun", key == "stack" { return "默认由内核编译选项决定：启用 gVisor 时为 mixed，否则为 system。" }
        if name == "Endpoint", kind == "wireguard", key == "workers" { return "默认使用目标设备的 CPU 数量。" }
        if name == "DNSServer", kind == "hosts", key == "path" { return "默认读取目标系统的 hosts 文件：Windows 使用系统目录，其他系统使用 /etc/hosts。" }
        if (name == "DNS" || name == "RouteOptions"), key == "final" { return "未设置时使用对应列表中的第一项。" }
        if name == "Outbound", kind == "selector", key == "default" { return "未设置时使用对应列表中的第一项。" }
        if name == "Endpoint", kind == "tailscale", ["hostname", "system_interface_name", "system_interface_mtu", "listen_port"].contains(key) { return "默认由目标设备的平台、主机信息和接口状态决定。" }
        if name == "Endpoint", kind == "openconnect", ["reported_os", "user_agent", "local_hostname", "dpd_interval", "trojan_interval", "dtls_local_port", "tcp_keep_alive", "tcp_keep_alive_interval"].contains(key) { return "默认由目标平台、VPN 类型、服务器协商结果或系统网络设置决定。" }
        if name == "Endpoint", ["openvpn-client", "openvpn-server"].contains(kind), ["mss_fix", "ping_restart", "renegotiate_bytes", "renegotiate_packets", "name"].contains(key) { return "默认由 OpenVPN 会话模式、传输协议、MTU、加密算法或服务器协商结果决定。" }
        if name == "Endpoint", kind == "openvpn-client", key == "topology" { return "TLS 模式默认采用服务器推送的拓扑；静态密钥模式默认使用 p2p。" }
        if ["OpenVPNOutboundTLSOptions", "OpenVPNInboundTLSOptions"].contains(name), ["version_max", "cipher", "groups"].contains(key) { return "默认由 TLS 实现及当前证书配置选择受支持的版本和算法。" }
        if ["InboundTLSOptions", "OutboundTLSOptions"].contains(name), ["cipher_suites", "curve_preferences", "server_name"].contains(key) { return "默认由 TLS 实现、对端地址和握手上下文决定。" }
        if ["CertificateProvider", "CertificateProviderReference"].contains(name), key == "data_directory" { return "默认使用目标设备环境中的证书存储目录，不使用编辑器所在设备的路径。" }
        if name == "V2RayTransport", kind == "grpc", ["idle_timeout", "ping_timeout"].contains(key) { return "默认行为取决于使用标准 gRPC 还是内置 gRPC 实现。" }
        return nil
    }
}
