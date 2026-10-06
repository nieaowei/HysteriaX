import SwiftUI

struct DNSAllocationFields: View {
    @Bindable var store: ManagementStore
    @Binding var draft: DNSAllocationDraft
    var allowsExternal = true
    var sshHost = ""

    private var availableNames: [String] {
        Array(Set(store.dnsRecords.filter { $0.zoneId == draft.zoneID && $0.supportsEditing && !$0.proxied && $0.state == "synced" }.map(\.name))).sorted()
    }
    var body: some View {
        Picker("地址方式", selection: $draft.mode) {
            if allowsExternal { Text("外部域名 / IP").tag("external") }
            Text("自动分配域名").tag("auto")
            Text("手动指定域名").tag("manual")
            Text("选择已有记录").tag("existing")
        }.accessibilityIdentifier("dns.allocation.mode")
        if draft.mode != "external" {
            Picker("域名区域", selection: $draft.zoneID) {
                Text("选择域名区域").tag("")
                ForEach(store.dnsZones.filter(\.enabled)) { zone in Text(zone.name).tag(zone.id) }
            }.accessibilityIdentifier("dns.allocation.zone")
            if store.dnsZones.filter(\.enabled).isEmpty {
                Button("配置 DNS 连接与域名区域…") { store.showDNSRecord(nil) }
            }
            if draft.mode == "auto" {
                TextField("域名前缀", text: $draft.prefix)
                if let zone = store.dnsZones.first(where: { $0.id == draft.zoneID }) {
                    Text("域名格式：\(draft.prefix.isEmpty ? "node" : draft.prefix)-<节点 ID>.\(zone.name)").font(.caption).foregroundStyle(.secondary)
                }
            } else if draft.mode == "manual" {
                TextField("子域名或完整域名", text: $draft.hostname).accessibilityIdentifier("dns.allocation.hostname")
            } else {
                Picker("已有域名", selection: $draft.selectedHostname) {
                    Text("选择记录").tag("")
                    ForEach(availableNames, id: \.self) { name in Text(name).tag(name) }
                }
                Text("保留已有记录的目标和 TTL。节点使用 DNS only 记录。").font(.caption).foregroundStyle(.secondary)
            }
            if draft.mode != "existing" {
                TextField("公网 IPv4", text: $draft.ipv4).accessibilityIdentifier("dns.allocation.ipv4")
                TextField("公网 IPv6（可选）", text: $draft.ipv6).accessibilityIdentifier("dns.allocation.ipv6")
                if sshHost.contains(":"), !sshHost.contains(" ") {
                    Button("填入 SSH 地址作为 IPv6") { draft.ipv6 = sshHost }
                } else if sshHost.split(separator: ".").count == 4, sshHost.split(separator: ".").allSatisfy({ UInt8($0) != nil }) {
                    Button("填入 SSH 地址作为 IPv4") { draft.ipv4 = sshHost }
                }
                Text("域名分配后，在代理配置中设置 ACME 或已有 TLS 证书，再部署节点。").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
