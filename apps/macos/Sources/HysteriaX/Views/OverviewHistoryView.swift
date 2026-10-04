import SwiftUI
import Charts

enum OverviewHistoryCategory { case traffic, quality }

struct OverviewHistoryView: View {
    @Bindable var store: ManagementStore
    var fixedNodeID: String? = nil
    var category: OverviewHistoryCategory
    var isActive: Bool
    @State private var state: OverviewHistoryState

    init(store: ManagementStore, fixedNodeID: String? = nil, category: OverviewHistoryCategory = .traffic,
         isActive: Bool = true, range: String = "24h", state: OverviewHistoryState? = nil) {
        self.store = store
        self.fixedNodeID = fixedNodeID
        self.category = category
        self.isActive = isActive
        if let state {
            _state = State(initialValue: state)
        } else {
            let cached = store.cachedOverviewHistory(range: range, nodeID: fixedNodeID, source: "users")
            _state = State(initialValue: OverviewHistoryState(range: range, history: cached))
        }
    }

    private var range: String { state.range }
    private var source: String { state.source }
    private var nodeID: String { state.nodeID }
    private var history: OverviewHistoryDisplay? { state.display }
    private var error: String? { state.error }
    private var loading: Bool { state.loading }
    private var selectedNodeID: String? { fixedNodeID ?? (nodeID.isEmpty ? nil : nodeID) }
    private var requestKey: String { "\(store.serviceAddress)|\(store.isConnected)|\(store.supportsOverviewMonitoring)|\(range)|\(source)|\(selectedNodeID ?? "all")|\(store.overviewHistoryRefreshToken)|\(isActive)" }

