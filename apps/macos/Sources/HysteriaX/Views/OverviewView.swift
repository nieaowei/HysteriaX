import SwiftUI

struct OverviewView: View {
    @Bindable var store: ManagementStore
    var navigate: (OverviewDestination) -> Void = { _ in }

    private var nodesNeedingAttention: Int {
        let failedStates: Set<String> = ["fingerprint_changed", "sync_failed", "rollback_failed", "drift", "unreachable", "delete_failed"]
        return store.nodes.filter { node in
            !(node.packageUsage?.alerts.isEmpty ?? true)
                || node.packageUsage?.restricted == true
                || (node.package?.quotaBytes != nil && node.packageUsage?.freshness != "fresh")
                || failedStates.contains(node.state)
                || node.dataFreshness == "stale"
                || (node.state == "deployed" && node.dataFreshness == "not_collected")
                || (node.openGaps ?? 0) > 0
                || (node.pendingRevocations ?? 0) > 0
        }.count
    }

    @SceneStorage("overview.tab") private var selectedTab = "summary"
    @State private var quotaScrollRequest: UUID?
    @State private var sceneState = OverviewSceneState()

    private var activeTab: String { ["summary", "traffic", "quality"].contains(selectedTab) ? selectedTab : "summary" }

    private var hasMonitoring: Bool { store.supportsOverviewMonitoring || store.overview != nil }

