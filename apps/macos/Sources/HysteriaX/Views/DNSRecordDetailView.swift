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
    var onOpenJobs: () -> Void
    var onOpenAudit: () -> Void

    private var canAct: Bool { isConnected && !busy }
    private var stateColor: Color {
        switch record.state {
        case "synced": .green
        case "failed", "remote_missing": .orange
        case "pending": .blue
        default: .secondary
        }
    }
    private var resolutionColor: Color {
        switch record.resolutionStatus {
        case "verified": .green
        case "pending": .orange
        default: .secondary
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header.padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    notices
                    GroupBox {
                        VStack(alignment: .leading, spacing: 14) {
                            field("目标", value: record.content, monospaced: true)
                            Divider()
                            LazyVGrid(columns: [GridItem(.adaptive(minimum: 170), alignment: .leading)], alignment: .leading, spacing: 14) {
                                field("记录类型", value: record.recordType)
                                field("TTL", value: record.ttl == 1 ? "自动" : "\(record.ttl) 秒")
                                field("代理模式", value: record.proxied ? "Cloudflare 代理" : "仅 DNS")
                                field("记录来源", value: record.origin == "hysteriax" ? "HysteriaX 创建" : "已有记录")
                                field("域名区域", value: zone?.name ?? record.zoneId)
                                field("服务连接", value: connection?.name ?? "未找到连接")
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                    } label: {
                        Label("记录信息", systemImage: "network")
                    }
                    GroupBox {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), alignment: .leading)], alignment: .leading, spacing: 14) {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("解析状态").font(.caption).foregroundStyle(.secondary)
                                Label(record.resolutionLabel, systemImage: record.resolutionStatus == "verified" ? "checkmark.circle" : "globe")
                                    .foregroundStyle(resolutionColor)
                            }
                            field("最近检查", value: record.checkedAt.map { DateDisplayText.local($0) } ?? "尚未检查")
                            field("绑定节点", value: node?.name ?? (record.boundNodeId == nil ? "未绑定节点" : "节点已不在当前列表"))
                            field("最近更新", value: DateDisplayText.local(record.updatedAt))
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                    } label: {
                        Label("解析与节点", systemImage: "point.3.connected.trianglepath.dotted")
                    }
                    if let result = record.resolutionDetail {
                        DisclosureGroup("解析检查结果") {
                            VStack(alignment: .leading, spacing: 12) {
                                field("检查时的预期目标", value: result["expected"]?.stringValue ?? record.content, monospaced: true)
                                field("管理服务解析结果", value: answers(result["management_answers"]), monospaced: true)
                                ForEach(Array((result["authoritative"]?.arrayValue ?? []).enumerated()), id: \.offset) { _, value in
                                    if let authority = value.objectValue {
                                        field(authority["server"]?.stringValue ?? "权威服务器",
                                              value: authority["error"] != nil ? "查询失败" : answers(authority["answers"]), monospaced: true)
                                        if let flattened = authority["flattened_addresses"]?.arrayValue, !flattened.isEmpty {
                                            field("CNAME 展平地址", value: answers(.array(flattened)), monospaced: true)
                                        }
                                    }
                                }
                            }
                            .padding(.top, 10)
                        }
                        .font(.callout)
                    }
                    DisclosureGroup("记录标识") {
                        VStack(alignment: .leading, spacing: 12) {
                            field("记录 ID", value: record.id, monospaced: true)
                            field("远端记录 ID", value: record.providerRecordId ?? "尚未写入远端", monospaced: true)
                            field("版本", value: "v\(record.revision)")
                        }
                        .padding(.top, 10)
                    }
                    .font(.callout)
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("dns.record.detail")
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(record.name).font(.headline).textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 10) {
                        Text(record.recordType).font(.caption.monospaced().weight(.semibold))
                        Label(record.stateLabel, systemImage: record.state == "synced" ? "checkmark.circle.fill" : "circle.fill")
                            .foregroundStyle(stateColor)
                        if !isConnected { Label("离线快照", systemImage: "wifi.slash").foregroundStyle(.secondary) }
                    }
                    .font(.caption)
                }
                Spacer(minLength: 0)
                Menu {
                    Button("任务记录", systemImage: "clock.arrow.circlepath", action: onOpenJobs)
                    Button("审计记录", systemImage: "list.clipboard", action: onOpenAudit)
                    Divider()
                    Button("删除记录…", role: .destructive, action: onDelete)
                        .disabled(!canAct || !record.supportsEditing || record.boundNodeId != nil || record.desired != nil)
                } label: {
                    Label("更多操作", systemImage: "ellipsis.circle")
                        .labelStyle(.iconOnly)
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .accessibilityIdentifier("dns.record.more")
            }
            HStack(spacing: 8) {
                Button("编辑…", systemImage: "pencil", action: onEdit)
                    .disabled(!canAct || !record.supportsEditing || record.desired != nil)
                Button("检查解析", systemImage: "arrow.triangle.2.circlepath", action: onCheck)
                    .disabled(!canAct || !record.supportsEditing || record.state != "synced")
                Button("绑定节点…", systemImage: "server.rack", action: onBind)
                    .disabled(!canAct || !record.supportsEditing || record.proxied || record.state != "synced" || record.boundNodeId != nil)
                if busy { ProgressView().controlSize(.small) }
            }
            .controlSize(.small)
        }
    }

    @ViewBuilder private var notices: some View {
        if let desired = record.desired {
            notice("待执行变更", symbol: "clock", color: .orange) {
                Text(desired["delete"]?.boolValue == true ? "正在等待删除记录。" :
                        desired["content"]?.stringValue.map { "目标将更新为 \($0)" } ?? "正在等待写入记录变更。")
                    .textSelection(.enabled)
            }
        }
        if let failedJob {
            notice("DNS 操作失败", symbol: "exclamationmark.triangle", color: .orange) {
                Text(failedJob.errorMessage ?? "操作未完成，请重试。")
                    .textSelection(.enabled)
                Button("重试", systemImage: "arrow.clockwise", action: onRetry).disabled(!canAct)
            }
        }
        if !record.supportsEditing {
            notice("只读记录", symbol: "lock", color: .secondary) {
                Text("此类型暂不支持编辑、删除和节点绑定。")
            }
        } else if record.proxied && record.boundNodeId == nil {
            notice("Cloudflare 代理已开启", symbol: "cloud", color: .secondary) {
                Text("绑定节点需要使用仅 DNS 模式。")
            }
        }
    }

    private func answers(_ value: JSONValue?) -> String {
        guard let values = value?.arrayValue else { return "未取得查询结果" }
        let strings = values.compactMap(\.stringValue)
        return strings.isEmpty ? "无解析记录" : strings.joined(separator: "\n")
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