    var body: some View {
        @Bindable var query = state
        return OverviewCard(category == .traffic ? "流量趋势" : "连接与质量趋势", systemImage: category == .traffic ? "chart.bar.xaxis" : "chart.xyaxis.line") {
            VStack(alignment: .leading, spacing: 14) {
                OverviewFilterLayout {
                    Picker("时间", selection: $query.range) { Text("24 小时").tag("24h"); Text("7 天").tag("7d"); Text("30 天").tag("30d") }
                        .pickerStyle(.segmented).layoutValue(key: OverviewControlWidth.self, value: 240)
                        .accessibilityIdentifier("overview.history.range").accessibilityValue(range == "24h" ? "24 小时" : range == "7d" ? "7 天" : "30 天")
                    if category == .traffic {
                        Picker("流量来源", selection: $query.source) { Text("用户代理流量").tag("users"); Text("节点网卡流量").tag("network") }
                            .layoutValue(key: OverviewControlWidth.self, value: 210)
                            .accessibilityIdentifier("overview.history.source").accessibilityValue(source == "network" ? "节点网卡流量" : "用户代理流量")
                    }
                    if fixedNodeID == nil {
                        Picker("节点", selection: $query.nodeID) { Text("全部节点").tag(""); ForEach(store.nodes) { Text($0.name).tag($0.id) } }
                            .layoutValue(key: OverviewControlWidth.self, value: 220)
                            .accessibilityIdentifier("overview.history.node").accessibilityValue(store.nodes.first { $0.id == nodeID }?.name ?? "全部节点")
                    }
                    if loading { ProgressView().controlSize(.small).layoutValue(key: OverviewControlWidth.self, value: 20) }
                }
                if let error { Text(error).font(.caption).foregroundStyle(.orange) }
                if let history {
                    if !store.isConnected { Text("以下为缓存趋势，恢复连接后更新。").foregroundStyle(.orange).font(.caption) }
                    VStack(alignment: .leading, spacing: 18) {
                            if category == .traffic {
                            chartTitle("流量", detail: "节点视角 · \(history.source == "users" ? "用户代理" : "网卡") · 发送 / 接收")
                            Chart(history.buckets) { bucket in
                                if let tx = bucket.txBytes { RectangleMark(xStart: .value("开始", bucket.date), xEnd: .value("结束", bucket.endDate), yStart: .value("字节", 0.0), yEnd: .value("字节", Double(tx))).offset(xStart: 1, xEnd: -1).foregroundStyle(by: .value("方向", "发送")) }
                                if let rx = bucket.rxBytes { RectangleMark(xStart: .value("开始", bucket.date), xEnd: .value("结束", bucket.endDate), yStart: .value("字节", Double(bucket.txBytes ?? 0)), yEnd: .value("字节", Double(bucket.txBytes ?? 0) + Double(rx))).offset(xStart: 1, xEnd: -1).foregroundStyle(by: .value("方向", "接收")) }
                            }.chartYAxis { AxisMarks { value in AxisGridLine(); AxisValueLabel { if let bytes = value.as(Int64.self) { Text(OverviewDisplay.bytes(bytes)) } } } }.chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.frame(height: 160)
                            Text("\(history.incompleteCount) 个时间段缺失或不完整；缺失不计作零，增量归入采样结束时段。").font(.caption).foregroundStyle(.secondary)
                            if history.source == "users", history.maxTrafficExpected > 0 {
                                Text("流量采集最少覆盖 \(history.minTrafficCovered) / \(history.maxTrafficExpected) 节点。").font(.caption).foregroundStyle(.secondary)
                            } else if history.source == "network" {
                                Text("网卡样本覆盖节点数：各时段 \(history.minTrafficCovered)–\(history.maxTrafficCovered)；未采集节点不计入总量。").font(.caption).foregroundStyle(.secondary)
                            }
                            }
                            if category == .quality {
                            chartTitle("在线用户与连接", detail: "用户跨节点去重 · 桶内平均值及峰值")
                            Chart(history.buckets) { bucket in
                                if let users = bucket.onlineUsersAvg { PointMark(x: .value("时间", bucket.date), y: .value("平均", users)).foregroundStyle(by: .value("指标", "在线用户平均")) }
                                if let peak = bucket.onlineUsersPeak { PointMark(x: .value("时间", bucket.date), y: .value("峰值", peak)).symbol(.diamond).foregroundStyle(by: .value("指标", "在线用户峰值")) }
                                if let connections = bucket.connectionsAvg { PointMark(x: .value("时间", bucket.date), y: .value("平均", connections)).foregroundStyle(by: .value("指标", "连接平均")) }
                                if let peak = bucket.connectionsPeak { PointMark(x: .value("时间", bucket.date), y: .value("峰值", peak)).symbol(.diamond).foregroundStyle(by: .value("指标", "连接峰值")) }
                            }.chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.frame(height: 150)
                            Text("\(history.onlineIncompleteCount) 个时段在线数据不完整；各时段最少覆盖节点：\(history.minOnlineCovered)。部分采集结果仅代表已覆盖节点。").font(.caption).foregroundStyle(.secondary)
                            chartTitle("代理请求延迟", detail: "管理服务器经公网节点访问目标 · 成功样本 P50 / P95")
                            Chart(history.buckets) { bucket in
                                if let value = bucket.latencyP50Ms { PointMark(x: .value("时间", bucket.date), y: .value("毫秒", value)).foregroundStyle(by: .value("指标", "P50")) }
                                if let value = bucket.latencyP95Ms { PointMark(x: .value("时间", bucket.date), y: .value("毫秒", value)).foregroundStyle(by: .value("指标", "P95")) }
                            }.chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.frame(height: 140)
                            chartTitle("代理探测成功率", detail: "成功 / 有效探测；未配置与无数据不计入分母")
                            Chart(history.buckets) { bucket in
                                if bucket.probeAttempts > 0 { RectangleMark(xStart: .value("开始", bucket.date), xEnd: .value("结束", bucket.endDate), yStart: .value("百分比", 0.0), yEnd: .value("百分比", Double(bucket.probeSuccesses) / Double(bucket.probeAttempts) * 100)).offset(xStart: 1, xEnd: -1) }
                            }.chartYScale(domain: 0...100).chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.frame(height: 120)
                            }
                            Text("更新：\(history.updatedAtText) · 时区 \(history.timezone)").font(.caption).foregroundStyle(.secondary)
                    }
                } else { Text(loading ? "正在加载历史数据…" : "暂无历史样本").foregroundStyle(.secondary).padding(.vertical, 12) }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier(category == .traffic ? "overview.history.traffic" : "overview.history.quality")
        .task(id: requestKey) {
            state.loading = false
            let query = "\(store.serviceAddress)|\(range)|\(source)|\(selectedNodeID ?? "all")"
            if query != state.loadedQuery { state.replaceHistory(store.cachedOverviewHistory(range: range, nodeID: selectedNodeID, source: source)); state.error = nil; state.loadedQuery = query }
            guard store.supportsOverviewMonitoring, isActive else { return }
            while !Task.isCancelled {
                if store.isConnected {
                    state.loading = true
                    do {
                        let value = try await store.overviewHistory(range: range, nodeID: selectedNodeID, source: source)
                        guard !Task.isCancelled else { return }
                        state.replaceHistory(value); state.error = nil
                    } catch {
                        guard !Task.isCancelled else { return }
                        state.error = "趋势暂不可用：\(error.localizedDescription)"
                    }
                    state.loading = false
                }
                do { try await Task.sleep(for: .seconds(60)) } catch { return }
            }
        }
    }
    private func chartTitle(_ title: String, detail: String) -> some View { VStack(alignment: .leading, spacing: 3) { Text(title).font(.headline); Text(detail).font(.caption).foregroundStyle(.secondary) } }
}
