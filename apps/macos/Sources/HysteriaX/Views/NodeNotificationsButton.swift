import SwiftUI

struct NodeNotificationsButton: View {
    @Bindable var store: ManagementStore
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

    private var notificationCount: Int {
        notificationNodes.reduce(0) { $0 + max($1.packageUsage?.alerts.count ?? 0, 1) }
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
        .accessibilityLabel("通知，\(notificationCount) 条当前提醒")
        .accessibilityIdentifier("overview.notifications")
        .help("节点提醒（\(notificationCount)）")
        .popover(isPresented: $isPresented, arrowEdge: .top) {
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    Text("节点提醒").font(.headline)
                    Text("\(notificationCount)").foregroundStyle(.secondary)
                    Spacer()
                    Button { isPresented = false } label: {
                        Label("关闭通知", systemImage: "xmark")
                            .labelStyle(.iconOnly)
                    }
                    .buttonStyle(.plain)
                    .keyboardShortcut(.cancelAction)
                    .help("关闭通知")
                }
                .padding(16)
                Divider()
                if !store.isConnected {
                    Label("显示上次同步的提醒，恢复连接后更新。", systemImage: "clock.arrow.circlepath")
                        .font(.caption).foregroundStyle(.secondary)
                        .padding(.horizontal, 16).padding(.top, 12)
                }
                if notificationNodes.isEmpty {
                    VStack(spacing: 10) {
                        Image(systemName: "bell.slash").font(.title).foregroundStyle(.secondary)
                        Text("暂无提醒").font(.headline)
                        Text("节点到期和流量预警将在这里显示。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 32)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 16) {
                            ForEach(notificationNodes) { node in
                                VStack(alignment: .leading, spacing: 8) {
                                    Label(node.name, systemImage: "server.rack").fontWeight(.semibold)
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
