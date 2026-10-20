/// Navigation and section names are UI labels; configuration paths stay unchanged.
enum TemplateModule {
    static let all = ["基础配置", "DNS", "入站与服务", "代理组／出站", "分流规则", "节点注入", "实验性配置", "高级配置", "版本历史"]
    static func destination(_ key: String) -> String {
        if key == "experimental" { return "实验性配置" }
        if ["dns", "hosts", "fake-ip-filter"].contains(key) { return "DNS" }
        if ["inbounds", "tun", "listeners", "port", "socks-port", "mixed-port", "redir-port", "tproxy-port", "allow-lan", "bind-address", "authentication", "skip-auth-prefixes", "lan-allowed-ips", "lan-disallowed-ips", "ss-config", "vmess-config", "tuic-server", "tunnels", "services"].contains(key) { return "入站与服务" }
        if ["proxy-groups", "proxy-providers", "proxies", "outbounds", "endpoints"].contains(key) { return "代理组／出站" }
        if ["rules", "rule-providers", "sub-rules", "route", "sniffer", "find-process-mode"].contains(key) { return "分流规则" }
        if ["mode", "log-level", "log", "ipv6", "profile", "ntp", "secret", "certificate", "certificate_providers", "http_clients"].contains(key) || key.hasPrefix("external-controller") || key.hasPrefix("external-ui") || key == "external-doh-server" { return "基础配置" }
        return "高级配置"
    }
}

enum TemplateFieldLayout {
    static let dialerFieldOrder = [
        "detour", "bind_interface", "inet4_bind_address", "inet6_bind_address",
        "bind_address_no_port", "routing_mark", "reuse_addr", "netns", "connect_timeout",
        "tcp_fast_open", "tcp_multi_path", "disable_tcp_keep_alive", "tcp_keep_alive",
        "tcp_keep_alive_interval", "udp_fragment", "domain_resolver", "network_strategy",
        "network_type", "fallback_network_type", "fallback_delay"
    ]
    static func orderedObjectFields(_ keys: [String], required: Set<String> = []) -> [String] {
        keys.sorted {
            let left = dialerFieldOrder.firstIndex(of: $0) ?? dialerFieldOrder.count
            let right = dialerFieldOrder.firstIndex(of: $1) ?? dialerFieldOrder.count
            if left != right { return left < right }
            if required.contains($0) != required.contains($1) { return required.contains($0) }
            return $0 < $1
        }
    }
    static func dialerFields(in keys: [String]) -> [String] {
        guard keys.contains(where: { ["detour", "bind_interface", "connect_timeout"].contains($0) }) else { return [] }
        return dialerFieldOrder.filter { keys.contains($0) }
    }
    static let objectSectionOrder = ["基本标识", "远端地址", "认证信息", "匹配条件", "规则动作", "协议选项", "TLS 与传输", "网络连接"]
    static func objectSection(_ key: String, path: [String]) -> String {
        if ["tag", "name", "type", "enabled", "disabled", "enable", "invert", "mode"].contains(key) { return "基本标识" }
        if ["server", "server_port", "address", "port", "listen", "listen_port", "url", "urls"].contains(key) { return "远端地址" }
        if ["password", "username", "uuid", "token", "users", "method", "private_key", "public_key", "access_key", "secret_key"].contains(key) { return "认证信息" }
        if ["tls", "transport", "multiplex", "smux", "udp_over_tcp", "reality", "utls", "ech"].contains(key) { return "TLS 与传输" }
        if dialerFieldOrder.contains(key) || ["dialer-proxy", "interface-name", "routing-mark"].contains(key) { return "网络连接" }
        if path.contains("rules") || path.contains("rule_set") {
            return ["action", "outbound", "server", "sniffer", "timeout", "strategy", "disable_cache", "rewrite_ttl", "client_subnet"].contains(key) ? "规则动作" : "匹配条件"
        }
        return "协议选项"
    }
    static let expandedObjects: Set<String> = ["dns", "route", "log", "profile", "ntp", "certificate", "tls", "experimental", "tun", "sniffer", "external-controller-cors", "clash-for-android"]

    private static let sectionOrder = [
        "基础信息", "日志", "状态持久化", "时间同步", "控制接口", "跨域访问", "Web 管理界面",
        "解析行为", "缓存设置", "Hosts 解析", "Fake IP", "服务器", "默认 DNS 服务器", "代理节点 DNS", "直连 DNS", "后备 DNS", "DNS 分流策略", "规则", "静态 Hosts",
        "监听端口", "局域网访问", "身份认证", "免认证网段", "允许访问的网段", "禁止访问的网段", "TUN 基础设置", "路由接管", "路由标记", "连接超时", "性能与兼容",
        "代理组", "代理提供者", "出站连接", "端点", "动态节点", "默认路由", "出口接口", "默认连接选项", "进程与邻居发现", "规则集", "子规则", "流量嗅探",
        "网络连接", "连接保活", "Geo 数据", "时间同步连接", "证书存储", "缓存文件", "Clash API", "V2Ray API", "调试设置", "实验性功能", "客户端兼容", "配置元信息", "其他选项"
    ]

