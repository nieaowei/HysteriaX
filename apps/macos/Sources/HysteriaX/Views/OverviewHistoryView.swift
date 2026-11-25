import SwiftUI
import Charts

enum OverviewHistoryCategory { case traffic, quality }

struct OverviewHistoryView: View {
    @Bindable var store: ManagementStore
    var fixedNodeID: String? = nil
    var category: OverviewHistoryCategory
    var isActive: Bool
    @State private var state: OverviewHistoryState
    @State private var lastRefreshToken: UUID?
    @State private var hoveredTrafficBucketID: String?
    @State private var hoveredOnlineBucketID: String?
    @State private var hoveredLatencyBucketID: String?
    @State private var hoveredProbeBucketID: String?

    init(store: ManagementStore, fixedNodeID: String? = nil, category: OverviewHistoryCategory = .traffic,
         isActive: Bool = true, range: String = "24h", state: OverviewHistoryState? = nil) {
        self.store = store
        self.fixedNodeID = fixedNodeID
        self.category = category
        self.isActive = isActive
        if let state {
            _state = State(initialValue: state)
        } else {
            let cached = store.cachedOverviewHistory(range: range, nodeID: fixedNodeID, source: "users", metrics: category == .traffic ? "traffic" : "quality")
            _state = State(initialValue: OverviewHistoryState(range: range, history: cached))
        }
    }

    private var metrics: String { category == .traffic ? "traffic" : "quality" }
    private var range: String { state.range }
    private var source: String { state.source }
    private var nodeID: String { state.nodeID }
    private var history: OverviewHistoryDisplay? { state.display }
    private var error: String? { state.error }
    private var loading: Bool { state.loading }
    private var selectedNodeID: String? { fixedNodeID ?? (nodeID.isEmpty ? nil : nodeID) }
    private var requestKey: String { "\(store.serviceAddress)|\(store.isConnected)|\(range)|\(source)|\(selectedNodeID ?? "all")|\(store.overviewHistoryRefreshToken)|\(isActive)" }

    var body: some View {
        directoryContent
            .task(id: store.dataRefreshToken) { await store.prepareDirectories([.nodes]) }
            .disabled([ManagementStore.Directory.nodes].contains { store.loadingDirectories.contains($0) })
    }

