import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct NodeConfigurationView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let nodeID: String

    @State private var detail: NodeDetail?
    @State private var isLoading = true
    @State private var isSaving = false
    @State private var errorMessage: String?
    @State private var listenAddress = ":443"
    @State private var proxyProbeURL = ""
    @State private var tlsMode = "none"
    @State private var acmeDomains: [StringListEntry] = []
    @State private var acmeEmail = ""
    @State private var acmeType = "http"
    @State private var acmeCA = ""
    @State private var acmeListenHost = ""
    @State private var acmeDirectory = ""
    @State private var acmeHTTPAltPort = ""
    @State private var acmeTLSAltPort = ""
    @State private var acmeDNSName = ""
    @State private var acmeDNSEntries: [StringMapEntry] = []
    @State private var acmeLegacyMode = false
    @State private var acmeDisableHTTP = false
    @State private var acmeDisableTLSALPN = false
    @State private var acmeLegacyHTTPPort = ""
    @State private var acmeLegacyTLSPort = ""
    @State private var certificatePath = ""
    @State private var privateKeyPath = ""
    @State private var tlsClientCAPath = ""
    @State private var tlsSNIGuard = "strict"
    @State private var echKeyPath = ""
    @State private var bandwidthUp = ""
    @State private var bandwidthDown = ""
    @State private var disableLossCompensation = false
    @State private var ignoreClientBandwidth = false
    @State private var disableUDP = false
    @State private var udpIdleTimeout = ""
    @State private var speedTest = false
    @State private var obfsType = "none"
    @State private var obfsPassword = ""
    @State private var obfsMinPacket = "512"
    @State private var obfsMaxPacket = "1200"
    @State private var congestionType = "bbr"
    @State private var bbrProfile = "standard"
    @State private var aclInline = ""
    @State private var aclFileReference = ""
    @State private var geoIPReference = ""
    @State private var geoSiteReference = ""
    @State private var geoUpdateInterval = ""
    @State private var quicIdleTimeout = ""
    @State private var quicMaxStreams = ""
    @State private var quicInitStreamWindow = ""
    @State private var quicMaxStreamWindow = ""
    @State private var quicInitConnectionWindow = ""
    @State private var quicMaxConnectionWindow = ""
    @State private var disablePathMTU = false
    @State private var disableStatelessReset = false
    @State private var resolverType = "none"
    @State private var resolverAddress = ""
    @State private var resolverSNI = ""
    @State private var resolverTimeout = ""
    @State private var resolverInsecure = false
    @State private var sniffEnabled = false
    @State private var sniffTimeout = ""
    @State private var sniffRewriteDomain = false
    @State private var sniffTCPPorts = ""
    @State private var sniffUDPPorts = ""
    @State private var hasOutbound = false
    @State private var outboundDrafts: [OutboundDraft] = []
    @State private var masqueradeType = "none"
    @State private var masqueradeDirectory = ""
    @State private var masqueradeURL = ""
    @State private var masqueradeContent = ""
    @State private var masqueradeHeaderEntries: [StringMapEntry] = []
    @State private var masqueradeStatusCode = ""
    @State private var masqueradeRewriteHost = false
    @State private var masqueradeXForwarded = false
    @State private var masqueradeInsecure = false
    @State private var masqueradeListenHTTP = ""
    @State private var masqueradeListenHTTPS = ""
    @State private var masqueradeForceHTTPS = false
    @State private var resources: [NodeResource] = []
    @State private var showingResourceImporter = false
    @State private var resourceKind = "certificate"
    @State private var resourceMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("节点配置").font(.title.bold())
                    Text(detail.map { "\($0.name) · 修订版 \($0.revision)" } ?? (isLoading ? "加载节点配置…" : "节点配置不可用"))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Button(isSaving ? "正在保存…" : "保存并同步") { save() }
                    .disabled(isSaving || isLoading || detail == nil || !store.isConnected)
                    .keyboardShortcut(.defaultAction)
            }
            if isLoading {
                ProgressView("读取节点配置…").frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let detail {
                Form {
                    Section("监听") {
                        TextField("UDP 监听地址", text: $listenAddress)
                        Text("支持端口列表和范围，例如 :443,445-450。端口跳跃节点的公网端口须与首个监听端口相同，远端还须安装 nftables 或 iptables。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    Section("部署连通性检查") {
                        TextField("HTTP 探测 URL（可选）", text: $proxyProbeURL, prompt: Text("http://status.example.test/health"))
                        Text("默认探测节点的本机统计接口。自定义 ACL 或 outbound 阻止该地址时，填写一个可通过当前路由访问并返回 HTTP 200 的无凭据 URL。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    Section("TLS 与证书") {
                        Picker("证书来源", selection: $tlsMode) {
                            Text("未设置").tag("none")
                            Text("ACME 自动申请").tag("acme")
                            Text("服务器已有证书或资源引用").tag("tls")
                        }
                        if tlsMode == "acme" {
                            Text("ACME 域名")
                            StringListEditor(entries: $acmeDomains, prompt: "域名")
                            TextField("ACME 邮箱", text: $acmeEmail)
                            Picker("ACME CA", selection: $acmeCA) {
                                Text("默认 CA").tag("")
                                Text("Let's Encrypt").tag("letsencrypt")
                                Text("ZeroSSL").tag("zerossl")
                            }
                            TextField("ACME 监听主机（可选）", text: $acmeListenHost)
                            TextField("ACME 状态目录（可选）", text: $acmeDirectory)
                            Toggle("使用旧版 ACME 字段", isOn: $acmeLegacyMode)
                            if acmeLegacyMode {
                                Toggle("禁用 HTTP-01", isOn: $acmeDisableHTTP)
                                Toggle("禁用 TLS-ALPN-01", isOn: $acmeDisableTLSALPN)
                                TextField("旧版 HTTP-01 备用端口", text: $acmeLegacyHTTPPort)
                                TextField("旧版 TLS-ALPN-01 备用端口", text: $acmeLegacyTLSPort)
                            } else {
                                Picker("验证方式", selection: $acmeType) {
                                    Text("HTTP-01（TCP 80）").tag("http")
                                    Text("TLS-ALPN-01（TCP 443）").tag("tls")
                                    Text("DNS-01").tag("dns")
                                }
                                if acmeType == "http" {
                                    TextField("HTTP-01 备用端口", text: $acmeHTTPAltPort)
                                } else if acmeType == "tls" {
                                    TextField("TLS-ALPN-01 备用端口", text: $acmeTLSAltPort)
                                } else {
                                    Picker("DNS 服务商", selection: $acmeDNSName) {
                                        Text("选择服务商").tag("")
                                        Text("Cloudflare").tag("cloudflare")
                                        Text("DuckDNS").tag("duckdns")
                                        Text("Gandi").tag("gandi")
                                        Text("GoDaddy").tag("godaddy")
                                        Text("Namecheap").tag("namecheap")
                                        Text("Njalla").tag("njalla")
                                        Text("Porkbun").tag("porkbun")
                                        Text("Vultr").tag("vultr")
                                    }
                                    Text("DNS 服务商参数会加密保存在管理服务。")
                                        .font(.callout).foregroundStyle(.secondary)
                                    Text("参数名须符合所选服务商要求，例如 cloudflare_api_token 或 porkbun_api_secret_key。")
                                        .font(.callout).foregroundStyle(.secondary)
                                    StringMapEditor(
                                        entries: $acmeDNSEntries,
                                        keyPrompt: "参数名",
                                        valuePrompt: "凭据或参数值",
                                        masksValues: true
                                    )
                                }
                            }
                        } else if tlsMode == "tls" {
                            TextField("证书路径或 resource:// 引用", text: $certificatePath)
                            TextField("私钥路径或 resource:// 引用", text: $privateKeyPath)
                            TextField("mTLS 客户端 CA（可选）", text: $tlsClientCAPath)
                            Picker("SNI 检查", selection: $tlsSNIGuard) {
                                Text("严格").tag("strict")
                                Text("DNS SAN").tag("dns-san")
                                Text("关闭").tag("disable")
                            }
                            Text("填写客户端 CA 后，分配用户时必须提供匹配的客户端证书和私钥。至少分配一位用户后再部署；健康检查会用该证书完成真实 Hysteria 连接，并在订阅中提供证书内容。")
                                .font(.callout).foregroundStyle(.secondary)
                        }
                        Picker("ECH 密钥资源", selection: $echKeyPath) {
                            Text("关闭 ECH").tag("")
                            ForEach(resources.filter { $0.resourceKind == "ech_key" }) { resource in
                                Text(resource.name).tag(resource.reference)
                            }
                        }
                        Text("ECH 需要 TLS 或 ACME 证书。上传 Hysteria 生成的 ech.pem，订阅会从中提取客户端配置列表。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    Section("配置资源") {
                        Picker("资源类型", selection: $resourceKind) {
                            Text("证书").tag("certificate")
                            Text("私钥").tag("private_key")
                            Text("ECH 密钥").tag("ech_key")
                            Text("ACL 规则").tag("acl")
                            Text("GeoIP 数据").tag("geoip")
                            Text("GeoSite 数据").tag("geosite")
                        }
                        Button("上传文件…") { showingResourceImporter = true }
                            .disabled(!store.isConnected)
                        if let resourceMessage {
                            Text(resourceMessage).font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
                        }
                        if resources.isEmpty {
                            Text("没有已上传资源。上传后复制 resource:// 引用到证书、私钥或 ACL 路径字段。")
                                .font(.callout).foregroundStyle(.secondary)
                        } else {
                            ForEach(resources) { resource in
                                HStack {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(resource.name).fontWeight(.medium)
                                        Text("\(resource.resourceKind) · \(ByteCountFormatter.string(fromByteCount: resource.sizeBytes, countStyle: .file))")
                                            .font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    Button("复制引用") { copy(resource.reference) }
                                        .buttonStyle(.borderless)
                                }
                            }
                        }
                    }
                    Section("带宽与拥塞控制") {
                        Text("速率使用固定版 Hysteria 的整数单位；非零值至少为 65,536 字节/秒。")
                            .font(.callout).foregroundStyle(.secondary)
                        TextField("上传限制（如 100 Mbps）", text: $bandwidthUp)
                        TextField("下载限制（如 500 Mbps）", text: $bandwidthDown)
                        Toggle("禁用带宽损失补偿", isOn: $disableLossCompensation)
                        Toggle("忽略客户端带宽声明", isOn: $ignoreClientBandwidth)
                        Picker("拥塞控制", selection: $congestionType) {
                            Text("BBR").tag("bbr")
                            Text("Reno").tag("reno")
                        }
                        if congestionType == "bbr" {
                            Picker("BBR 配置", selection: $bbrProfile) {
                                Text("标准").tag("standard")
                                Text("保守").tag("conservative")
                                Text("激进").tag("aggressive")
                            }
                        }
                    }
                    Section("UDP、测速与混淆") {
                        Toggle("禁用 UDP 转发", isOn: $disableUDP)
                        TextField("UDP 空闲超时（如 30s）", text: $udpIdleTimeout)
                        Text("UDP 空闲超时须为 2 到 600 秒；留空或填 0 使用默认值。")
                            .font(.callout).foregroundStyle(.secondary)
                        Toggle("启用测速服务", isOn: $speedTest)
                        Picker("混淆", selection: $obfsType) {
                            Text("关闭").tag("none")
                            Text("Salamander").tag("salamander")
                            Text("Gecko（实验性）").tag("gecko")
                        }
                        if obfsType != "none" {
                            SecureField("混淆密码", text: $obfsPassword)
                        }
                        if obfsType == "gecko" {
                            TextField("最小分片字节数", text: $obfsMinPacket)
                            TextField("最大分片字节数", text: $obfsMaxPacket)
                        }
                    }
                    Section("ACL") {
                        TextField("ACL 文件路径或 resource:// 引用", text: $aclFileReference)
                        TextField("GeoIP 文件路径或 resource:// 引用", text: $geoIPReference)
                        TextField("GeoSite 文件路径或 resource:// 引用", text: $geoSiteReference)
                        TextField("Geo 数据更新间隔（如 24h）", text: $geoUpdateInterval)
                        Text("acl.file 和下面的内联规则不能同时填写。")
                            .font(.callout).foregroundStyle(.secondary)
                        Text("每行一条规则，例如 reject(all, udp/443)。")
                            .font(.callout).foregroundStyle(.secondary)
                        TextEditor(text: $aclInline)
                            .font(.system(.body, design: .monospaced))
                            .frame(minHeight: 100)
                    }
                    Section("QUIC 参数") {
                        TextField("空闲超时（如 30s）", text: $quicIdleTimeout)
                        TextField("最大并发流", text: $quicMaxStreams)
                        TextField("初始流接收窗口（字节）", text: $quicInitStreamWindow)
                        TextField("最大流接收窗口（字节）", text: $quicMaxStreamWindow)
                        TextField("初始连接接收窗口（字节）", text: $quicInitConnectionWindow)
                        TextField("最大连接接收窗口（字节）", text: $quicMaxConnectionWindow)
                        Toggle("禁用路径 MTU 探测", isOn: $disablePathMTU)
                        Toggle("禁用无状态重置", isOn: $disableStatelessReset)
                        Text("非默认接收窗口至少 16 KiB，初始窗口不能大于最大窗口；并发流至少 8，空闲超时须为 4 到 120 秒。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    Section("DNS 解析器") {
                        Picker("类型", selection: $resolverType) {
                            Text("系统默认").tag("none")
                            Text("UDP").tag("udp")
                            Text("TCP").tag("tcp")
                            Text("TLS").tag("tls")
                            Text("HTTPS").tag("https")
                        }
                        if resolverType != "none" {
                            TextField("解析器地址", text: $resolverAddress)
                            TextField("查询超时（如 4s）", text: $resolverTimeout)
                            if resolverType == "tls" || resolverType == "https" {
                                TextField("TLS SNI", text: $resolverSNI)
                                Toggle("跳过解析器 TLS 验证", isOn: $resolverInsecure)
                            }
                        }
                    }
                    Section("协议嗅探") {
                        Toggle("启用嗅探", isOn: $sniffEnabled)
                        if sniffEnabled {
                            TextField("嗅探超时（如 2s）", text: $sniffTimeout)
                            Toggle("重写已有域名请求", isOn: $sniffRewriteDomain)
                            TextField("TCP 端口（如 80,443）", text: $sniffTCPPorts)
                            TextField("UDP 端口（如 all）", text: $sniffUDPPorts)
                        }
                    }
                    Section("出站代理") {
                        Toggle("配置出站列表", isOn: $hasOutbound)
                            .onChange(of: hasOutbound) { _, enabled in
                                if enabled && outboundDrafts.isEmpty {
                                    outboundDrafts = [OutboundDraft()]
                                }
                            }
                        if hasOutbound {
                            Text("第一项是默认出站；ACL 可按名称选择其他出站。")
                                .font(.caption).foregroundStyle(.secondary)
                            ForEach($outboundDrafts) { $outbound in
                                VStack(alignment: .leading, spacing: 8) {
                                    HStack {
                                        Text(outbound.id == outboundDrafts.first?.id ? "默认出站" : "备用出站")
                                            .font(.headline)
                                        Spacer()
                                        Button("设为默认", systemImage: "arrow.up.to.line") {
                                            moveOutboundToDefault(outbound.id)
                                        }
                                        .disabled(outbound.id == outboundDrafts.first?.id)
                                        Button(role: .destructive) {
                                            removeOutbound(outbound.id)
                                        } label: {
                                            Label("删除", systemImage: "trash")
                                        }
                                    }
                                    TextField("出站名称", text: $outbound.name)
                                    Picker("类型", selection: $outbound.type) {
                                        Text("直连").tag("direct")
                                        Text("SOCKS5").tag("socks5")
                                        Text("HTTP(S)").tag("http")
                                    }
                                    if outbound.type == "socks5" {
                                        TextField("SOCKS5 地址", text: $outbound.address)
                                        TextField("用户名（可选）", text: $outbound.username)
                                        SecureField("密码（可选）", text: $outbound.password)
                                    } else if outbound.type == "http" {
                                        TextField("代理 URL", text: $outbound.url)
                                        Toggle("跳过 HTTPS 代理验证", isOn: $outbound.insecure)
                                    } else {
                                        Picker("直连模式", selection: $outbound.directMode) {
                                            Text("自动（双栈）").tag("")
                                            Text("优先 IPv6").tag("64")
                                            Text("优先 IPv4").tag("46")
                                            Text("仅 IPv6").tag("6")
                                            Text("仅 IPv4").tag("4")
                                        }
                                        TextField("绑定 IPv4", text: $outbound.bindIPv4)
                                        TextField("绑定 IPv6", text: $outbound.bindIPv6)
                                        TextField("绑定网络设备", text: $outbound.bindDevice)
                                        Toggle("启用 TCP Fast Open", isOn: $outbound.fastOpen)
                                    }
                                }
                                .padding(10)
                                .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                            }
                            Button("添加出站", systemImage: "plus") {
                                outboundDrafts.append(OutboundDraft())
                            }
                        }
                    }
                    Section("伪装") {
                        Picker("模式", selection: $masqueradeType) {
                            Text("关闭").tag("none")
                            Text("静态文件目录").tag("file")
                            Text("反向代理").tag("proxy")
                            Text("固定文本").tag("string")
                        }
                        if masqueradeType == "file" { TextField("节点上的目录路径", text: $masqueradeDirectory) }
                        if masqueradeType == "proxy" {
                            TextField("上游 URL", text: $masqueradeURL)
                            Toggle("重写 Host", isOn: $masqueradeRewriteHost)
                            Toggle("添加 X-Forwarded 头", isOn: $masqueradeXForwarded)
                            Toggle("跳过上游 TLS 验证", isOn: $masqueradeInsecure)
                        }
                        if masqueradeType == "string" {
                            TextField("返回内容", text: $masqueradeContent)
                            TextField("HTTP 状态码", text: $masqueradeStatusCode)
                            Text("响应头")
                            StringMapEditor(
                                entries: $masqueradeHeaderEntries,
                                keyPrompt: "响应头名称",
                                valuePrompt: "响应头值",
                                masksValues: false
                            )
                        }
                        TextField("额外 HTTP 监听地址", text: $masqueradeListenHTTP)
                        TextField("额外 HTTPS 监听地址", text: $masqueradeListenHTTPS)
                        Toggle("强制 HTTPS", isOn: $masqueradeForceHTTPS)
                    }
                    Section("兼容限制") {
                        Text("Realm 暂不开放：固定版 Mihomo 的真实 rendezvous 连接超时。启用 Mimic 也仍受兼容限制；ECH 需要使用已上传的 ech_key 资源。端口跳跃已通过实时连接验收。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    Section("YAML 预览") {
                        Text(detail.yamlPreview)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                if let errorMessage {
                    Text(errorMessage).foregroundStyle(.red).font(.callout)
                }
            } else if let errorMessage {
                VStack(spacing: 12) {
                    ContentUnavailableView(
                        "无法加载节点配置",
                        systemImage: "exclamationmark.triangle",
                        description: Text(errorMessage)
                    )
                    Button("重试") { Task { await load() } }
                }
            }
        }
        .padding(20)
        .frame(minWidth: 650, minHeight: 760)
        .task(id: nodeID) { await load() }
        .fileImporter(isPresented: $showingResourceImporter, allowedContentTypes: [.data], allowsMultipleSelection: false) { result in
            handleResourceImport(result)
        }
    }

    private func resetForm() {
        detail = nil
        errorMessage = nil
        listenAddress = ":443"
        proxyProbeURL = ""
        tlsMode = "none"
        acmeDomains = []
        acmeEmail = ""
        acmeType = "http"
        acmeCA = ""
        acmeListenHost = ""
        acmeDirectory = ""
        acmeHTTPAltPort = ""
        acmeTLSAltPort = ""
        acmeDNSName = ""
        acmeDNSEntries = []
        acmeLegacyMode = false
        acmeDisableHTTP = false
        acmeDisableTLSALPN = false
        acmeLegacyHTTPPort = ""
        acmeLegacyTLSPort = ""
        certificatePath = ""
        privateKeyPath = ""
        tlsClientCAPath = ""
        tlsSNIGuard = "strict"
        echKeyPath = ""
        bandwidthUp = ""
        bandwidthDown = ""
        disableLossCompensation = false
        ignoreClientBandwidth = false
        disableUDP = false
        udpIdleTimeout = ""
        speedTest = false
        obfsType = "none"
        obfsPassword = ""
        obfsMinPacket = "512"
        obfsMaxPacket = "1200"
        congestionType = "bbr"
        bbrProfile = "standard"
        aclInline = ""
        aclFileReference = ""
        geoIPReference = ""
        geoSiteReference = ""
        geoUpdateInterval = ""
        quicIdleTimeout = ""
        quicMaxStreams = ""
        quicInitStreamWindow = ""
        quicMaxStreamWindow = ""
        quicInitConnectionWindow = ""
        quicMaxConnectionWindow = ""
        disablePathMTU = false
        disableStatelessReset = false
        resolverType = "none"
        resolverAddress = ""
        resolverSNI = ""
        resolverTimeout = ""
        resolverInsecure = false
        sniffEnabled = false
        sniffTimeout = ""
        sniffRewriteDomain = false
        sniffTCPPorts = ""
        sniffUDPPorts = ""
        hasOutbound = false
        outboundDrafts = []
        masqueradeType = "none"
        masqueradeDirectory = ""
        masqueradeURL = ""
        masqueradeContent = ""
        masqueradeHeaderEntries = []
        masqueradeStatusCode = ""
        masqueradeRewriteHost = false
        masqueradeXForwarded = false
        masqueradeInsecure = false
        masqueradeListenHTTP = ""
        masqueradeListenHTTPS = ""
        masqueradeForceHTTPS = false
        resources = []
        showingResourceImporter = false
        resourceKind = "certificate"
        resourceMessage = nil
    }

    private func load() async {
        isLoading = true
        resetForm()
        defer { isLoading = false }
        do {
            let loaded = try await store.nodeDetail(nodeID)
            let loadedResources = try await store.nodeResources(nodeID)
            detail = loaded
            resources = loadedResources
            listenAddress = loaded.connection.listenAddress
            proxyProbeURL = loaded.proxyProbeUrl ?? ""
            let config = loaded.config
            echKeyPath = config["ech"]?.objectValue?["keyPath"]?.stringValue ?? ""
            if let acme = config["acme"]?.objectValue {
                tlsMode = "acme"
                if case .array(let domains) = acme["domains"] {
                    acmeDomains = domains.compactMap(\.stringValue).map(StringListEntry.init(value:))
                }
                acmeEmail = acme["email"]?.stringValue ?? ""
                acmeType = acme["type"]?.stringValue ?? "http"
                acmeCA = acme["ca"]?.stringValue ?? ""
                acmeListenHost = acme["listenHost"]?.stringValue ?? ""
                acmeDirectory = acme["dir"]?.stringValue ?? ""
                acmeLegacyMode = acme["type"] == nil && ["disableHTTP", "disableTLSALPN", "altHTTPPort", "altTLSALPNPort"].contains(where: { acme[$0] != nil })
                acmeDisableHTTP = acme["disableHTTP"]?.boolValue ?? false
                acmeDisableTLSALPN = acme["disableTLSALPN"]?.boolValue ?? false
                acmeLegacyHTTPPort = integerText(acme["altHTTPPort"])
                acmeLegacyTLSPort = integerText(acme["altTLSALPNPort"])
                if let http = acme["http"]?.objectValue { acmeHTTPAltPort = integerText(http["altPort"]) }
                if let tls = acme["tls"]?.objectValue { acmeTLSAltPort = integerText(tls["altPort"]) }
                if let dns = acme["dns"]?.objectValue {
                    acmeDNSName = dns["name"]?.stringValue ?? ""
                    acmeDNSEntries = mapEntries(dns["config"])
                }
            } else if let tls = config["tls"]?.objectValue {
                tlsMode = "tls"
                certificatePath = tls["cert"]?.stringValue ?? ""
                privateKeyPath = tls["key"]?.stringValue ?? ""
                tlsClientCAPath = tls["clientCA"]?.stringValue ?? ""
                tlsSNIGuard = tls["sniGuard"]?.stringValue ?? "strict"
            }
            if let bandwidth = config["bandwidth"]?.objectValue {
                bandwidthUp = bandwidth["up"]?.stringValue ?? ""
                bandwidthDown = bandwidth["down"]?.stringValue ?? ""
                disableLossCompensation = bandwidth["disableLossCompensation"]?.boolValue ?? false
            }
            ignoreClientBandwidth = config["ignoreClientBandwidth"]?.boolValue ?? false
            disableUDP = config["disableUDP"]?.boolValue ?? false
            udpIdleTimeout = config["udpIdleTimeout"]?.stringValue ?? ""
            speedTest = config["speedTest"]?.boolValue ?? false
            if let obfs = config["obfs"]?.objectValue {
                obfsType = obfs["type"]?.stringValue ?? "none"
                if let block = obfs[obfsType]?.objectValue {
                    obfsPassword = block["password"]?.stringValue ?? ""
                    if case .integer(let min) = block["minPacketSize"] { obfsMinPacket = String(min) }
                    if case .integer(let max) = block["maxPacketSize"] { obfsMaxPacket = String(max) }
                }
            }
            if let congestion = config["congestion"]?.objectValue {
                congestionType = congestion["type"]?.stringValue ?? "bbr"
                bbrProfile = congestion["bbrProfile"]?.stringValue ?? "standard"
            }
            if let acl = config["acl"]?.objectValue, case .array(let rules) = acl["inline"] {
                aclInline = rules.compactMap(\.stringValue).joined(separator: "\n")
            }
            if let acl = config["acl"]?.objectValue {
                aclFileReference = acl["file"]?.stringValue ?? ""
                geoIPReference = acl["geoip"]?.stringValue ?? ""
                geoSiteReference = acl["geosite"]?.stringValue ?? ""
                geoUpdateInterval = acl["geoUpdateInterval"]?.stringValue ?? ""
            }
            if let quic = config["quic"]?.objectValue {
                quicIdleTimeout = quic["maxIdleTimeout"]?.stringValue ?? ""
                quicMaxStreams = integerText(quic["maxIncomingStreams"])
                quicInitStreamWindow = integerText(quic["initStreamReceiveWindow"])
                quicMaxStreamWindow = integerText(quic["maxStreamReceiveWindow"])
                quicInitConnectionWindow = integerText(quic["initConnReceiveWindow"])
                quicMaxConnectionWindow = integerText(quic["maxConnReceiveWindow"])
                disablePathMTU = quic["disablePathMTUDiscovery"]?.boolValue ?? false
                disableStatelessReset = quic["disableStatelessReset"]?.boolValue ?? false
            }
            if let resolver = config["resolver"]?.objectValue {
                resolverType = resolver["type"]?.stringValue ?? "none"
                if let settings = resolver[resolverType]?.objectValue {
                    resolverAddress = settings["addr"]?.stringValue ?? ""
                    resolverSNI = settings["sni"]?.stringValue ?? ""
                    resolverTimeout = settings["timeout"]?.stringValue ?? ""
                    resolverInsecure = settings["insecure"]?.boolValue ?? false
                }
            }
            if let sniff = config["sniff"]?.objectValue {
                sniffEnabled = sniff["enable"]?.boolValue ?? false
                sniffTimeout = sniff["timeout"]?.stringValue ?? ""
                sniffRewriteDomain = sniff["rewriteDomain"]?.boolValue ?? false
                sniffTCPPorts = sniff["tcpPorts"]?.stringValue ?? ""
                sniffUDPPorts = sniff["udpPorts"]?.stringValue ?? ""
            }
            if case .array(let outbounds) = config["outbounds"] {
                outboundDrafts = outbounds.compactMap(OutboundDraft.init(value:))
                hasOutbound = !outboundDrafts.isEmpty
            }
            if let masquerade = config["masquerade"]?.objectValue {
                masqueradeType = masquerade["type"]?.stringValue ?? "none"
                masqueradeListenHTTP = masquerade["listenHTTP"]?.stringValue ?? ""
                masqueradeListenHTTPS = masquerade["listenHTTPS"]?.stringValue ?? ""
                masqueradeForceHTTPS = masquerade["forceHTTPS"]?.boolValue ?? false
                if let settings = masquerade[masqueradeType]?.objectValue {
                    masqueradeDirectory = settings["dir"]?.stringValue ?? ""
                    masqueradeURL = settings["url"]?.stringValue ?? ""
                    masqueradeContent = settings["content"]?.stringValue ?? ""
                    masqueradeRewriteHost = settings["rewriteHost"]?.boolValue ?? false
                    masqueradeXForwarded = settings["xForwarded"]?.boolValue ?? false
                    masqueradeInsecure = settings["insecure"]?.boolValue ?? false
                    masqueradeStatusCode = integerText(settings["statusCode"])
                    masqueradeHeaderEntries = mapEntries(settings["headers"])
                }
            }
        } catch { errorMessage = error.localizedDescription }
    }

    private func handleResourceImport(_ result: Result<[URL], Error>) {
        do {
            guard let url = try result.get().first else { return }
            let hasAccess = url.startAccessingSecurityScopedResource()
            defer { if hasAccess { url.stopAccessingSecurityScopedResource() } }
            let data = try Data(contentsOf: url, options: .mappedIfSafe)
            guard !data.isEmpty, data.count <= 20 * 1024 * 1024 else {
                errorMessage = "资源文件须在 1 字节到 20 MiB 之间。"
                return
            }
            Task {
                do {
                    let receipt = try await store.uploadResource(
                        nodeID: nodeID,
                        name: url.lastPathComponent,
                        kind: resourceKind,
                        data: data
                    )
                    copy(receipt.reference)
                    resources = try await store.nodeResources(nodeID)
                    resourceMessage = "已加密上传，引用已复制：\(receipt.reference)"
                } catch { errorMessage = error.localizedDescription }
            }
        } catch { errorMessage = error.localizedDescription }
    }

    private func copy(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    private func integerText(_ value: JSONValue?) -> String {
        guard case .integer(let integer) = value else { return "" }
        return String(integer)
    }

    private func setInteger(_ value: String, field: String, in object: inout [String: JSONValue]) -> Bool {
        if value.isEmpty {
            object.removeValue(forKey: field)
            return true
        }
        guard let integer = Int(value), integer >= 0 else {
            errorMessage = "\(field)须为非负整数。"
            return false
        }
        object[field] = .integer(integer)
        return true
    }

    private func durationMilliseconds(_ value: String) -> Double? {
        var remaining = value.trimmingCharacters(in: .whitespacesAndNewlines)
        if remaining.hasPrefix("+") { remaining.removeFirst() }
        guard !remaining.isEmpty, !remaining.hasPrefix("-") else { return nil }
        let numeric = CharacterSet(charactersIn: "0123456789.")
        let units: [(String, Double)] = [
            ("ns", 0.000_001),
            ("us", 0.001),
            ("µs", 0.001),
            ("μs", 0.001),
            ("ms", 1),
            ("s", 1_000),
            ("m", 60_000),
            ("h", 3_600_000)
        ]
        var totalMilliseconds = 0.0
        while !remaining.isEmpty {
            let split = remaining.rangeOfCharacter(from: numeric.inverted)?.lowerBound ?? remaining.endIndex
            guard split != remaining.startIndex else { return nil }
            var number = String(remaining[..<split])
            if number.hasPrefix(".") { number = "0" + number }
            if number.hasSuffix(".") { number += "0" }
            guard let amount = Double(number), amount.isFinite, amount >= 0 else { return nil }
            let unitRemainder = remaining[split...]
            guard let (unit, multiplier) = units.first(where: { unitRemainder.hasPrefix($0.0) }) else {
                return nil
            }
            totalMilliseconds += amount * multiplier
            guard totalMilliseconds.isFinite else { return nil }
            remaining = String(unitRemainder.dropFirst(unit.count))
        }
        return totalMilliseconds
    }

    private func validateDurationRange(
        _ value: String,
        field: String,
        minimumMilliseconds: Double,
        maximumMilliseconds: Double
    ) -> Bool {
        guard let milliseconds = durationMilliseconds(value),
              milliseconds == 0 || (minimumMilliseconds...maximumMilliseconds).contains(milliseconds) else {
            errorMessage = "\(field)须为0，或在 \(Int(minimumMilliseconds))ms 到 \(Int(maximumMilliseconds))ms 之间。"
            return false
        }
        return true
    }

    private func bandwidthRate(_ value: String) -> (amount: UInt64, bytesPerSecond: UInt64)? {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let numeric = CharacterSet(charactersIn: "0123456789")
        let split = value.rangeOfCharacter(from: numeric.inverted)?.lowerBound ?? value.endIndex
        guard split != value.startIndex,
              let amount = UInt64(value[..<split]) else { return nil }
        let multiplier: UInt64
        switch value[split...].trimmingCharacters(in: .whitespacesAndNewlines) {
        case "b", "bps": multiplier = 1
        case "k", "kb", "kbps": multiplier = 1_000
        case "m", "mb", "mbps": multiplier = 1_000_000
        case "g", "gb", "gbps": multiplier = 1_000_000_000
        case "t", "tb", "tbps": multiplier = 1_000_000_000_000
        default: return nil
        }
        let bitsPerSecond = amount.multipliedReportingOverflow(by: multiplier)
        guard !bitsPerSecond.overflow else { return nil }
        return (amount, bitsPerSecond.partialValue / 8)
    }

    private func validateBandwidthValue(_ value: String, field: String) -> Bool {
        guard let rate = bandwidthRate(value) else {
            errorMessage = "\(field)须为整数，并使用 bps、kbps、Mbps、Gbps 或 Tbps 单位。"
            return false
        }
        if rate.amount > 0 && rate.bytesPerSecond < 65_536 {
            errorMessage = "\(field)换算后须至少为65,536字节/秒。"
            return false
        }
        return true
    }

    private func setPort(_ value: String, field: String, key: String, in object: inout [String: JSONValue]) -> Bool {
        if value.isEmpty {
            object.removeValue(forKey: key)
            return true
        }
        guard let port = Int(value), (1...65535).contains(port) else {
            errorMessage = "\(field)必须在 1 到 65535 之间。"
            return false
        }
        object[key] = .integer(port)
        return true
    }

    private func mapEntries(_ value: JSONValue?) -> [StringMapEntry] {
        guard let object = value?.objectValue else { return [] }
        return object.keys.sorted().compactMap { key in
            guard let value = object[key]?.stringValue else { return nil }
            return StringMapEntry(key: key, value: value)
        }
    }

    private func stringMap(_ entries: [StringMapEntry]) throws -> [String: JSONValue] {
        var values: [String: JSONValue] = [:]
        for entry in entries {
            let key = entry.key.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !key.isEmpty, values[key] == nil else {
                throw MapEditorError.emptyOrDuplicateKey
            }
            values[key] = .string(entry.value)
        }
        return values
    }

    private func moveOutboundToDefault(_ id: UUID) {
        guard let index = outboundDrafts.firstIndex(where: { $0.id == id }), index > 0 else { return }
        let outbound = outboundDrafts.remove(at: index)
        outboundDrafts.insert(outbound, at: 0)
    }

    private func removeOutbound(_ id: UUID) {
        outboundDrafts.removeAll { $0.id == id }
        if outboundDrafts.isEmpty { hasOutbound = false }
    }

    private func save() {
        guard let detail else { return }
        if let validationError = ProxyProbeURLValidation.error(proxyProbeURL) {
            errorMessage = validationError
            return
        }
        if !bandwidthUp.isEmpty && !validateBandwidthValue(bandwidthUp, field: "上传带宽") { return }
        if !bandwidthDown.isEmpty && !validateBandwidthValue(bandwidthDown, field: "下载带宽") { return }
        var config = detail.config

        if tlsMode == "acme" {
            let domains = acmeDomains.map {
                $0.value.trimmingCharacters(in: .whitespacesAndNewlines)
            }.filter { !$0.isEmpty }
            guard !domains.isEmpty else {
                errorMessage = "请填写 ACME 域名。"
                return
            }
            guard Set(domains.map { $0.lowercased() }).count == domains.count else {
                errorMessage = "ACME 域名不能重复。"
                return
            }
            var acme = config["acme"]?.objectValue ?? [:]
            acme["domains"] = .array(domains.map(JSONValue.string))
            if acmeLegacyMode { acme.removeValue(forKey: "type") }
            else { acme["type"] = .string(acmeType) }
            if !acmeEmail.isEmpty { acme["email"] = .string(acmeEmail) }
            else { acme.removeValue(forKey: "email") }
            if acmeCA.isEmpty { acme.removeValue(forKey: "ca") }
            else { acme["ca"] = .string(acmeCA) }
            if acmeListenHost.isEmpty { acme.removeValue(forKey: "listenHost") }
            else { acme["listenHost"] = .string(acmeListenHost) }
            if acmeDirectory.isEmpty { acme.removeValue(forKey: "dir") }
            else { acme["dir"] = .string(acmeDirectory) }
            if acmeLegacyMode {
                acme["disableHTTP"] = .bool(acmeDisableHTTP)
                acme["disableTLSALPN"] = .bool(acmeDisableTLSALPN)
                guard setPort(acmeLegacyHTTPPort, field: "旧版 HTTP-01 备用端口", key: "altHTTPPort", in: &acme),
                      setPort(acmeLegacyTLSPort, field: "旧版 TLS-ALPN-01 备用端口", key: "altTLSALPNPort", in: &acme) else { return }
                acme.removeValue(forKey: "http")
                acme.removeValue(forKey: "tls")
                acme.removeValue(forKey: "dns")
            } else {
                for field in ["disableHTTP", "disableTLSALPN", "altHTTPPort", "altTLSALPNPort"] { acme.removeValue(forKey: field) }
                if acmeType == "http" {
                    var settings = acme["http"]?.objectValue ?? [:]
                    guard setPort(acmeHTTPAltPort, field: "HTTP-01 备用端口", key: "altPort", in: &settings) else { return }
                    acme["http"] = settings.isEmpty ? nil : .object(settings)
                    acme.removeValue(forKey: "tls")
                    acme.removeValue(forKey: "dns")
                } else if acmeType == "tls" {
                    var settings = acme["tls"]?.objectValue ?? [:]
                    guard setPort(acmeTLSAltPort, field: "TLS-ALPN-01 备用端口", key: "altPort", in: &settings) else { return }
                    acme["tls"] = settings.isEmpty ? nil : .object(settings)
                    acme.removeValue(forKey: "http")
                    acme.removeValue(forKey: "dns")
                } else {
                    guard !acmeDNSName.trimmingCharacters(in: .whitespaces).isEmpty else { errorMessage = "请选择 DNS 服务商。"; return }
                    do {
                        acme["dns"] = .object([
                            "name": .string(acmeDNSName),
                            "config": .object(try stringMap(acmeDNSEntries))
                        ])
                    } catch { errorMessage = "ACME DNS 参数名不能为空或重复。"; return }
                    acme.removeValue(forKey: "http")
                    acme.removeValue(forKey: "tls")
                }
            }
            config["acme"] = .object(acme)
            config.removeValue(forKey: "tls")
        } else if tlsMode == "tls" {
            guard !certificatePath.isEmpty, !privateKeyPath.isEmpty else {
                errorMessage = "请填写证书和私钥路径或资源引用。"
                return
            }
            var tls = config["tls"]?.objectValue ?? [:]
            tls["cert"] = .string(certificatePath)
            tls["key"] = .string(privateKeyPath)
            tls["sniGuard"] = .string(tlsSNIGuard)
            if tlsClientCAPath.isEmpty { tls.removeValue(forKey: "clientCA") }
            else { tls["clientCA"] = .string(tlsClientCAPath) }
            config["tls"] = .object(tls)
            config.removeValue(forKey: "acme")
        } else {
            config.removeValue(forKey: "acme")
            config.removeValue(forKey: "tls")
        }
        if echKeyPath.isEmpty {
            config.removeValue(forKey: "ech")
        } else {
            config["ech"] = .object(["keyPath": .string(echKeyPath)])
        }
        var bandwidth = config["bandwidth"]?.objectValue ?? [:]
        if !bandwidthUp.isEmpty { bandwidth["up"] = .string(bandwidthUp) }
        else { bandwidth.removeValue(forKey: "up") }
        if !bandwidthDown.isEmpty { bandwidth["down"] = .string(bandwidthDown) }
        else { bandwidth.removeValue(forKey: "down") }
        if disableLossCompensation { bandwidth["disableLossCompensation"] = .bool(true) }
        else { bandwidth.removeValue(forKey: "disableLossCompensation") }
        if !bandwidth.isEmpty { config["bandwidth"] = .object(bandwidth) }
        else { config.removeValue(forKey: "bandwidth") }
        var congestion = config["congestion"]?.objectValue ?? [:]
        congestion["type"] = .string(congestionType)
        congestion["bbrProfile"] = .string(bbrProfile)
        config["congestion"] = .object(congestion)
        config["disableUDP"] = .bool(disableUDP)
        config["ignoreClientBandwidth"] = .bool(ignoreClientBandwidth)
        config["speedTest"] = .bool(speedTest)
        if udpIdleTimeout.isEmpty {
            config.removeValue(forKey: "udpIdleTimeout")
        } else {
            guard validateDurationRange(
                udpIdleTimeout,
                field: "UDP 空闲超时",
                minimumMilliseconds: 2_000,
                maximumMilliseconds: 600_000
            ) else { return }
            config["udpIdleTimeout"] = .string(udpIdleTimeout)
        }
        if obfsType != "none" {
            guard !obfsPassword.isEmpty else { errorMessage = "请设置混淆密码。"; return }
            var obfs = config["obfs"]?.objectValue ?? [:]
            var block = obfs[obfsType]?.objectValue ?? [:]
            block["password"] = .string(obfsPassword)
            if obfsType == "gecko" {
                guard let min = Int(obfsMinPacket), let max = Int(obfsMaxPacket), min >= 512, max >= min, max <= 2048 else {
                    errorMessage = "Gecko 分片范围须满足 512 ≤ 最小值 ≤ 最大值 ≤ 2048。"
                    return
                }
                block["minPacketSize"] = .integer(min)
                block["maxPacketSize"] = .integer(max)
            }
            obfs["type"] = .string(obfsType)
            obfs[obfsType] = .object(block)
            config["obfs"] = .object(obfs)
        } else {
            config.removeValue(forKey: "obfs")
        }
        let rules = aclInline.split(whereSeparator: \.isNewline).map { JSONValue.string(String($0)) }
        var acl = config["acl"]?.objectValue ?? [:]
        if !aclFileReference.isEmpty { acl["file"] = .string(aclFileReference) }
        else { acl.removeValue(forKey: "file") }
        if !geoIPReference.isEmpty { acl["geoip"] = .string(geoIPReference) }
        else { acl.removeValue(forKey: "geoip") }
        if !geoSiteReference.isEmpty { acl["geosite"] = .string(geoSiteReference) }
        else { acl.removeValue(forKey: "geosite") }
        if !geoUpdateInterval.isEmpty { acl["geoUpdateInterval"] = .string(geoUpdateInterval) }
        else { acl.removeValue(forKey: "geoUpdateInterval") }
        if !aclFileReference.isEmpty && !rules.isEmpty {
            errorMessage = "acl.file 和内联规则不能同时启用。"
            return
        }
        if !rules.isEmpty { acl["inline"] = .array(rules) }
        else { acl.removeValue(forKey: "inline") }
        if !acl.isEmpty { config["acl"] = .object(acl) }
        else { config.removeValue(forKey: "acl") }

        var quic = config["quic"]?.objectValue ?? [:]
        guard setInteger(quicInitStreamWindow, field: "initStreamReceiveWindow", in: &quic),
              setInteger(quicMaxStreamWindow, field: "maxStreamReceiveWindow", in: &quic),
              setInteger(quicInitConnectionWindow, field: "initConnReceiveWindow", in: &quic),
              setInteger(quicMaxConnectionWindow, field: "maxConnReceiveWindow", in: &quic),
              setInteger(quicMaxStreams, field: "maxIncomingStreams", in: &quic) else { return }
        for (text, field) in [
            (quicInitStreamWindow, "初始流接收窗口"),
            (quicMaxStreamWindow, "最大流接收窗口"),
            (quicInitConnectionWindow, "初始连接接收窗口"),
            (quicMaxConnectionWindow, "最大连接接收窗口")
        ] {
            if let value = Int(text), value > 0, value < 16_384 {
                errorMessage = "\(field)须为0，或至少16,384字节。"
                return
            }
        }
        let initialStreamWindow = Int(quicInitStreamWindow).flatMap { $0 > 0 ? $0 : nil } ?? 8_388_608
        let maximumStreamWindow = Int(quicMaxStreamWindow).flatMap { $0 > 0 ? $0 : nil } ?? 8_388_608
        let initialConnectionWindow = Int(quicInitConnectionWindow).flatMap { $0 > 0 ? $0 : nil } ?? 20_971_520
        let maximumConnectionWindow = Int(quicMaxConnectionWindow).flatMap { $0 > 0 ? $0 : nil } ?? 20_971_520
        guard initialStreamWindow <= maximumStreamWindow else {
            errorMessage = "初始流接收窗口不能大于最大流接收窗口。"
            return
        }
        guard initialConnectionWindow <= maximumConnectionWindow else {
            errorMessage = "初始连接接收窗口不能大于最大连接接收窗口。"
            return
        }
        if let streams = Int(quicMaxStreams), streams > 0, streams < 8 {
            errorMessage = "最大并发流须为0，或至少为8。"
            return
        }
        if quicIdleTimeout.isEmpty { quic.removeValue(forKey: "maxIdleTimeout") }
        else {
            guard validateDurationRange(
                quicIdleTimeout,
                field: "QUIC 空闲超时",
                minimumMilliseconds: 4_000,
                maximumMilliseconds: 120_000
            ) else { return }
            quic["maxIdleTimeout"] = .string(quicIdleTimeout)
        }
        if disablePathMTU { quic["disablePathMTUDiscovery"] = .bool(true) }
        else { quic.removeValue(forKey: "disablePathMTUDiscovery") }
        if disableStatelessReset { quic["disableStatelessReset"] = .bool(true) }
        else { quic.removeValue(forKey: "disableStatelessReset") }
        if !quic.isEmpty { config["quic"] = .object(quic) }
        else { config.removeValue(forKey: "quic") }

        if resolverType == "none" {
            config.removeValue(forKey: "resolver")
        } else {
            guard !resolverAddress.isEmpty else { errorMessage = "请填写 DNS 解析器地址。"; return }
            var resolver = config["resolver"]?.objectValue ?? [:]
            var settings = resolver[resolverType]?.objectValue ?? [:]
            settings["addr"] = .string(resolverAddress)
            if resolverTimeout.isEmpty { settings.removeValue(forKey: "timeout") }
            else { settings["timeout"] = .string(resolverTimeout) }
            if resolverType == "tls" || resolverType == "https" {
                if resolverSNI.isEmpty { settings.removeValue(forKey: "sni") }
                else { settings["sni"] = .string(resolverSNI) }
                if resolverInsecure { settings["insecure"] = .bool(true) }
                else { settings.removeValue(forKey: "insecure") }
            }
            resolver["type"] = .string(resolverType)
            resolver[resolverType] = .object(settings)
            config["resolver"] = .object(resolver)
        }

        if sniffEnabled {
            var sniff: [String: JSONValue] = ["enable": .bool(true)]
            if !sniffTimeout.isEmpty { sniff["timeout"] = .string(sniffTimeout) }
            if sniffRewriteDomain { sniff["rewriteDomain"] = .bool(true) }
            if !sniffTCPPorts.isEmpty { sniff["tcpPorts"] = .string(sniffTCPPorts) }
            if !sniffUDPPorts.isEmpty { sniff["udpPorts"] = .string(sniffUDPPorts) }
            config["sniff"] = .object(sniff)
        } else {
            config.removeValue(forKey: "sniff")
        }

        if hasOutbound {
            guard !outboundDrafts.isEmpty else {
                errorMessage = "至少需要一个出站配置。"
                return
            }
            var names = Set<String>()
            var entries: [JSONValue] = []
            for outbound in outboundDrafts {
                let name = outbound.name.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !name.isEmpty else { errorMessage = "请填写每个出站名称。"; return }
                guard names.insert(name.lowercased()).inserted else {
                    errorMessage = "出站名称不能重复。"
                    return
                }
                var entry: [String: JSONValue] = ["name": .string(name), "type": .string(outbound.type)]
                if outbound.type == "socks5" {
                    guard !outbound.address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                        errorMessage = "请填写 SOCKS5 地址。"
                        return
                    }
                    var settings: [String: JSONValue] = ["addr": .string(outbound.address)]
                    if !outbound.username.isEmpty { settings["username"] = .string(outbound.username) }
                    if !outbound.password.isEmpty { settings["password"] = .string(outbound.password) }
                    entry["socks5"] = .object(settings)
                } else if outbound.type == "http" {
                    guard !outbound.url.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                        errorMessage = "请填写 HTTP(S) 代理 URL。"
                        return
                    }
                    var settings: [String: JSONValue] = ["url": .string(outbound.url)]
                    if outbound.insecure { settings["insecure"] = .bool(true) }
                    entry["http"] = .object(settings)
                } else {
                    var direct: [String: JSONValue] = [:]
                    if !outbound.directMode.isEmpty { direct["mode"] = .string(outbound.directMode) }
                    if !outbound.bindIPv4.isEmpty { direct["bindIPv4"] = .string(outbound.bindIPv4) }
                    if !outbound.bindIPv6.isEmpty { direct["bindIPv6"] = .string(outbound.bindIPv6) }
                    if !outbound.bindDevice.isEmpty { direct["bindDevice"] = .string(outbound.bindDevice) }
                    if outbound.fastOpen { direct["fastOpen"] = .bool(true) }
                    if !direct.isEmpty { entry["direct"] = .object(direct) }
                }
                entries.append(.object(entry))
            }
            config["outbounds"] = .array(entries)
        } else {
            config.removeValue(forKey: "outbounds")
        }

        if masqueradeType == "none" {
            config.removeValue(forKey: "masquerade")
        } else {
            var masquerade = config["masquerade"]?.objectValue ?? [:]
            var settings = masquerade[masqueradeType]?.objectValue ?? [:]
            if masqueradeType == "file" {
                guard !masqueradeDirectory.isEmpty else { errorMessage = "请填写伪装文件目录。"; return }
                settings["dir"] = .string(masqueradeDirectory)
            } else if masqueradeType == "proxy" {
                guard !masqueradeURL.isEmpty else { errorMessage = "请填写伪装上游 URL。"; return }
                settings["url"] = .string(masqueradeURL)
                if masqueradeRewriteHost { settings["rewriteHost"] = .bool(true) }
                else { settings.removeValue(forKey: "rewriteHost") }
                if masqueradeXForwarded { settings["xForwarded"] = .bool(true) }
                else { settings.removeValue(forKey: "xForwarded") }
                if masqueradeInsecure { settings["insecure"] = .bool(true) }
                else { settings.removeValue(forKey: "insecure") }
            } else {
                settings["content"] = .string(masqueradeContent)
                do {
                    settings["headers"] = .object(try stringMap(masqueradeHeaderEntries))
                } catch { errorMessage = "伪装响应头名称不能为空或重复。"; return }
                if masqueradeStatusCode.isEmpty {
                    settings.removeValue(forKey: "statusCode")
                } else if let status = Int(masqueradeStatusCode), (100...599).contains(status) {
                    settings["statusCode"] = .integer(status)
                } else {
                    errorMessage = "HTTP 状态码必须在 100 到 599 之间。"
                    return
                }
            }
            masquerade["type"] = .string(masqueradeType)
            masquerade[masqueradeType] = .object(settings)
            if masqueradeListenHTTP.isEmpty { masquerade.removeValue(forKey: "listenHTTP") }
            else { masquerade["listenHTTP"] = .string(masqueradeListenHTTP) }
            if masqueradeListenHTTPS.isEmpty { masquerade.removeValue(forKey: "listenHTTPS") }
            else { masquerade["listenHTTPS"] = .string(masqueradeListenHTTPS) }
            if masqueradeForceHTTPS { masquerade["forceHTTPS"] = .bool(true) }
            else { masquerade.removeValue(forKey: "forceHTTPS") }
            config["masquerade"] = .object(masquerade)
        }

        isSaving = true
        Task {
            defer { isSaving = false }
            do {
                try await store.updateNodeConfig(
                    detail,
                    config: .object(config),
                    listenAddress: listenAddress,
                    proxyProbeURL: proxyProbeURL
                )
                dismiss()
            } catch { errorMessage = error.localizedDescription }
        }
    }
}

private enum MapEditorError: Error {
    case emptyOrDuplicateKey
}

private struct StringListEntry: Identifiable {
    let id: UUID
    var value: String

    init(value: String = "") {
        id = UUID()
        self.value = value
    }
}

private struct StringListEditor: View {
    @Binding var entries: [StringListEntry]
    let prompt: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if entries.isEmpty {
                Text("暂无条目")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            ForEach($entries) { $entry in
                HStack(spacing: 8) {
                    TextField(prompt, text: $entry.value)
                    Button(role: .destructive) {
                        let id = entry.id
                        entries.removeAll { $0.id == id }
                    } label: {
                        Label("删除条目", systemImage: "minus.circle")
                            .labelStyle(.iconOnly)
                    }
                    .accessibilityLabel("删除条目")
                }
            }
            Button("添加条目", systemImage: "plus") {
                entries.append(StringListEntry())
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct StringMapEntry: Identifiable {
    let id: UUID
    var key: String
    var value: String

    init(key: String = "", value: String = "") {
        id = UUID()
        self.key = key
        self.value = value
    }
}

private struct StringMapEditor: View {
    @Binding var entries: [StringMapEntry]
    let keyPrompt: String
    let valuePrompt: String
    let masksValues: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if entries.isEmpty {
                Text("暂无映射项")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            ForEach($entries) { $entry in
                HStack(spacing: 8) {
                    TextField(keyPrompt, text: $entry.key)
                        .frame(minWidth: 100)
                    if masksValues {
                        SecureField(valuePrompt, text: $entry.value)
                            .frame(minWidth: 140)
                    } else {
                        TextField(valuePrompt, text: $entry.value)
                            .frame(minWidth: 140)
                    }
                    Button(role: .destructive) {
                        let id = entry.id
                        entries.removeAll { $0.id == id }
                    } label: {
                        Label("删除映射项", systemImage: "minus.circle")
                            .labelStyle(.iconOnly)
                    }
                    .accessibilityLabel("删除映射项")
                }
            }
            Button("添加映射项", systemImage: "plus") {
                entries.append(StringMapEntry())
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct OutboundDraft: Identifiable {
    let id: UUID
    var name: String
    var type: String
    var address: String
    var username: String
    var password: String
    var url: String
    var insecure: Bool
    var directMode: String
    var bindIPv4: String
    var bindIPv6: String
    var bindDevice: String
    var fastOpen: Bool

    init() {
        id = UUID()
        name = "default"
        type = "direct"
        address = ""
        username = ""
        password = ""
        url = ""
        insecure = false
        directMode = ""
        bindIPv4 = ""
        bindIPv6 = ""
        bindDevice = ""
        fastOpen = false
    }

    init?(value: JSONValue) {
        guard let entry = value.objectValue,
              let type = entry["type"]?.stringValue,
              ["direct", "socks5", "http"].contains(type) else { return nil }
        let settings = entry[type]?.objectValue ?? [:]
        let direct = entry["direct"]?.objectValue ?? [:]
        id = UUID()
        name = entry["name"]?.stringValue ?? "default"
        self.type = type
        address = settings["addr"]?.stringValue ?? ""
        username = settings["username"]?.stringValue ?? ""
        password = settings["password"]?.stringValue ?? ""
        url = settings["url"]?.stringValue ?? ""
        insecure = settings["insecure"]?.boolValue ?? false
        directMode = direct["mode"]?.stringValue ?? ""
        bindIPv4 = direct["bindIPv4"]?.stringValue ?? ""
        bindIPv6 = direct["bindIPv6"]?.stringValue ?? ""
        bindDevice = direct["bindDevice"]?.stringValue ?? ""
        fastOpen = direct["fastOpen"]?.boolValue ?? false
    }
}