    private static let moduleOrder = [
        "基础配置": ["基础信息", "日志", "状态持久化", "时间同步", "时间同步连接", "控制接口", "跨域访问", "Web 管理界面", "证书存储", "证书内容", "证书文件", "证书目录", "证书提供者", "HTTP 客户端"],
        "DNS": ["基础信息", "解析行为", "缓存设置", "Hosts 解析", "Fake IP", "规则", "服务器", "默认 DNS 服务器", "代理节点 DNS", "直连 DNS", "后备 DNS", "后备 DNS 筛选", "DNS 分流策略", "代理节点 DNS 策略", "静态 Hosts", "Fake IP 过滤"],
        "入站与服务": ["监听端口", "局域网访问", "身份认证", "免认证网段", "允许访问的网段", "禁止访问的网段", "入站连接", "自定义监听器", "TUN 基础设置", "路由接管", "路由标记", "连接超时", "性能与兼容"],
        "代理组／出站": ["代理组", "代理提供者", "出站连接", "端点", "动态节点"],
        "分流规则": ["默认路由", "出口接口", "默认连接选项", "进程与邻居发现", "规则", "规则集", "子规则", "流量嗅探"],
        "实验性配置": ["缓存文件", "Clash API", "V2Ray API", "调试设置", "实验性功能", "实验性指纹"],
        "高级配置": ["网络连接", "连接保活", "Geo 数据", "Geo 数据来源", "网络命名空间", "证书内容", "证书文件", "证书目录", "客户端认证证书", "自定义证书", "ECH 密钥", "客户端兼容", "配置元信息", "其他选项"]
    ]

    private static let collectionTitles = [
        "inbounds": "入站连接", "listeners": "自定义监听器", "services": "服务", "tun": "TUN", "tuic-server": "TUIC 服务", "tunnels": "端口转发",
        "authentication": "身份认证", "skip-auth-prefixes": "免认证网段", "lan-allowed-ips": "允许访问的网段", "lan-disallowed-ips": "禁止访问的网段",
        "proxy-groups": "代理组", "proxy-providers": "代理提供者", "proxies": "动态节点", "outbounds": "出站连接", "endpoints": "端点",
        "dns": "DNS", "rules": "规则", "rule_set": "规则集", "rule-providers": "规则集", "sub-rules": "子规则", "hosts": "静态 Hosts",
        "servers": "服务器", "nameserver": "服务器", "default-nameserver": "默认 DNS 服务器", "proxy-server-nameserver": "代理节点 DNS", "direct-nameserver": "直连 DNS", "fallback": "后备 DNS", "fallback-filter": "后备 DNS 筛选",
        "nameserver-policy": "DNS 分流策略", "proxy-server-nameserver-policy": "代理节点 DNS 策略", "fake-ip-filter": "Fake IP 过滤", "client_subnet": "客户端子网", "dhcp_lease_files": "DHCP 租约文件",
        "default_domain_resolver": "默认域名解析器", "default_network_type": "默认网络类型", "default_fallback_network_type": "默认回退网络类型",
        "log": "日志", "profile": "状态持久化", "ntp": "时间同步", "experimental": "实验性功能", "cache_file": "缓存文件", "clash_api": "Clash API", "v2ray_api": "V2Ray API", "debug": "调试设置",
        "certificate": "证书内容", "certificate_path": "证书文件", "certificate_directory_path": "证书目录", "certificate_providers": "证书提供者", "tls": "TLS 设置", "custom-certifactes": "自定义证书", "client-auth-cert": "客户端认证证书", "ech-key": "ECH 密钥",
        "http_clients": "HTTP 客户端", "network_namespaces": "网络命名空间", "geox-url": "Geo 数据来源", "iptables": "iptables 接管",
        "external-controller-cors": "跨域访问", "allow-origins": "允许的来源", "sniffer": "流量嗅探", "sniff": "嗅探协议", "sniffing": "嗅探协议", "force-domain": "强制嗅探域名", "skip-domain": "跳过嗅探的域名", "skip-dst-address": "跳过嗅探的目标地址", "skip-src-address": "跳过嗅探的源地址", "port-whitelist": "嗅探端口白名单",
        "dns-hijack": "DNS 劫持", "route-address": "接管的路由地址", "route-address-set": "接管的路由规则集", "route-exclude-address": "排除的路由地址", "route-exclude-address-set": "排除的路由规则集",
        "inet4-route-address": "IPv4 路由地址", "inet4-route-exclude-address": "IPv4 排除地址", "inet6-address": "IPv6 地址", "inet6-route-address": "IPv6 路由地址", "inet6-route-exclude-address": "IPv6 排除地址", "loopback-address": "回环地址",
        "include-interface": "接管的网络接口", "exclude-interface": "排除的网络接口", "include-package": "接管的应用", "exclude-package": "排除的应用", "include-uid": "接管的用户", "exclude-uid": "排除的用户", "include-uid-range": "接管的用户范围", "exclude-uid-range": "排除的用户范围", "include-android-user": "接管的 Android 用户", "include-mac-address": "接管的 MAC 地址", "exclude-mac-address": "排除的 MAC 地址",
        "exclude-dst-port": "排除的目标端口", "exclude-dst-port-range": "排除的目标端口范围", "exclude-src-port": "排除的源端口", "exclude-src-port-range": "排除的源端口范围",
        "domain_resolver": "域名解析器", "network_type": "网络类型", "fallback_network_type": "回退网络类型", "clash-for-android": "Android 客户端兼容", "fingerprints": "实验性指纹"
    ]

