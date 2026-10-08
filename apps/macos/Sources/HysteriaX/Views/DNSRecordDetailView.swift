import SwiftUI

struct DNSRecordDetailView: View {
    let record: DNSRecord
    let zone: DNSZone?
    let connection: DNSConnection?
    let node: NodeSummary?
    let failedJob: JobSummary?
    let isConnected: Bool
    let busy: Bool
    var onEdit: () -> Void
    var onCheck: () -> Void
    var onDelete: () -> Void
    var onBind: () -> Void
    var onRetry: () -> Void
    var onOpenNode: (String) -> Void

    private var canAct: Bool { isConnected && !busy }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header.padding(16)
            Divider()
            GeometryReader { geometry in
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        notices
                        OverviewColumnsLayout(wideColumns: 2, wideMinimum: 576, narrowColumns: 1) {
                            GroupBox {
                                VStack(alignment: .leading, spacing: 8) {
                                    compactItem(L10n.text("目标"), value: record.content, monospaced: true)
                                    OverviewColumnsLayout(wideColumns: 2, wideMinimum: 0, spacing: 8) {
                                        compactItem(L10n.text("记录类型"), value: record.recordType)
                                        compactItem("TTL", value: record.ttl == 1 ? L10n.text("自动") : L10n.text("{0} 秒", String(describing: (record.ttl))))
                                        compactItem(L10n.text("代理模式"), value: record.proxied ? L10n.text("Cloudflare 代理") : L10n.text("仅 DNS"))
                                        compactItem(L10n.text("记录来源"), value: record.origin == "hysteriax" ? L10n.text("HysteriaX 创建") : L10n.text("已有记录"))
                                        compactItem(L10n.text("域名区域"), value: zone?.name ?? record.zoneId)
                                        compactItem(L10n.text("服务连接"), value: connection?.name ?? L10n.text("未找到连接"))
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(4)
                            } label: {
                                Label(L10n.text("记录信息"), systemImage: "network")
                            }
                            .frame(maxWidth: .infinity, alignment: .topLeading)
                            GroupBox {
                                VStack(alignment: .leading, spacing: 8) {
                                    OverviewPairLayout(horizontalMinimum: 0, flexibleIndex: 1, spacing: 12) {
                                        Text(L10n.text("解析状态")).font(.caption).foregroundStyle(.secondary)
                                            .frame(width: 56, alignment: .leading)
                                        Label(record.resolutionLabel, systemImage: record.resolutionStatus == "verified" ? "checkmark.circle" : "globe")
                                            .font(.callout)
                                            .foregroundStyle(record.resolutionColor)
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                    }
                                    compactField(L10n.text("最近检查"), value: record.checkedAt.map { DateDisplayText.local($0) } ?? L10n.text("尚未检查"))
                                    if let node {
                                        OverviewPairLayout(horizontalMinimum: 0, flexibleIndex: 1, spacing: 12) {
                                            Text(L10n.text("绑定节点")).font(.caption).foregroundStyle(.secondary)
                                                .frame(width: 56, alignment: .leading)
                                            Button(node.name) { onOpenNode(node.id) }
                                                .buttonStyle(.link)
                                                .font(.callout)
                                                .frame(maxWidth: .infinity, alignment: .leading)
                                                .help(L10n.text("查看节点详情"))
                                                .accessibilityIdentifier("dns.record.bound-node.\(node.id)")
                                        }
                                    } else {
                                        compactField(L10n.text("绑定节点"), value: record.boundNodeId == nil ? L10n.text("未绑定节点") : L10n.text("节点已不在当前列表"))
                                    }
                                    compactField(L10n.text("最近更新"), value: DateDisplayText.local(record.updatedAt))
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(4)
                            } label: {
                                Label(L10n.text("解析与节点"), systemImage: "point.3.connected.trianglepath.dotted")
                            }
                            .frame(maxWidth: .infinity, alignment: .topLeading)
                        }
                        if let result = record.resolutionDetail {
                            DisclosureGroup(L10n.text("解析检查结果")) {
                                VStack(alignment: .leading, spacing: 12) {
                                    field(L10n.text("检查时的预期目标"), value: result["expected"]?.stringValue ?? record.content, monospaced: true)
                                    field(L10n.text("管理服务解析结果"), value: answers(result["management_answers"]), monospaced: true)
                                    ForEach(Array((result["authoritative"]?.arrayValue ?? []).enumerated()), id: \.offset) { _, value in
                                        if let authority = value.objectValue {
                                            field(authority["server"]?.stringValue ?? L10n.text("权威服务器"),
                                                  value: authority["error"] != nil ? L10n.text("查询失败") : answers(authority["answers"]), monospaced: true)
                                            if let flattened = authority["flattened_addresses"]?.arrayValue, !flattened.isEmpty {
                                                field(L10n.text("CNAME 展平地址"), value: answers(.array(flattened)), monospaced: true)
                                            }
                                        }
                                    }
                                }
                                .padding(.top, 10)
                            }
                            .font(.callout)
                        }
                        DisclosureGroup(L10n.text("记录标识")) {
                            VStack(alignment: .leading, spacing: 12) {
                                field(L10n.text("记录 ID"), value: record.id, monospaced: true)
                                field(L10n.text("远端记录 ID"), value: record.providerRecordId ?? L10n.text("尚未写入远端"), monospaced: true)
                                field(L10n.text("版本"), value: "v\(record.revision)")
                            }
                            .padding(.top, 10)
                        }
                        .font(.callout)
                    }
                    // Give the scroll content one concrete width instead of probing its intrinsic width.
                    .frame(width: max(0, geometry.size.width - 32), alignment: .leading)
                    .padding(16)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("dns.record.detail")
    }

    private var header: some View {
        DetailHeaderLayout {
            VStack(alignment: .leading, spacing: 6) {
                Text(record.name).font(.headline).textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    DNSRecordTypeBadge(record: record)
                    Label(record.stateLabel, systemImage: record.state == "synced" ? "checkmark.circle.fill" : "circle.fill")
                        .foregroundStyle(record.syncColor)
                    if !isConnected { Label(L10n.text("离线快照"), systemImage: "wifi.slash").foregroundStyle(.secondary) }
                }
                .font(.caption)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            HStack(spacing: 8) {
                Button(L10n.text("编辑"), action: onEdit)
                    .disabled(!canAct || !record.supportsEditing || record.desired != nil)
                Button(L10n.text("检查解析"), action: onCheck)
                    .disabled(!canAct || !record.supportsEditing || record.state != "synced")
                Button(L10n.text("绑定节点"), action: onBind)
                    .disabled(!canAct || !record.supportsEditing || record.proxied || record.state != "synced" || record.boundNodeId != nil)
                if busy { ProgressView().controlSize(.small) }
                Button(L10n.text("删除记录"), role: .destructive, action: onDelete)
                    .foregroundStyle(.red)
                    .disabled(!canAct || !record.supportsEditing || record.boundNodeId != nil || record.desired != nil)
                    .accessibilityIdentifier("dns.record.delete")
            }
            .controlSize(.small)
            .fixedSize()
        }
    }

    @ViewBuilder private var notices: some View {
        if let desired = record.desired {
            notice(L10n.text("待执行变更"), symbol: "clock", color: .orange) {
                Text(desired["delete"]?.boolValue == true ? L10n.text("正在等待删除记录。") :
                        desired["content"]?.stringValue.map { L10n.text("目标将更新为 {0}", String(describing: ($0))) } ?? L10n.text("正在等待写入记录变更。"))
                    .textSelection(.enabled)
            }
        }
        if let failedJob {
            notice(L10n.text("DNS 操作失败"), symbol: "exclamationmark.triangle", color: .orange) {
                Text(failedJob.errorMessage ?? L10n.text("操作未完成，请重试。"))
                    .textSelection(.enabled)
                Button(L10n.text("重试"), systemImage: "arrow.clockwise", action: onRetry).disabled(!canAct)
            }
        }
        if !record.supportsEditing {
            notice(L10n.text("只读记录"), symbol: "lock", color: .secondary) {
                Text(L10n.text("此类型暂不支持编辑、删除和节点绑定。"))
            }
        } else if record.proxied && record.boundNodeId == nil {
            notice(L10n.text("Cloudflare 代理已开启"), symbol: "cloud", color: .secondary) {
                Text(L10n.text("绑定节点需要使用仅 DNS 模式。"))
            }
        }
    }

    private func answers(_ value: JSONValue?) -> String {
        guard let values = value?.arrayValue else { return L10n.text("未取得查询结果") }
        let strings = values.compactMap(\.stringValue)
        return strings.isEmpty ? L10n.text("无解析记录") : strings.joined(separator: "\n")
    }

    private func compactItem(_ title: String, value: String, monospaced: Bool = false) -> some View {
        compactField(title, value: value, monospaced: monospaced)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func compactField(_ title: String, value: String, monospaced: Bool = false) -> some View {
        OverviewPairLayout(horizontalMinimum: 0, flexibleIndex: 1, spacing: 12) {
            Text(title).font(.caption).foregroundStyle(.secondary)
                .frame(width: 56, alignment: .leading)
            Text(value).font(monospaced ? .system(.callout, design: .monospaced) : .callout)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func field(_ title: String, value: String, monospaced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(monospaced ? .system(.callout, design: .monospaced) : .callout)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func notice<Content: View>(_ title: String, symbol: String, color: Color, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: symbol).font(.callout.weight(.medium)).foregroundStyle(color)
            content().font(.callout)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(color.opacity(0.07), in: RoundedRectangle(cornerRadius: 8))
    }
}
