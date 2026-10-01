import SwiftUI

struct OverviewView: View {
    @Bindable var store: ManagementStore

    private var nodesNeedingAttention: Int {
        let failedStates: Set<String> = ["fingerprint_changed", "sync_failed", "rollback_failed", "drift", "unreachable", "delete_failed"]
        return store.nodes.filter { node in
            failedStates.contains(node.state)
                || node.dataFreshness == "stale"
                || (node.state == "deployed" && node.dataFreshness == "not_collected")
                || (node.openGaps ?? 0) > 0
                || (node.pendingRevocations ?? 0) > 0
        }.count
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                if !store.isConnected {
                    ContentUnavailableView(
                        store.lastUpdated == nil ? "连接管理服务" : "管理服务已断开",
                        systemImage: "network.slash",
                        description: Text(store.lastUpdated.map {
                            "以下为 \($0.formatted(date: .abbreviated, time: .shortened)) 更新的缓存数据。恢复连接后刷新；断开期间不能写入。"
                        } ?? "打开设置，填写服务 HTTPS 地址和管理员令牌。")
                    )
                    .frame(maxWidth: .infinity, minHeight: 180)
                }
                Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 16) {
                    GridRow {
                        summaryCard("节点", value: "\(store.nodes.count)", symbol: "server.rack")
                        summaryCard("用户", value: "\(store.users.count)", symbol: "person.2")
                        summaryCard("待处理任务", value: "\(store.jobs.filter { $0.status == "queued" || $0.status == "running" }.count)", symbol: "hourglass")
                        summaryCard("需关注节点", value: "\(nodesNeedingAttention)", symbol: "exclamationmark.triangle")
                    }
                }
                GroupBox("最近任务") {
                    if store.jobs.isEmpty {
                        Text("暂无任务").foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 8)
                    } else {
                        VStack(spacing: 0) {
                            ForEach(store.jobs.prefix(5)) { job in
                                HStack {
                                    Image(systemName: job.status == "failed" ? "exclamationmark.circle" : "checkmark.circle")
                                        .foregroundStyle(job.status == "failed" ? .orange : .secondary)
                                    Text(JobDisplayText.kind(job.kind)).fontWeight(.medium)
                                    Spacer()
                                    Text(JobDisplayText.stage(job.stage)).foregroundStyle(.secondary)
                                    Text(JobDisplayText.status(job.status)).foregroundStyle(.secondary).frame(width: 92, alignment: .trailing)
                                }.padding(.vertical, 8)
                                if job.id != store.jobs.prefix(5).last?.id { Divider() }
                            }
                        }
                    }
                }
            }
            .padding(28)
        }
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
            }.frame(minWidth: 150, minHeight: 60)
        }
    }
}
