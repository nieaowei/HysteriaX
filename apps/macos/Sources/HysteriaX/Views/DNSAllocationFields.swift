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
        Picker(L10n.text("地址方式"), selection: $draft.mode) {
            if allowsExternal { Text(L10n.text("外部域名 / IP")).tag("external") }
            Text(L10n.text("自动分配域名")).tag("auto")
            Text(L10n.text("手动指定域名")).tag("manual")
            Text(L10n.text("选择已有记录")).tag("existing")
        }.accessibilityIdentifier("dns.allocation.mode")
            .onChange(of: sshHost, initial: true) { previous, current in
                draft.updateSSHAddress(from: previous, to: current)
            }
        if draft.mode != "external" {
            Picker(L10n.text("域名区域"), selection: $draft.zoneID) {
                Text(L10n.text("选择域名区域")).tag("")
                ForEach(store.dnsZones.filter(\.enabled)) { zone in Text(zone.name).tag(zone.id) }
            }.accessibilityIdentifier("dns.allocation.zone")
            if store.dnsZones.filter(\.enabled).isEmpty {
                Button(L10n.text("配置 DNS 连接与域名区域…")) { store.showDNSRecord(nil) }
            }
            if draft.mode == "auto" {
                TextField(L10n.text("域名前缀"), text: $draft.prefix)
                if let zone = store.dnsZones.first(where: { $0.id == draft.zoneID }) {
                    Text(L10n.text("域名格式：{0}-<节点 ID>.{1}", String(describing: (draft.prefix.isEmpty ? "node" : draft.prefix)), String(describing: (zone.name)))).font(.caption).foregroundStyle(.secondary)
                }
            } else if draft.mode == "manual" {
                TextField(L10n.text("子域名或完整域名"), text: $draft.hostname).accessibilityIdentifier("dns.allocation.hostname")
            } else {
                Picker(L10n.text("已有域名"), selection: $draft.selectedHostname) {
                    Text(L10n.text("选择记录")).tag("")
                    ForEach(availableNames, id: \.self) { name in Text(name).tag(name) }
                }
                Text(L10n.text("保留已有记录的目标和 TTL。节点使用 DNS only 记录。")).font(.caption).foregroundStyle(.secondary)
            }
            if draft.mode != "existing" {
                TextField(L10n.text("公网 IPv4"), text: $draft.ipv4).accessibilityIdentifier("dns.allocation.ipv4")
                TextField(L10n.text("公网 IPv6"), text: $draft.ipv6).accessibilityIdentifier("dns.allocation.ipv6")
                Text(L10n.text("公网 IPv4 和 IPv6 至少填写一个，也可同时填写。"))
                    .font(.caption).foregroundStyle(.secondary)
                Text(L10n.text("域名分配后，在代理配置中设置 ACME 或已有 TLS 证书，再部署节点。")).font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