    private var directoryContent: some View {
        @Bindable var query = state
        return OverviewCard(category == .traffic ? L10n.text("流量趋势") : L10n.text("连接与质量趋势"), systemImage: category == .traffic ? "chart.bar.xaxis" : "chart.xyaxis.line", isRefreshing: loading) {
            VStack(alignment: .leading, spacing: 14) {
                OverviewFilterLayout {
                    Picker(L10n.text("时间"), selection: $query.range) { Text(L10n.text("24 小时")).tag("24h"); Text(L10n.text("7 天")).tag("7d"); Text(L10n.text("30 天")).tag("30d") }
                        .pickerStyle(.segmented).layoutValue(key: OverviewControlWidth.self, value: 240)
                        .accessibilityIdentifier("overview.history.range").accessibilityValue(range == "24h" ? L10n.text("24 小时") : range == "7d" ? L10n.text("7 天") : L10n.text("30 天"))
                    if category == .traffic {
                        Picker(L10n.text("流量来源"), selection: $query.source) { Text(L10n.text("用户代理流量")).tag("users"); Text(L10n.text("节点网卡流量")).tag("network") }
                            .layoutValue(key: OverviewControlWidth.self, value: 210)
                            .accessibilityIdentifier("overview.history.source").accessibilityValue(source == "network" ? L10n.text("节点网卡流量") : L10n.text("用户代理流量"))
                    }
                    if fixedNodeID == nil {
                        Picker(L10n.text("节点"), selection: $query.nodeID) { Text(L10n.text("全部节点")).tag(""); ForEach(store.nodes) { Text($0.name).tag($0.id) } }
                            .layoutValue(key: OverviewControlWidth.self, value: 220)
                            .accessibilityIdentifier("overview.history.node").accessibilityValue(store.nodes.first { $0.id == nodeID }?.name ?? L10n.text("全部节点"))
                    }
                }
                if let error { Text(error).font(.caption).foregroundStyle(.orange) }
                if let history {
                    if !store.isConnected { Text(L10n.text("以下为缓存趋势，恢复连接后更新。")).foregroundStyle(.orange).font(.caption) }
                    VStack(alignment: .leading, spacing: 18) {
                            if category == .traffic {
                            chartTitle(L10n.text("流量"), detail: L10n.text("节点视角 · {0} · 发送 / 接收", String(describing: (history.source == "users" ? L10n.text("用户代理") : L10n.text("网卡")))))
                            Chart {
                                ForEach(history.buckets) { bucket in
                                    if let tx = bucket.txBytes { RectangleMark(xStart: .value(L10n.text("开始"), bucket.date), xEnd: .value(L10n.text("结束"), bucket.endDate), yStart: .value(L10n.text("字节"), 0.0), yEnd: .value(L10n.text("字节"), Double(tx))).offset(xStart: 1, xEnd: -1).foregroundStyle(by: .value(L10n.text("方向"), L10n.text("发送"))) }
                                    if let rx = bucket.rxBytes { RectangleMark(xStart: .value(L10n.text("开始"), bucket.date), xEnd: .value(L10n.text("结束"), bucket.endDate), yStart: .value(L10n.text("字节"), Double(bucket.txBytes ?? 0)), yEnd: .value(L10n.text("字节"), Double(bucket.txBytes ?? 0) + Double(rx))).offset(xStart: 1, xEnd: -1).foregroundStyle(by: .value(L10n.text("方向"), L10n.text("接收"))) }
                                }
                                if let hovered = hoveredBucket(in: history, id: hoveredTrafficBucketID) {
                                    RectangleMark(xStart: .value(L10n.text("开始"), hovered.date), xEnd: .value(L10n.text("结束"), hovered.endDate))
                                        .foregroundStyle(Color.primary.opacity(0.08))
                                    RuleMark(x: .value(L10n.text("时间"), hovered.midpoint))
                                        .foregroundStyle(Color.secondary.opacity(0.45))
                                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                                }
                            }.chartYAxis { AxisMarks { value in AxisGridLine(); AxisValueLabel { if let bytes = value.as(Int64.self) { Text(OverviewDisplay.bytes(bytes)) } } } }.chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.chartOverlay { proxy in
                                hoverOverlayContent(proxy: proxy, history: history, hoveredID: $hoveredTrafficBucketID, matchMode: .interval) { bucket in
                                    trafficTooltip(bucket: bucket, history: history)
                                }
                            }.frame(height: 160)
                            Text(L10n.text("{0} 个时间段缺失或不完整；缺失不计作零，增量归入采样结束时段。", String(describing: (history.incompleteCount)))).font(.caption).foregroundStyle(.secondary)
                            if history.source == "users", history.maxTrafficExpected > 0 {
                                Text(L10n.text("流量采集最少覆盖 {0} / {1} 节点。", String(describing: (history.minTrafficCovered)), String(describing: (history.maxTrafficExpected)))).font(.caption).foregroundStyle(.secondary)
                            } else if history.source == "network" {
                                Text(L10n.text("网卡样本覆盖节点数：各时段 {0}–{1}；未采集节点不计入总量。", String(describing: (history.minTrafficCovered)), String(describing: (history.maxTrafficCovered)))).font(.caption).foregroundStyle(.secondary)
                            }
                            }
                            if category == .quality {
                            chartTitle(L10n.text("在线用户与连接"), detail: L10n.text("用户跨节点去重 · 桶内平均值及峰值"))
                            Chart {
                                ForEach(history.buckets) { bucket in
                                    if let users = bucket.onlineUsersAvg { PointMark(x: .value(L10n.text("时间"), bucket.date), y: .value(L10n.text("平均"), users)).foregroundStyle(by: .value(L10n.text("指标"), L10n.text("在线用户平均"))) }
                                    if let peak = bucket.onlineUsersPeak { PointMark(x: .value(L10n.text("时间"), bucket.date), y: .value(L10n.text("峰值"), peak)).symbol(.diamond).foregroundStyle(by: .value(L10n.text("指标"), L10n.text("在线用户峰值"))) }
                                    if let connections = bucket.connectionsAvg { PointMark(x: .value(L10n.text("时间"), bucket.date), y: .value(L10n.text("平均"), connections)).foregroundStyle(by: .value(L10n.text("指标"), L10n.text("连接平均"))) }
                                    if let peak = bucket.connectionsPeak { PointMark(x: .value(L10n.text("时间"), bucket.date), y: .value(L10n.text("峰值"), peak)).symbol(.diamond).foregroundStyle(by: .value(L10n.text("指标"), L10n.text("连接峰值"))) }
                                }
                                if let hovered = hoveredBucket(in: history, id: hoveredOnlineBucketID) {
                                    RuleMark(x: .value(L10n.text("时间"), hovered.date))
                                        .foregroundStyle(Color.secondary.opacity(0.45))
                                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                                }
                            }.chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.chartOverlay { proxy in
                                hoverOverlayContent(proxy: proxy, history: history, hoveredID: $hoveredOnlineBucketID, matchMode: .nearestStart) { bucket in
                                    onlineTooltip(bucket: bucket)
                                }
                            }.frame(height: 150)
                            Text(L10n.text("{0} 个时段在线数据不完整；各时段最少覆盖节点：{1}。部分采集结果仅代表已覆盖节点。", String(describing: (history.onlineIncompleteCount)), String(describing: (history.minOnlineCovered)))).font(.caption).foregroundStyle(.secondary)
                            chartTitle(L10n.text("代理请求延迟"), detail: L10n.text("管理服务器经公网节点访问目标 · 成功样本 P50 / P95"))
                            Chart {
                                ForEach(history.buckets) { bucket in
                                    if let value = bucket.latencyP50Ms { PointMark(x: .value(L10n.text("时间"), bucket.date), y: .value(L10n.text("毫秒"), value)).foregroundStyle(by: .value(L10n.text("指标"), "P50")) }
                                    if let value = bucket.latencyP95Ms { PointMark(x: .value(L10n.text("时间"), bucket.date), y: .value(L10n.text("毫秒"), value)).foregroundStyle(by: .value(L10n.text("指标"), "P95")) }
                                }
                                if let hovered = hoveredBucket(in: history, id: hoveredLatencyBucketID) {
                                    RuleMark(x: .value(L10n.text("时间"), hovered.date))
                                        .foregroundStyle(Color.secondary.opacity(0.45))
                                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                                }
                            }.chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.chartOverlay { proxy in
                                hoverOverlayContent(proxy: proxy, history: history, hoveredID: $hoveredLatencyBucketID, matchMode: .nearestStart) { bucket in
                                    latencyTooltip(bucket: bucket)
                                }
                            }.frame(height: 140)
                            chartTitle(L10n.text("代理探测成功率"), detail: L10n.text("成功 / 有效探测；未配置与无数据不计入分母"))
                            Chart {
                                ForEach(history.buckets) { bucket in
                                    if bucket.probeAttempts > 0 { RectangleMark(xStart: .value(L10n.text("开始"), bucket.date), xEnd: .value(L10n.text("结束"), bucket.endDate), yStart: .value(L10n.text("百分比"), 0.0), yEnd: .value(L10n.text("百分比"), Double(bucket.probeSuccesses) / Double(bucket.probeAttempts) * 100)).offset(xStart: 1, xEnd: -1) }
                                }
                                if let hovered = hoveredBucket(in: history, id: hoveredProbeBucketID) {
                                    RectangleMark(xStart: .value(L10n.text("开始"), hovered.date), xEnd: .value(L10n.text("结束"), hovered.endDate))
                                        .foregroundStyle(Color.primary.opacity(0.08))
                                    RuleMark(x: .value(L10n.text("时间"), hovered.midpoint))
                                        .foregroundStyle(Color.secondary.opacity(0.45))
                                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                                }
                            }.chartYScale(domain: 0...100).chartXScale(domain: history.domain).chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) { AxisGridLine(); AxisTick(); AxisValueLabel(anchor: .center) } }.chartOverlay { proxy in
                                hoverOverlayContent(proxy: proxy, history: history, hoveredID: $hoveredProbeBucketID, matchMode: .interval) { bucket in
                                    probeTooltip(bucket: bucket)
                                }
                            }.frame(height: 120)
                            }
                            Text(L10n.text("更新：{0} · 时区 {1}", String(describing: (history.updatedAtText)), String(describing: (history.timezone)))).font(.caption).foregroundStyle(.secondary)
                    }
                } else { Text(loading ? L10n.text("正在加载历史数据…") : L10n.text("暂无历史样本")).foregroundStyle(.secondary).padding(.vertical, 12) }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier(category == .traffic ? "overview.history.traffic" : "overview.history.quality")
        .onChange(of: requestKey) { _, _ in
            hoveredTrafficBucketID = nil
            hoveredOnlineBucketID = nil
            hoveredLatencyBucketID = nil
            hoveredProbeBucketID = nil
        }
        .task(id: requestKey) {
            state.loading = false
            var bypassCache = lastRefreshToken != nil && lastRefreshToken != store.overviewHistoryRefreshToken
            lastRefreshToken = store.overviewHistoryRefreshToken
            let query = "\(store.serviceAddress)|\(range)|\(source)|\(selectedNodeID ?? "all")"
            if query != state.loadedQuery { state.replaceHistory(store.cachedOverviewHistory(range: range, nodeID: selectedNodeID, source: source, metrics: metrics)); state.error = nil; state.loadedQuery = query }
            guard isActive else { return }
            while !Task.isCancelled {
                if store.isConnected {
                    state.loading = true
                    do {
                        let value = try await store.overviewHistory(range: range, nodeID: selectedNodeID, source: source, metrics: metrics, bypassCache: bypassCache)
                        guard !Task.isCancelled else { return }
                        state.replaceHistory(value); state.error = nil
                    } catch {
                        guard !Task.isCancelled else { return }
                        state.error = L10n.text("趋势暂不可用：{0}", String(describing: (error.localizedDescription)))
                    }
                    state.loading = false
                    bypassCache = false
                }
                do { try await Task.sleep(for: .seconds(60)) } catch { return }
            }
        }
    }
    private func chartTitle(_ title: String, detail: String) -> some View { VStack(alignment: .leading, spacing: 3) { Text(title).font(.headline); Text(detail).font(.caption).foregroundStyle(.secondary) } }