    static func section(path: [String], complex: Bool) -> String {
        guard let root = path.first, let key = path.last else { return "其他选项" }
        if path == ["dns", "optimistic"] { return "缓存设置" }
        if complex { return collectionTitles[key] ?? key }
        if path.count > 1 {
            switch root {
            case "dns":
                if key.contains("cache") || ["disable_expire", "optimistic"].contains(key) { return "缓存设置" }
                if key.hasPrefix("fake-ip") { return "Fake IP" }
                if ["use-hosts", "use-system-hosts"].contains(key) { return "Hosts 解析" }
                if ["enable", "listen", "listen-routing-mark"].contains(key) { return "基础信息" }
                return "解析行为"
            case "route":
                if key == "final" { return "默认路由" }
                if ["find_process", "find_neighbor"].contains(key) { return "进程与邻居发现" }
                if ["auto_detect_interface", "override_android_vpn", "default_interface", "default_mark"].contains(key) { return "出口接口" }
                return "默认连接选项"
            case "log": return "日志"
            case "profile": return "状态持久化"
            case "ntp": return ["enabled", "enable", "interval", "write_to_system", "write-to-system", "server", "server_port", "port"].contains(key) ? "时间同步" : "时间同步连接"
            case "certificate", "tls": return "证书存储"
            case "experimental": return "实验性功能"
            case "external-controller-cors": return "跨域访问"
            case "sniffer": return "流量嗅探"
            case "clash-for-android": return "客户端兼容"
            case "tun":
                if ["enable", "device", "stack", "mtu"].contains(key) { return "TUN 基础设置" }
                if key.contains("mark") || key.hasPrefix("iproute2") { return "路由标记" }
                if key.contains("timeout") { return "连接超时" }
                if key.hasPrefix("auto-") || key == "strict-route" { return "路由接管" }
                return "性能与兼容"
            default: return "其他选项"
            }
        }
        if ["mode", "ipv6"].contains(key) { return "基础信息" }
        if key == "log-level" { return "日志" }
        if key.hasPrefix("external-controller") || ["secret", "external-doh-server"].contains(key) { return "控制接口" }
        if key.hasPrefix("external-ui") { return "Web 管理界面" }
        if ["port", "socks-port", "mixed-port", "redir-port", "tproxy-port", "ss-config", "vmess-config"].contains(key) { return "监听端口" }
        if ["allow-lan", "bind-address"].contains(key) { return "局域网访问" }
        if key == "find-process-mode" { return "进程与邻居发现" }
        if key.contains("keep-alive") { return "连接保活" }
        if key.hasPrefix("geo") { return "Geo 数据" }
        if ["interface-name", "routing-mark", "tcp-concurrent", "inbound-mptcp", "inbound-tfo", "unified-delay", "global-client-fingerprint", "global-ua", "disable-keep-alive", "etag-support"].contains(key) { return "网络连接" }
        if key == "$schema" { return "配置元信息" }
        return "其他选项"
    }

    static func precedes(_ lhs: String, _ rhs: String, module: String) -> Bool {
        let preferred = moduleOrder[module] ?? []
        let order = preferred + sectionOrder.filter { !preferred.contains($0) }
        let left = order.firstIndex(of: lhs) ?? order.count
        let right = order.firstIndex(of: rhs) ?? order.count
        return left == right ? lhs < rhs : left < right
    }
}
