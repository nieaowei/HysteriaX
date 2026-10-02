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
                serverMonitoringSection
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

    private var serverMonitoringSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 16) {
                HStack {
                    Label("管理服务器", systemImage: "server.rack").font(.headline)
                    Spacer()
                    Label(store.isConnected ? "已连接" : "未连接", systemImage: "circle.fill")
                        .foregroundStyle(store.isConnected ? .green : .secondary)
                }
                if let monitor = store.serverMonitoring {
                    if !store.isConnected || store.serverMonitoringError != nil {
                        Label("以下为上次采集的数据", systemImage: "clock.arrow.circlepath")
                            .font(.callout).foregroundStyle(.orange)
                    }
                    HStack(alignment: .top, spacing: 24) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(monitor.hostname ?? "主机名未提供").fontWeight(.medium)
                            Text(monitor.os ?? "系统信息未提供").foregroundStyle(.secondary)
                            Text(store.serviceAddress).font(.caption).foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                        Spacer()
                        VStack(alignment: .trailing, spacing: 6) {
                            Text("服务版本 \(monitor.serviceVersion)")
                            Text("服务运行 \(uptime(monitor.serviceUptimeSeconds))")
                            Text("主机运行 \(uptime(monitor.hostUptimeSeconds))").foregroundStyle(.secondary)
                        }
                    }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), spacing: 16)], alignment: .leading, spacing: 16) {
                        resourceMetric("CPU", symbol: "cpu", fraction: monitor.cpuUsagePercent / 100,
                                       value: String(format: "%.1f%%", monitor.cpuUsagePercent),
                                       detail: "\(monitor.cpuCount) 个逻辑核心")
                        resourceMetric("内存", symbol: "memorychip",
                                       fraction: fraction(used: monitor.memoryUsedBytes, total: monitor.memoryTotalBytes),
                                       value: usage(monitor.memoryUsedBytes, monitor.memoryTotalBytes), detail: "已用 / 总量")
                        resourceMetric("根目录磁盘", symbol: "internaldrive",
                                       fraction: fraction(used: monitor.rootDiskUsedBytes, total: monitor.rootDiskTotalBytes),
                                       value: usage(monitor.rootDiskUsedBytes, monitor.rootDiskTotalBytes), detail: "已用 / 总量")
                    }
                    HStack {
                        Label(monitor.database == "ok" ? "数据库正常" : "数据库不可用",
                              systemImage: monitor.database == "ok" ? "checkmark.circle" : "exclamationmark.triangle")
                            .foregroundStyle(monitor.database == "ok" ? Color.secondary : Color.orange)
                        Spacer()
                        Text("采集于 \(sampleTime(monitor.sampledAt)) · 每 15 秒刷新")
                            .foregroundStyle(.secondary)
                    }.font(.caption)
                    Text("资源指标为管理服务所在环境可见的主机数据。")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    Text(store.isLoading ? "正在获取服务器监控信息…" : "暂无服务器监控信息")
                        .foregroundStyle(.secondary).padding(.vertical, 8)
                }
                if let error = store.serverMonitoringError {
                    Text(error).font(.caption).foregroundStyle(.orange)
                }
            }.frame(maxWidth: .infinity, alignment: .leading).padding(8)
        }
    }

    private func resourceMetric(_ title: String, symbol: String, fraction: Double?, value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: symbol).foregroundStyle(.secondary)
            Text(value).font(.title3).fontWeight(.semibold).monospacedDigit()
            if let fraction {
                ProgressView(value: min(max(fraction, 0), 1))
                    .tint(fraction >= 0.9 ? .orange : .accentColor)
            }
            Text(detail).font(.caption).foregroundStyle(.secondary)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func fraction(used: Int?, total: Int?) -> Double? {
        guard let used, let total, total > 0 else { return nil }
        return Double(used) / Double(total)
    }

    private func usage(_ used: Int?, _ total: Int?) -> String {
        guard let used, let total, total > 0 else { return "未提供" }
        return "\(ByteCountFormatter.string(fromByteCount: Int64(used), countStyle: .binary)) / \(ByteCountFormatter.string(fromByteCount: Int64(total), countStyle: .binary))"
    }

    private func uptime(_ seconds: Int) -> String {
        let days = seconds / 86_400
        let hours = seconds % 86_400 / 3_600
        let minutes = seconds % 3_600 / 60
        return days > 0 ? "\(days) 天 \(hours) 小时" : "\(hours) 小时 \(minutes) 分钟"
    }

    private func sampleTime(_ timestamp: String) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = formatter.date(from: timestamp) ?? ISO8601DateFormatter().date(from: timestamp)
        return date?.formatted(date: .abbreviated, time: .standard) ?? timestamp
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