    var body: some View {
        @Bindable var quality = sceneState.quality
        return VStack(alignment: .leading, spacing: 0) {
            OverviewTabPane(state: sceneState.tab(activeTab), tabID: activeTab, scrollRequest: activeTab == "traffic" ? quotaScrollRequest : nil) {
                VStack(alignment: .leading, spacing: 6) {
                    statusBar
                    tabContent(activeTab)
                }
            }
            .id(activeTab)
        }
        .onAppear {
            if !["summary", "traffic", "quality"].contains(selectedTab) { selectedTab = "summary" }
        }
        .onChange(of: store.serviceAddress) { _, _ in quotaScrollRequest = nil; sceneState.changedService() }
        .sheet(item: $quality.inspectedNode) { node in
            OverviewNodeInspector(store: store, selected: node, close: { quality.inspectedNode = nil }, navigate: navigate)
        }
        .toolbar {
            ToolbarItem(placement: .principal) {
                Picker("概览分类", selection: $selectedTab) {
                    tabLabel("运行概况", symbol: "square.grid.2x2", id: "summary")
                    tabLabel("流量与额度", symbol: "chart.bar.xaxis", id: "traffic")
                    tabLabel("连接与质量", symbol: "network", id: "quality")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .accessibilityIdentifier("overview.tabPicker")
            }
            ToolbarItem(placement: .primaryAction) {
                NodeNotificationsButton(store: store, navigate: navigate)
            }
        }
    }

    private var statusBar: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !store.isConnected {
                Text(store.lastUpdated == nil ? "打开设置，填写管理服务地址和令牌。" : "显示上次更新的缓存数据，恢复连接后刷新。")
                    .foregroundStyle(.orange)
            }
            if let error = store.overviewError { Text(error).foregroundStyle(.orange) }
        }.font(.caption2)
    }

    private func tabLabel(_ title: String, symbol: String, id: String) -> some View {
        Label(title, systemImage: symbol)
            .labelStyle(.iconOnly)
            .help(title)
            .accessibilityLabel(title)
            .accessibilityIdentifier("overview.tab.\(id)")
            .tag(id)
    }

    @ViewBuilder private func tabContent(_ tab: String) -> some View {
        switch tab {
        case "summary":
            if hasMonitoring {
                OverviewMonitoringView(store: store, navigate: navigate, section: .metrics, state: sceneState.summary) {
                    selectedTab = "traffic"
                    quotaScrollRequest = UUID()
                }
            } else {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 180))], spacing: 16) {
                    summaryCard("节点", value: "\(store.nodes.count)", symbol: "server.rack")
                    summaryCard("用户", value: "\(store.users.count)", symbol: "person.2")
                    summaryCard("最近任务中待处理", value: "\(store.jobs.filter { $0.status == "queued" || $0.status == "running" }.count)", symbol: "hourglass")
                    summaryCard("需关注节点", value: "\(nodesNeedingAttention)", symbol: "exclamationmark.triangle")
                }
                monitoringUnavailable
            }
            OverviewSummaryLayout {
                summaryTasksColumn
                serverMonitoringSection
            }
        case "traffic":
            if hasMonitoring { OverviewHistoryView(store: store, category: .traffic, isActive: true, state: sceneState.traffic.history) }
            else { monitoringUnavailable }
            OverviewMonitoringView(store: store, navigate: navigate, section: .quota).id("overview.quota")
            userSummarySection
        case "quality":
            if hasMonitoring {
                OverviewMonitoringView(store: store, navigate: navigate, section: .quality, state: sceneState.quality)
                OverviewHistoryView(store: store, category: .quality, isActive: true, state: sceneState.quality.history)
            } else { monitoringUnavailable }
        default:
            EmptyView()
        }
    }

    private var summaryTasksColumn: some View {
        VStack(alignment: .leading, spacing: 20) {
            if hasMonitoring { OverviewMonitoringView(store: store, navigate: navigate, section: .issues, state: sceneState.summary) }
            recentJobsSection
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private var monitoringUnavailable: some View {
        Text("当前服务不支持高级概览监控，升级后可查看持续探测、在线统计与历史趋势。")
            .font(.callout).foregroundStyle(.secondary)
    }

    private var recentJobsSection: some View {
        OverviewCard("最近任务", systemImage: "list.bullet.rectangle") {
            if store.jobs.isEmpty {
                Text("暂无任务").foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 8)
            } else {
                VStack(spacing: 0) {
                    ForEach(store.jobs.prefix(5)) { job in
                        HStack {
                            Image(systemName: job.status == "failed" ? "exclamationmark.circle" : "checkmark.circle")
                                .foregroundStyle(job.status == "failed" ? .orange : .secondary)
                            Button(JobDisplayText.kind(job.kind)) { navigate(OverviewDestination(section: "jobs", entityID: job.id)) }.buttonStyle(.plain).fontWeight(.medium).accessibilityIdentifier("overview.job.\(job.id)")
                            Spacer()
                            Text(JobDisplayText.stage(job.stage)).foregroundStyle(.secondary).lineLimit(1)
                            Text(JobDisplayText.status(job.status)).foregroundStyle(.secondary).frame(width: 60, alignment: .trailing)
                        }.padding(.vertical, 8)
                        if job.id != store.jobs.prefix(5).last?.id { Divider() }
                    }
                }
            }
        }
    }

    private var userSummarySection: some View {
        let now = Date()
        let enabled = store.users.filter(\.enabled).count
        let expired = store.users.filter { DateDisplayText.parse($0.expiresAt).map { $0 <= now } ?? false }.count
        let expiring = store.users.filter { DateDisplayText.parse($0.expiresAt).map { $0 > now && $0 <= now.addingTimeInterval(7 * 86400) } ?? false }.count
        let depleted = store.users.filter { user in user.quotaBytes.map { user.usageBytes >= $0 } ?? false }.count
        return OverviewCard("用户状态", systemImage: "person.2") {
            VStack(alignment: .leading, spacing: 6) {
                Text("全部 \(store.users.count) · 启用 \(enabled) · 停用 \(store.users.count - enabled)")
                Text("已过期 \(expired) · 7 天内到期 \(expiring) · 额度耗尽 \(depleted)").foregroundStyle(.secondary)
                Text("到期与额度分类可能重叠；启用状态不代表用户在线。").font(.caption).foregroundStyle(.secondary)
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var serverMonitoringSection: some View {
        OverviewServerCard(monitoring: store.serverMonitoring, serviceAddress: store.serviceAddress,
                           isConnected: store.isConnected, isLoading: store.isLoading, error: store.serverMonitoringError)
    }

    private func summaryCard(_ title: String, value: String, symbol: String) -> some View {
        GroupBox {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(title).foregroundStyle(.secondary)
                    Text(value).font(.system(size: 30, weight: .semibold, design: .rounded))
                }
                Spacer()
                Image(systemName: symbol).font(.title2).foregroundStyle(.tint)
            }.frame(maxWidth: .infinity, minHeight: 60, alignment: .leading)
        }
    }
}