    private func hoveredBucket(in history: OverviewHistoryDisplay, id: String?) -> OverviewChartBucket? {
        guard let id else { return nil }
        return history.buckets.first { $0.id == id }
    }

    @ViewBuilder
    private func hoverOverlayContent<Tooltip: View>(
        proxy: ChartProxy,
        history: OverviewHistoryDisplay,
        hoveredID: Binding<String?>,
        matchMode: OverviewHistoryDisplay.BucketMatchMode,
        @ViewBuilder tooltip: @escaping (OverviewChartBucket) -> Tooltip
    ) -> some View {
        GeometryReader { geometry in
            if let plotAnchor = proxy.plotFrame {
                let plotFrame = geometry[plotAnchor]
                ZStack(alignment: .topLeading) {
                    Color.clear
                        .contentShape(Rectangle())
                        .onContinuousHover { phase in
                            switch phase {
                            case .active(let location):
                                guard plotFrame.contains(location) else {
                                    hoveredID.wrappedValue = nil
                                    return
                                }
                                let plotX = location.x - plotFrame.origin.x
                                if let date = proxy.value(atX: plotX, as: Date.self),
                                   let bucket = history.bucket(at: date, mode: matchMode) {
                                    hoveredID.wrappedValue = bucket.id
                                } else {
                                    hoveredID.wrappedValue = nil
                                }
                            case .ended:
                                hoveredID.wrappedValue = nil
                            }
                        }
                    if let hovered = hoveredBucket(in: history, id: hoveredID.wrappedValue),
                       let relativeX = proxy.position(forX: matchMode == .interval ? hovered.midpoint : hovered.date) {
                        let anchorX = plotFrame.origin.x + relativeX
                        let placeOnRight = anchorX <= plotFrame.midX
                        let card = tooltipCard(bucket: hovered, history: history) {
                            tooltip(hovered)
                        }
                        .fixedSize()
                        .allowsHitTesting(false)

                        if placeOnRight {
                            card
                                .padding(.leading, anchorX + 12)
                                .padding(.top, plotFrame.minY + 6)
                        } else {
                            card
                                .frame(width: max(0, anchorX - 12), alignment: .trailing)
                                .padding(.top, plotFrame.minY + 6)
                        }
                    }
                }
            }
        }
    }

