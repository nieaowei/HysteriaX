import Foundation
import Network

struct DNSAllocationDraft {
    var mode = "external"
    var zoneID = ""
    var prefix = "node"
    var hostname = ""
    var ipv4 = ""
    var ipv6 = ""
    var selectedHostname = ""
    var idempotencyKey = UUID().uuidString

    mutating func updateSSHAddress(from previous: String, to current: String) {
        let previous = previous.trimmingCharacters(in: .whitespacesAndNewlines)
        let current = current.trimmingCharacters(in: .whitespacesAndNewlines)
        if ipv4.isEmpty || ipv4 == previous {
            ipv4 = IPv4Address(current) == nil ? "" : current
        }
        if ipv6.isEmpty || ipv6 == previous {
            ipv6 = IPv6Address(current) == nil ? "" : current
        }
    }

    func allocation(zones: [DNSZone], records: [DNSRecord]) throws -> DNSAllocation? {
        guard mode != "external" else { return nil }
        guard let zone = zones.first(where: { $0.id == zoneID && $0.enabled }) else { throw APIClientError.server(L10n.text("请选择已启用的域名区域。")) }
        var name = hostname.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if mode == "manual", !name.hasSuffix(".\(zone.name)"), name != zone.name { name += ".\(zone.name)" }
        let ids = mode == "existing" ? records.filter { $0.zoneId == zoneID && $0.name == selectedHostname && $0.state == "synced" && !$0.proxied && ["A", "AAAA", "CNAME"].contains($0.recordType) }.map(\.id) : []
        if mode == "existing", ids.isEmpty { throw APIClientError.server(L10n.text("请选择可用的 DNS 记录。")) }
        if mode == "manual", hostname.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { throw APIClientError.server(L10n.text("请输入子域名。")) }
        let ipv4 = ipv4.trimmingCharacters(in: .whitespacesAndNewlines)
        let ipv6 = ipv6.trimmingCharacters(in: .whitespacesAndNewlines)
        if mode != "existing", ipv4.isEmpty && ipv6.isEmpty { throw APIClientError.server(L10n.text("请输入至少一个公网 IPv4 或 IPv6 地址。")) }
        return DNSAllocation(idempotencyKey: idempotencyKey, zoneId: zoneID, mode: mode,
            prefix: mode == "auto" ? prefix : nil, hostname: mode == "manual" ? name : nil,
            ipv4: ipv4.isEmpty ? nil : ipv4, ipv6: ipv6.isEmpty ? nil : ipv6, recordIds: ids)
    }
}

extension DNSRecord {
    var supportsEditing: Bool { ["A", "AAAA", "CNAME"].contains(recordType) }
    var stateLabel: String {
        switch state { case "synced": L10n.text("已写入"); case "pending": L10n.text("待写入"); case "failed": L10n.text("写入失败"); case "remote_missing": L10n.text("远端已删除"); case "deleted": L10n.text("已删除"); default: state }
    }
    var resolutionLabel: String {
        switch resolutionStatus { case "verified": L10n.text("解析已验证"); case "pending": L10n.text("解析待更新"); case "unchecked": L10n.text("尚未检查"); case "proxied": L10n.text("代理模式"); default: resolutionStatus }
    }
}
