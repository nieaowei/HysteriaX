import SwiftUI

enum OverviewMonitoringSection { case summary, metrics, issues, quota, quality }

struct OverviewMonitoringView: View {
    @Bindable var store: ManagementStore
    var navigate: (OverviewDestination) -> Void
    var section: OverviewMonitoringSection = .summary
    var showQuota: () -> Void = {}
    @State private var viewState: OverviewTabState

    init(store: ManagementStore, navigate: @escaping (OverviewDestination) -> Void,
         section: OverviewMonitoringSection = .summary, state: OverviewTabState? = nil, showQuota: @escaping () -> Void = {}) {
        self.store = store
        self.navigate = navigate
        self.section = section
        self.showQuota = showQuota
        _viewState = State(initialValue: state ?? OverviewTabState())
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            if let overview = store.overview {
                if section == .summary || section == .metrics {
                    OverviewColumnsLayout(wideColumns: 4, wideMinimum: 768) { metricCards(overview) }
                }
                if section == .summary || section == .issues { issues(overview) }
                if section == .quota {
                    quotaRank(overview)
                    packageRisks
                }
                if section == .quality {
                OverviewCard("节点监控", systemImage: "server.rack") {
                    VStack(alignment: .leading, spacing: 10) {
                        HStack {
                            Text("已采集节点在线用户 \(overview.onlineUsers.map(String.init) ?? "—") · 连接 \(overview.connections.map(String.init) ?? "—")").fontWeight(.medium)
                            Spacer()
                            Text("覆盖 \(overview.coveredNodes) / \(overview.eligibleNodes)").foregroundStyle(.secondary)
                        }
                        ForEach(overview.nodes) { node in
                            Button { viewState.inspectedNode = node } label: {
                                HStack {
                                    VStack(alignment: .leading, spacing: 3) {
                                        Text(node.name).fontWeight(.medium)
                                        Text("部署：\(OverviewDisplay.status(node.deploymentState)) · 在线统计：\(OverviewDisplay.status(node.onlineStatus))").font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    Text(OverviewDisplay.status(node.probeStatus)).foregroundStyle(node.probeStatus == "failed" ? Color.red : node.probeStatus == "healthy" ? Color.green : Color.secondary)
                                    Text(OverviewDisplay.milliseconds(node.latencyMs)).monospacedDigit().frame(width: 76, alignment: .trailing)
                                    Image(systemName: "chevron.right").foregroundStyle(.secondary)
                                }.padding(.vertical, 4).contentShape(Rectangle())
                            }.buttonStyle(.plain).accessibilityIdentifier("overview.node.\(node.nodeID)")
                        }
                        if overview.nodes.isEmpty { Text("添加并部署节点后开始监控。").foregroundStyle(.secondary) }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
                }
            } else if store.supportsOverviewMonitoring {
                OverviewCard("监控信息", systemImage: "network") { Text(store.overviewError ?? "正在获取概览监控…").frame(maxWidth: .infinity, alignment: .leading) }
            }
        }
    }

    private func metricCards(_ overview: OverviewResponse) -> some View {
        Group {
                    metric("节点健康", value: "\(overview.nodes.filter { $0.probeStatus == "healthy" }.count) / \(overview.nodeCount)", detail: "公网探测健康 / 全部节点", symbol: "server.rack")
                    metric("需处理节点", value: "\(overview.attentionNodes)", detail: "\(overview.issues.count) 项当前提醒", symbol: "exclamationmark.triangle")
                    Button(action: showQuota) { metric("套餐风险", value: "\(overview.riskNodes)", detail: "到期、额度预警或受限节点", symbol: "gauge.with.dots.needle.67percent") }.buttonStyle(.plain).accessibilityIdentifier("overview.quotaShortcut")
                    metric("任务", value: "\(overview.queuedJobs + overview.runningJobs)", detail: "排队 \(overview.queuedJobs) · 执行 \(overview.runningJobs) · 24h 失败 \(overview.failedJobs24h)", symbol: "hourglass")
        }
    }

    private func metric(_ title: String, value: String, detail: String, symbol: String) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Label(title, systemImage: symbol).foregroundStyle(.secondary)
                Text(value).font(.system(size: 28, weight: .semibold, design: .rounded)).monospacedDigit()
                Text(detail).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }.frame(maxWidth: .infinity, minHeight: 96, alignment: .leading)
        }
    }
    private func issues(_ overview: OverviewResponse) -> some View {
        OverviewCard("需处理事项", systemImage: "checklist") {
            VStack(alignment: .leading, spacing: 10) {
                if overview.issues.isEmpty { Label("暂无需处理事项", systemImage: "checkmark.circle").foregroundStyle(.secondary).padding(.vertical, 12) }
                ForEach(viewState.showAllIssues ? overview.issues : Array(overview.issues.prefix(8))) { issue in
                    Button { navigate(OverviewDestination(section: issue.entityType == "job" ? "jobs" : issue.entityType == "user" ? "users" : "nodes", entityID: issue.entityID)) } label: {
                        HStack(alignment: .top) {
                            Image(systemName: issue.severity <= 1 ? "exclamationmark.circle.fill" : "exclamationmark.triangle").foregroundStyle(issue.severity <= 1 ? Color.red : Color.orange)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(issue.name).fontWeight(.medium)
                                Text(OverviewDisplay.reason(issue)).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                                if let at = issue.occurredAt { Text(DateDisplayText.local(at)).font(.caption).foregroundStyle(.tertiary) }
                            }
                            Spacer(minLength: 8)
                            Image(systemName: "chevron.right").foregroundStyle(.secondary)
                        }.padding(.vertical, 4).contentShape(Rectangle())
                    }.buttonStyle(.plain).accessibilityIdentifier("overview.issue.\(issue.id)")
                }
                if overview.issues.count > 8 { Button(viewState.showAllIssues ? "收起" : "显示全部 \(overview.issues.count) 项") { viewState.showAllIssues.toggle() } }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }.accessibilityIdentifier("overview.issues")
    }
    private var packageRisks: some View {
        let nodes = store.nodes.filter { $0.packageUsage?.restricted == true || !($0.packageUsage?.alerts.isEmpty ?? true) }
        return OverviewCard("套餐与到期提醒", systemImage: "calendar") {
            VStack(alignment: .leading, spacing: 16) {
                if nodes.isEmpty { Text("暂无套餐或到期提醒").foregroundStyle(.secondary) }
                ForEach(nodes) { node in
                    VStack(alignment: .leading, spacing: 6) {
                        Button(node.name) { navigate(OverviewDestination(section: "nodes", entityID: node.id)) }.buttonStyle(.plain).fontWeight(.medium)
                        NodePackageStatus(package: node.package, usage: node.packageUsage)
                    }
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func quotaRank(_ overview: OverviewResponse) -> some View {
        OverviewCard("节点额度排行", systemImage: "gauge.with.dots.needle.67percent") {
            VStack(alignment: .leading, spacing: 16) {
                if overview.quotaRank.isEmpty { Text("暂无设置流量额度的节点").foregroundStyle(.secondary) }
                ForEach(overview.quotaRank) { node in
                    Button { navigate(OverviewDestination(section: "nodes", entityID: node.id)) } label: {
                        VStack(alignment: .leading, spacing: 6) {
                            HStack { Text(node.name).fontWeight(.medium); Spacer(); Text(PackageDisplay.usage(node.package, node.packageUsage)).font(.caption) }
                            if let quota = node.package?.quotaBytes, let used = node.packageUsage?.usageBytes {
                                if quota > 0 { ProgressView(value: min(1, Double(used) / Double(quota))) }
                                Text("剩余 \(OverviewDisplay.bytes(max(0, quota - used)))").font(.caption).foregroundStyle(.secondary)
                            }
                            Text("到期：\(DateDisplayText.local(node.package?.expiresAt)) · 重置：\(DateDisplayText.local(node.packageUsage?.nextResetAt))").font(.caption).foregroundStyle(.secondary)
                            if node.packageUsage?.freshness != "fresh" { Text("用量未采集或已陈旧 · \(DateDisplayText.local(node.packageUsage?.sampledAt))").font(.caption).foregroundStyle(.orange) }
                        }.contentShape(Rectangle())
                    }.buttonStyle(.plain)
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }.accessibilityIdentifier("overview.quotaRank")
    }
}
