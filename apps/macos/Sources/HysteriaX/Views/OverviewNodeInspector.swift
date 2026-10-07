import SwiftUI

struct OverviewNodeInspector: View {
    @Bindable var store: ManagementStore
    let selected: OverviewNode
    let close: () -> Void
    let navigate: (OverviewDestination) -> Void
    private var node: OverviewNode { store.overview?.nodes.first { $0.nodeID == selected.nodeID } ?? selected }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack { Text(node.name).font(.title2.bold()); Spacer(); Button(L10n.text("关闭"), action: close).keyboardShortcut(.cancelAction) }
            Text(L10n.text("公网入口与转发：{0}", String(describing: (OverviewDisplay.status(node.inletStatus)))))
            Text(L10n.text("外部目标：{0}", String(describing: (node.externalStatus.map(OverviewDisplay.status) ?? L10n.text("未配置")))))
            Text(L10n.text("代理就绪 {0} · 目标请求 {1}", String(describing: (OverviewDisplay.milliseconds(node.connectionMs))), String(describing: (OverviewDisplay.milliseconds(node.latencyMs)))))
            Text(L10n.text("最近探测：{0}", String(describing: (DateDisplayText.local(node.probeSampledAt))))).foregroundStyle(.secondary)
            if let reason = node.reason { Text(reason).foregroundStyle(.orange) }
            ScrollView { OverviewHistoryView(store: store, fixedNodeID: node.nodeID, category: .quality) }.frame(maxHeight: 380)
            HStack { Spacer(); Button(L10n.text("管理此节点")) { close(); navigate(OverviewDestination(section: "nodes", entityID: node.nodeID)) } }
        }.padding(24).frame(minWidth: 650, idealWidth: 760, minHeight: 650, maxHeight: 820)
    }
}
