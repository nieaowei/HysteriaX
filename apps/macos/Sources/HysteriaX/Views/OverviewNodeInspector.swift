import SwiftUI

struct OverviewNodeInspector: View {
    @Bindable var store: ManagementStore
    let selected: OverviewNode
    let close: () -> Void
    let navigate: (OverviewDestination) -> Void
    private var node: OverviewNode { store.overview?.nodes.first { $0.nodeID == selected.nodeID } ?? selected }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack { Text(node.name).font(.title2.bold()); Spacer(); Button("关闭", action: close).keyboardShortcut(.cancelAction) }
            Text("公网入口与转发：\(OverviewDisplay.status(node.inletStatus))")
            Text("外部目标：\(node.externalStatus.map(OverviewDisplay.status) ?? "未配置")")
            Text("代理就绪 \(OverviewDisplay.milliseconds(node.connectionMs)) · 目标请求 \(OverviewDisplay.milliseconds(node.latencyMs))")
            Text("最近探测：\(DateDisplayText.local(node.probeSampledAt))").foregroundStyle(.secondary)
            if let reason = node.reason { Text(reason).foregroundStyle(.orange) }
            ScrollView { OverviewHistoryView(store: store, fixedNodeID: node.nodeID, category: .quality) }.frame(maxHeight: 380)
            HStack { Spacer(); Button("管理此节点") { close(); navigate(OverviewDestination(section: "nodes", entityID: node.nodeID)) } }
        }.padding(24).frame(minWidth: 650, idealWidth: 760, minHeight: 650, maxHeight: 820)
    }
}
