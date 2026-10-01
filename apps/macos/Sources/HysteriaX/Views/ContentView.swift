import SwiftUI

private enum MainSection: String, CaseIterable, Identifiable {
    case overview, nodes, users, jobs, audit

    var id: String { rawValue }
    var title: String {
        switch self {
        case .overview: "概览"
        case .nodes: "节点"
        case .users: "用户"
        case .jobs: "任务"
        case .audit: "审计"
        }
    }
    var symbol: String {
        switch self {
        case .overview: "square.grid.2x2"
        case .nodes: "server.rack"
        case .users: "person.2"
        case .jobs: "list.bullet.rectangle"
        case .audit: "clock.arrow.circlepath"
        }
    }
}

struct ContentView: View {
    @Bindable var store: ManagementStore
    @SceneStorage("selectedSection") private var selectedSection = MainSection.overview.rawValue
    @Environment(\.openSettings) private var openSettings

    private var selection: Binding<String> {
        Binding(get: { selectedSection }, set: { selectedSection = $0 })
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
                    Text(store.isConnected ? "服务已连接" : "服务未连接")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button { openSettings() } label: { Image(systemName: "gearshape") }
                        .buttonStyle(.plain).help("管理服务设置")
                }
                .padding(.horizontal, 14).padding(.vertical, 10)
            }
        } detail: {
            detail
                .toolbar {
                    ToolbarItemGroup {
                        Button { Task { await store.refresh() } } label: {
                            Label("刷新", systemImage: "arrow.clockwise")
                        }
                        .disabled(store.isLoading)
                        SettingsLink { Label("设置", systemImage: "gearshape") }
                    }
                }
        }
        .task { await store.refresh() }
        .alert("无法连接管理服务", isPresented: Binding(
            get: { store.errorMessage != nil },
            set: { if !$0 { store.errorMessage = nil } }
        )) {
            Button("好", role: .cancel) { store.errorMessage = nil }
        } message: {
            Text(store.errorMessage ?? "")
        }
    }

    @ViewBuilder
    private var detail: some View {
        switch MainSection(rawValue: selectedSection) ?? .overview {
        case .overview: OverviewView(store: store)
        case .nodes: NodesView(store: store)
        case .users: UsersView(store: store)
        case .jobs: JobsView(store: store)
        case .audit: AuditView(store: store)
        }
    }
}