    private func tooltipCard<Content: View>(
        bucket: OverviewChartBucket,
        history: OverviewHistoryDisplay,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(bucket.timeRangeText(range: history.range, timeZoneIdentifier: history.timezone))
                .fontWeight(.semibold)
            content()
        }
        .font(.caption)
        .monospacedDigit()
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.primary.opacity(0.12), lineWidth: 1))
        .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
    }

    private func tooltipRow(_ label: String, value: String) -> some View {
        HStack(spacing: 10) {
            Text(label).foregroundStyle(.secondary)
            Spacer(minLength: 8)
            Text(value).fontWeight(.medium)
        }
    }

    @ViewBuilder
    private func trafficTooltip(bucket: OverviewChartBucket, history: OverviewHistoryDisplay) -> some View {
        tooltipRow(L10n.text("发送"), value: bucket.txBytes.map(OverviewDisplay.bytes) ?? "—")
        tooltipRow(L10n.text("接收"), value: bucket.rxBytes.map(OverviewDisplay.bytes) ?? "—")
        if bucket.txBytes != nil || bucket.rxBytes != nil {
            tooltipRow(L10n.text("合计"), value: OverviewDisplay.bytes((bucket.txBytes ?? 0) + (bucket.rxBytes ?? 0)))
        }
        if history.source == "users", bucket.trafficExpectedNodes > 0 {
            tooltipRow(L10n.text("覆盖节点"), value: L10n.text("覆盖 {0} / {1}", String(bucket.trafficCoveredNodes), String(bucket.trafficExpectedNodes)))
        } else if history.source == "network" {
            tooltipRow(L10n.text("覆盖节点"), value: String(bucket.trafficCoveredNodes))
        }
        if bucket.incomplete {
            Text(L10n.text("数据不完整")).foregroundStyle(.orange)
        }
    }

    @ViewBuilder
    private func onlineTooltip(bucket: OverviewChartBucket) -> some View {
        tooltipRow(L10n.text("在线用户平均"), value: OverviewDisplay.metricNumber(bucket.onlineUsersAvg))
        tooltipRow(L10n.text("在线用户峰值"), value: OverviewDisplay.metricNumber(bucket.onlineUsersPeak))
        tooltipRow(L10n.text("连接平均"), value: OverviewDisplay.metricNumber(bucket.connectionsAvg))
        tooltipRow(L10n.text("连接峰值"), value: OverviewDisplay.metricNumber(bucket.connectionsPeak))
        tooltipRow(L10n.text("覆盖节点"), value: String(bucket.coveredNodes))
        if bucket.onlineIncomplete {
            Text(L10n.text("数据不完整")).foregroundStyle(.orange)
        }
    }

    @ViewBuilder
    private func latencyTooltip(bucket: OverviewChartBucket) -> some View {
        if bucket.latencyP50Ms == nil && bucket.latencyP95Ms == nil {
            Text(L10n.text("无有效样本")).foregroundStyle(.secondary)
        } else {
            tooltipRow("P50", value: OverviewDisplay.milliseconds(bucket.latencyP50Ms))
            tooltipRow("P95", value: OverviewDisplay.milliseconds(bucket.latencyP95Ms))
        }
    }

    @ViewBuilder
    private func probeTooltip(bucket: OverviewChartBucket) -> some View {
        if bucket.probeAttempts > 0 {
            tooltipRow(L10n.text("成功率"), value: OverviewDisplay.percentage(bucket.probeSuccessRate))
            tooltipRow(L10n.text("成功探测"), value: "\(bucket.probeSuccesses) / \(bucket.probeAttempts)")
        } else {
            Text(L10n.text("无有效探测")).foregroundStyle(.secondary)
        }
    }
}
