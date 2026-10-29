import SwiftUI

struct OverviewView: View {
    @Bindable var store: ManagementStore
    var navigate: (OverviewDestination) -> Void = { _ in }

    @SceneStorage("overview.tab") private var selectedTab = "summary"
    @State private var quotaScrollRequest: UUID?
    @State private var sceneState = OverviewSceneState()

    private var activeTab: String { ["summary", "traffic", "quality"].contains(selectedTab) ? selectedTab : "summary" }

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
                Picker(L10n.text("概览分类"), selection: $selectedTab) {
                    tabLabel(L10n.text("运行概况"), symbol: "square.grid.2x2", id: "summary")
                    tabLabel(L10n.text("流量与额度"), symbol: "chart.bar.xaxis", id: "traffic")
                    tabLabel(L10n.text("连接与质量"), symbol: "network", id: "quality")
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
                Text(store.lastUpdated == nil ? L10n.text("打开设置，填写管理服务地址和令牌。") : L10n.text("显示上次更新的缓存数据，恢复连接后刷新。"))
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
            OverviewMonitoringView(store: store, navigate: navigate, section: .metrics, state: sceneState.summary) {
                selectedTab = "traffic"
                quotaScrollRequest = UUID()
            }
            OverviewSummaryLayout {
                summaryTasksColumn
                serverMonitoringSection
            }
        case "traffic":
            OverviewHistoryView(store: store, category: .traffic, isActive: true, state: sceneState.traffic.history)
            OverviewMonitoringView(store: store, navigate: navigate, section: .quota).id("overview.quota")
            userSummarySection
        case "quality":
            OverviewMonitoringView(store: store, navigate: navigate, section: .quality, state: sceneState.quality)
            OverviewHistoryView(store: store, category: .quality, isActive: true, state: sceneState.quality.history)
        default:
            EmptyView()
        }
    }

    private var summaryTasksColumn: some View {
        VStack(alignment: .leading, spacing: 20) {
            OverviewMonitoringView(store: store, navigate: navigate, section: .issues, state: sceneState.summary)
            recentJobsSection
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private var recentJobsSection: some View {
        OverviewCard(L10n.text("最近任务"), systemImage: "list.bullet.rectangle") {
            if store.jobs.isEmpty {
                Text(L10n.text("暂无任务")).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 8)
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
        return OverviewCard(L10n.text("用户状态"), systemImage: "person.2") {
            VStack(alignment: .leading, spacing: 6) {
                if let summary = store.clientSummary {
                    Text(L10n.text("全部 {0} · 启用 {1} · 停用 {2}", String(summary.userTotal), String(summary.userEnabled), String(summary.userTotal - summary.userEnabled)))
                    Text(L10n.text("已过期 {0} · 7 天内到期 {1} · 额度耗尽 {2}", String(summary.userExpired), String(summary.userExpiring), String(summary.userDepleted))).foregroundStyle(.secondary)
                } else {
                    Text(L10n.text("尚未读取")).foregroundStyle(.secondary)
                }
                Text(L10n.text("到期与额度分类可能重叠；启用状态不代表用户在线。")).font(.caption).foregroundStyle(.secondary)
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var serverMonitoringSection: some View {
        OverviewServerCard(monitoring: store.serverMonitoring, serviceAddress: store.serviceAddress,
                           isConnected: store.isConnected, isLoading: store.isLoading, error: store.serverMonitoringError)
    }

}
