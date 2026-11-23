import SwiftUI

private enum MainSection: String, CaseIterable, Identifiable {
    case overview, nodes, dns, credentials, users, templates, jobs, audit

    static let monitoringSections: [MainSection] = [.overview]
    static let infrastructureSections: [MainSection] = [.nodes, .dns, .credentials]
    static let operationsSections: [MainSection] = [.users, .templates, .jobs, .audit]

    var id: String { rawValue }
    var title: String {
        switch self {
        case .overview: L10n.text("概览")
        case .dns: "DNS"
        case .nodes: L10n.text("节点")
        case .users: L10n.text("用户")
        case .templates: L10n.text("订阅模板")
        case .credentials: L10n.text("凭据")
        case .jobs: L10n.text("任务")
        case .audit: L10n.text("审计")
        }
    }
    var pageTitle: String {
        switch self {
        case .overview: L10n.text("HysteriaX 管理中心")
        case .dns: L10n.text("DNS 记录")
        case .nodes: L10n.text("节点")
        case .users: L10n.text("用户")
        case .templates: L10n.text("订阅模板")
        case .credentials: L10n.text("凭据")
        case .jobs: L10n.text("任务")
        case .audit: L10n.text("审计")
        }
    }
    var pageAccessibilityIdentifier: String {
        switch self {
        case .overview: "overview.page"
        case .dns: "dns.page"
        case .nodes: "nodes.title"
        case .users: "users.title"
        case .templates: "templates.page"
        case .credentials: "credentials.page"
        case .jobs: "jobs.title"
        case .audit: "audit.title"
        }
    }
    var symbol: String {
        switch self {
        case .overview: "square.grid.2x2"
        case .dns: "network"
        case .nodes: "server.rack"
        case .users: "person.2"
        case .templates: "doc.text"
        case .credentials: "key.horizontal"
        case .jobs: "list.bullet.rectangle"
        case .audit: "clock.arrow.circlepath"
        }
    }
    var iconColor: Color {
        switch self {
        case .overview: .blue
        case .dns: .cyan
        case .nodes: .indigo
        case .users: .green
        case .templates: .mint
        case .credentials: .orange
        case .jobs: .purple
        case .audit: .teal
        }
    }
}

struct ContentView: View {
    @Bindable var store: ManagementStore
    @SceneStorage("selectedSection") private var selectedSection = MainSection.overview.rawValue
    @State private var overviewDestination: OverviewDestination?
    @Environment(\.openSettings) private var openSettings

    private var selection: Binding<String> {
        Binding(get: { selectedSection }, set: { selectedSection = $0 })
    }
    private var currentSection: MainSection {
        MainSection(rawValue: selectedSection) ?? .overview
    }
    private var currentPageDescription: String {
        switch currentSection {
        case .overview:
            store.lastUpdated.map { L10n.text("数据更新于 {0}", String(describing: (L10n.date($0, date: .abbreviated, time: .shortened)))) } ?? L10n.text("连接管理服务以读取最新状态")
        case .dns:
            L10n.text("管理域名解析记录和节点域名分配。")
        case .nodes:
            L10n.text("管理 SSH 连接、Hysteria 配置和部署状态。")
        case .users:
            L10n.text("管理启停、到期、流量额度和节点分配。")
        case .templates:
            L10n.text("管理订阅配置、节点注入、草稿与发布版本。")
        case .credentials:
            L10n.text("集中管理凭据、引用、到期时间和更新结果。")
        case .jobs:
            L10n.text("部署、同步和撤权任务的阶段与结果。")
        case .audit:
            L10n.text("配置修改、凭据轮换、额度重置和撤权记录。")
        }
    }

    private var serviceHost: String? {
        guard !store.serviceAddress.isEmpty else { return nil }
        return URL(string: store.serviceAddress)?.host ?? store.serviceAddress
    }

    private var connectionStatusText: String {
        if store.isConnected { return L10n.text("服务已连接") }
        if store.isLoading && store.hasConfiguredService { return L10n.text("正在连接…") }
        return L10n.text("服务未连接")
    }

    private func sidebarBadge(for section: MainSection) -> (count: Int, tone: StatusTone)? {
        switch section {
        case .overview:
            let count = store.overview?.issues.count ?? 0
            return count > 0 ? (count, .warning) : nil
        case .nodes:
            let count = Int(store.overview?.attentionNodes ?? 0)
            return count > 0 ? (count, .warning) : nil
        case .credentials:
            let count = store.clientSummary?.notificationCredentials.count ?? 0
            return count > 0 ? (count, .warning) : nil
        case .jobs:
            guard let overview = store.overview else { return nil }
            let active = Int(overview.queuedJobs + overview.runningJobs)
            if active > 0 { return (active, .active) }
            let failed = Int(overview.failedJobs24h)
            return failed > 0 ? (failed, .warning) : nil
        case .dns, .users, .templates, .audit:
            return nil
        }
    }

    @ViewBuilder
    private func sidebarRow(_ section: MainSection) -> some View {
        HStack(spacing: 8) {
            Label {
                Text(section.title)
            } icon: {
                Image(systemName: section.symbol)
                    .foregroundStyle(section.iconColor)
            }
            Spacer(minLength: 4)
            if let badge = sidebarBadge(for: section) {
                SidebarCountBadge(count: badge.count, tone: badge.tone)
            }
        }
        .contentShape(Rectangle())
        .tag(section.rawValue)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(section.title)
        .accessibilityIdentifier("sidebar.\(section.rawValue)")
    }

