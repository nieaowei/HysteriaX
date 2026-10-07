import SwiftUI

struct NodeNotificationsButton: View {
    @Bindable var store: ManagementStore
    var navigate: (OverviewDestination) -> Void = { _ in }
    @State private var isPresented = false

    private var notificationNodes: [NodeSummary] {
        store.nodes.filter {
            !($0.packageUsage?.alerts.isEmpty ?? true) || $0.packageUsage?.restricted == true
        }.sorted {
            if $0.packageUsage?.restricted != $1.packageUsage?.restricted {
                return $0.packageUsage?.restricted == true
            }
            return $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }

    private var monitoringIssues: [OverviewIssue] { store.overview?.issues.filter { $0.kind != "package" } ?? [] }

    private var notificationCount: Int {
        notificationNodes.reduce(0) { $0 + max($1.packageUsage?.alerts.count ?? 0, 1) } + monitoringIssues.count
    }

    var body: some View {
        Button { isPresented.toggle() } label: {
            Image(systemName: notificationCount > 0 ? "bell.fill" : "bell")
                .overlay(alignment: .topTrailing) {
                    if notificationCount > 0 {
                        Text(notificationCount > 99 ? "99+" : String(notificationCount))
                            .font(.system(size: 9, weight: .semibold))
                            .monospacedDigit()
                            .padding(.horizontal, 4)
                            .padding(.vertical, 2)
                            .foregroundStyle(.white)
                            .background(.red, in: Capsule())
                            .offset(x: 9, y: -7)
                            .accessibilityHidden(true)
                    }
                }
                .frame(width: 36, height: 34)
        }
        .accessibilityLabel(L10n.text("通知，{0} 条当前提醒", String(describing: (notificationCount))))
        .accessibilityIdentifier("overview.notifications")
        .help(L10n.text("节点提醒（{0}）", String(describing: (notificationCount))))
        .popover(isPresented: $isPresented, arrowEdge: .top) {
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    Text(L10n.text("节点提醒")).font(.headline)
                    Text("\(notificationCount)").foregroundStyle(.secondary)
                    Spacer()
                    Button { isPresented = false } label: {
                        Label(L10n.text("关闭通知"), systemImage: "xmark")
                            .labelStyle(.iconOnly)
                    }
                    .buttonStyle(.plain)
                    .keyboardShortcut(.cancelAction)
                    .help(L10n.text("关闭通知"))
                }
                .padding(16)
                Divider()
                if !store.isConnected {
                    Label(L10n.text("显示上次同步的提醒，恢复连接后更新。"), systemImage: "clock.arrow.circlepath")
                        .font(.caption).foregroundStyle(.secondary)
                        .padding(.horizontal, 16).padding(.top, 12)
                }
                if !monitoringIssues.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(L10n.text("监控事项 {0}", String(describing: (monitoringIssues.count)))).font(.subheadline.bold())
                        ForEach(monitoringIssues.prefix(6)) { issue in
                            Button {
                                isPresented = false
                                navigate(OverviewDestination(section: issue.entityType == "job" ? "jobs" : issue.entityType == "user" ? "users" : "nodes", entityID: issue.entityID))
                            } label: { Text("\(issue.name)：\(OverviewDisplay.reason(issue))").lineLimit(2) }.buttonStyle(.plain)
                        }
                    }.padding(16)
                    Divider()
                }
                if notificationNodes.isEmpty && monitoringIssues.isEmpty {
                    VStack(spacing: 10) {
                        Image(systemName: "bell.slash").font(.title).foregroundStyle(.secondary)
                        Text(L10n.text("暂无提醒")).font(.headline)
                        Text(L10n.text("节点到期和流量预警将在这里显示。"))
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 32)
                } else if !notificationNodes.isEmpty {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 16) {
                            ForEach(notificationNodes) { node in
                                VStack(alignment: .leading, spacing: 8) {
                                    Button {
                                        isPresented = false
                                        navigate(OverviewDestination(section: "nodes", entityID: node.id))
                                    } label: { Label(node.name, systemImage: "server.rack").fontWeight(.semibold) }.buttonStyle(.plain)
                                    NodePackageStatus(package: node.package, usage: node.packageUsage)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                if node.id != notificationNodes.last?.id { Divider() }
                            }
                        }
                        .padding(16)
                    }
                    .frame(maxHeight: 380)
                }
            }
            .frame(width: 420)
        }
    }
}
