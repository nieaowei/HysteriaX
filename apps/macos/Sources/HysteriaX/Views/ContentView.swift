import SwiftUI

private enum MainSection: String, CaseIterable, Identifiable {
    case overview, nodes, dns, users, credentials, jobs, audit

    var id: String { rawValue }
    var title: String {
        switch self {
        case .overview: L10n.text("概览")
        case .dns: "DNS"
        case .nodes: L10n.text("节点")
        case .users: L10n.text("用户")
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
        case .credentials: "key.horizontal"
        case .jobs: "list.bullet.rectangle"
        case .audit: "clock.arrow.circlepath"
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
        case .credentials:
            L10n.text("集中管理凭据、引用、到期时间和更新结果。")
        case .jobs:
            L10n.text("部署、同步和撤权任务的阶段与结果。")
        case .audit:
            L10n.text("配置修改、凭据轮换、额度重置和撤权记录。")
        }
    }

    var body: some View {
        NavigationSplitView {
            List(selection: selection) {
                ForEach(MainSection.allCases) { section in
                    Label(section.title, systemImage: section.symbol)
                        .tag(section.rawValue)
                        .accessibilityLabel(section.title)
                        .accessibilityIdentifier("sidebar.\(section.rawValue)")
                }
            }
            .listStyle(.sidebar)
            .navigationSplitViewColumnWidth(min: 180, ideal: 210)
            .safeAreaInset(edge: .bottom) {
                HStack(spacing: 8) {
                    Circle().fill(store.isConnected ? Color.green : Color.secondary).frame(width: 8, height: 8)
                    Text(store.isConnected ? L10n.text("服务已连接") : L10n.text("服务未连接"))
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button { openSettings() } label: { Image(systemName: "gearshape") }
                        .buttonStyle(.plain).help(L10n.text("管理服务设置"))
                }
                .padding(.horizontal, 14).padding(.vertical, 10)
            }
        } detail: {
            detail
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(currentSection.pageAccessibilityIdentifier)
                .navigationTitle(currentSection.pageTitle)
                .navigationSubtitle(currentPageDescription)
                .toolbar {
                    ToolbarItemGroup {
                        Button { Task { await store.refresh(); store.requestOverviewHistoryRefresh() } } label: {
                            Label(L10n.text("刷新"), systemImage: "arrow.clockwise")
                        }
                        .disabled(store.isLoading)
                    }
                }
        }
        .task { await store.refresh() }
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
            overviewDestination = destination
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
        case .users: UsersView(store: store, initialSelection: overviewDestination?.section == "users" ? overviewDestination?.entityID : nil, onInitialSelectionHandled: { overviewDestination = nil })
        case .jobs: JobsView(store: store, initialSelection: overviewDestination?.section == "jobs" ? overviewDestination?.entityID : nil, onInitialSelectionHandled: { overviewDestination = nil })
        case .credentials: CredentialsView(store: store) { type, id in
            if type == "dns_connection" {
                selectedSection = "dns"
            } else {
                overviewDestination = OverviewDestination(section: type == "user" ? "users" : type == "job" ? "jobs" : "nodes", entityID: id)
                selectedSection = type == "user" ? "users" : type == "job" ? "jobs" : "nodes"
            }
        }
        case .audit: AuditView(store: store)
        }
    }
}