    var body: some View {
        NavigationSplitView {
            List(selection: selection) {
                Section(L10n.text("监控")) {
                    ForEach(MainSection.monitoringSections) { sidebarRow($0) }
                }
                Section(L10n.text("基础设施")) {
                    ForEach(MainSection.infrastructureSections) { sidebarRow($0) }
                }
                Section(L10n.text("访问与运维")) {
                    ForEach(MainSection.operationsSections) { sidebarRow($0) }
                }
            }
            .listStyle(.sidebar)
            .navigationSplitViewColumnWidth(min: 185, ideal: 215)
            .safeAreaInset(edge: .bottom) {
                VStack(spacing: 0) {
                    Divider()
                    HStack(spacing: 8) {
                        Group {
                            if store.isLoading && !store.isConnected && store.hasConfiguredService {
                                ProgressView()
                                    .controlSize(.mini)
                            } else {
                                Circle()
                                    .fill(store.isConnected ? Color.green : (store.hasConfiguredService ? Color.orange : Color.secondary))
                                    .frame(width: 8, height: 8)
                            }
                        }
                        .frame(width: 12, height: 12)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(connectionStatusText)
                                .font(.caption.weight(.medium))
                                .foregroundStyle(store.isConnected ? .primary : .secondary)
                                .lineLimit(1)
                            if let host = serviceHost {
                                Text(host)
                                    .font(.caption2)
                                    .foregroundStyle(.tertiary)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                        Spacer(minLength: 4)
                        if !store.isConnected && store.hasConfiguredService && !store.isLoading {
                            Button {
                                Task { await store.refresh(); store.requestOverviewHistoryRefresh() }
                            } label: {
                                Image(systemName: "arrow.clockwise")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            .buttonStyle(.plain)
                            .help(L10n.text("重新连接"))
                            .accessibilityLabel(L10n.text("重新连接"))
                            .accessibilityIdentifier("sidebar.reconnect")
                        }
                        Button { openSettings() } label: {
                            Image(systemName: "gearshape")
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .help(L10n.text("管理服务设置"))
                        .accessibilityLabel(L10n.text("管理服务设置"))
                        .accessibilityIdentifier("sidebar.settings")
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .help(store.serviceAddress.isEmpty ? L10n.text("打开设置以连接管理服务") : store.serviceAddress)
                }
                .background(.bar)
            }
        } detail: {
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .toastOverlay(store.toast)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(currentSection.pageAccessibilityIdentifier)
                .navigationTitle(currentSection.pageTitle)
                .navigationSubtitle(currentPageDescription)
                .toolbar {
                    ToolbarItemGroup {
                        Button { Task { await store.refresh(); store.requestOverviewHistoryRefresh() } } label: {
                            Label(L10n.text("刷新"), systemImage: "arrow.clockwise")
                        }
                        .keyboardShortcut("r", modifiers: .command)
                        .help(L10n.text("刷新"))
                        .disabled(store.isLoading)
                    }
                }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task { await store.refresh() }
        .onChange(of: store.requestedJobID) { _, id in
            if let id {
                overviewDestination = OverviewDestination(section: "jobs", entityID: id)
                selectedSection = "jobs"
                store.requestedJobID = nil
            }
        }
        .onChange(of: store.requestedSection) { _, section in
            if let section { selectedSection = section; store.requestedSection = nil }
        }
        .alert(L10n.text("无法连接管理服务"), isPresented: Binding(
            get: { store.errorMessage != nil },
            set: { if !$0 { store.errorMessage = nil } }
        )) {
            Button(L10n.text("好"), role: .cancel) { store.errorMessage = nil }
        } message: {
            Text(store.errorMessage ?? "")
        }
    }

    @ViewBuilder
    private var detail: some View {
        switch currentSection {
        case .overview: OverviewView(store: store) { destination in
            overviewDestination = destination.entityID.isEmpty ? nil : destination
            selectedSection = destination.section
        }
        case .nodes: NodesView(store: store, initialSelection: overviewDestination?.section == "nodes" ? overviewDestination?.entityID : nil, onInitialSelectionHandled: { overviewDestination = nil }, onOpenJob: { jobID in
            overviewDestination = OverviewDestination(section: "jobs", entityID: jobID)
            selectedSection = "jobs"
        })
        case .dns: DNSRecordsView(store: store) { nodeID in
            overviewDestination = OverviewDestination(section: "nodes", entityID: nodeID)
            selectedSection = "nodes"
        }
        case .users: UsersView(store: store, initialSelection: overviewDestination?.section == "users" ? overviewDestination?.entityID : nil, onInitialSelectionHandled: { overviewDestination = nil }, onOpenNode: { nodeID in
            overviewDestination = OverviewDestination(section: "nodes", entityID: nodeID)
            selectedSection = "nodes"
        })
        case .jobs: JobsView(store: store, initialSelection: overviewDestination?.section == "jobs" ? overviewDestination?.entityID : nil, onInitialSelectionHandled: { overviewDestination = nil })
        case .credentials: CredentialsView(store: store) { type, id in
            if type == "dns_connection" {
                selectedSection = "dns"
            } else {
                overviewDestination = OverviewDestination(section: type == "user" ? "users" : type == "job" ? "jobs" : "nodes", entityID: id)
                selectedSection = type == "user" ? "users" : type == "job" ? "jobs" : "nodes"
            }
        }
        case .templates: SubscriptionTemplatesView(store: store)
        case .audit: AuditView(store: store)
        }
    }
}
