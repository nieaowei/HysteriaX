import SwiftUI

struct AuthorizationUserImpactList: View {
    let pairs: [AuthorizationPair]
    let users: [UserSummary]
    let nodes: [NodeSummary]
    @State private var searchText = ""
    @State private var page = 0
    private let pageSize = 50

    var body: some View {
        let userNames = Dictionary(users.map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first })
        let nodeNames = Dictionary(nodes.map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first })
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let impacts = AuthorizationUserImpact.grouped(pairs).filter {
            query.isEmpty || (userNames[$0.id] ?? $0.id).localizedStandardContains(query) || $0.id.localizedCaseInsensitiveContains(query)
        }
        let pageCount = max(1, (impacts.count + pageSize - 1) / pageSize)
        let currentPage = min(page, pageCount - 1)
        VStack(alignment: .leading, spacing: 8) {
            TextField(L10n.text("搜索成员名称或 ID"), text: $searchText).textFieldStyle(.roundedBorder)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(impacts.dropFirst(currentPage * pageSize).prefix(pageSize))) { impact in
                        DisclosureGroup(L10n.text("{0} · {1} 个节点", userNames[impact.id] ?? impact.id, String(impact.nodeIDs.count))) {
                            LazyVStack(alignment: .leading, spacing: 5) {
                                ForEach(impact.nodeIDs, id: \.self) { id in Text(nodeNames[id] ?? id) }
                            }
                            .font(.caption).padding(.leading, 12)
                        }
                    }
                    if impacts.isEmpty { Text(L10n.text("没有匹配的成员")).foregroundStyle(.secondary) }
                }
            }
            .frame(height: 180)
            HStack {
                Text(L10n.text("第 {0} / {1} 页 · 每页最多 {2} 人", String(currentPage + 1), String(pageCount), String(pageSize)))
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button(L10n.text("上一页")) { page = currentPage - 1 }.disabled(currentPage == 0)
                Button(L10n.text("下一页")) { page = currentPage + 1 }.disabled(currentPage + 1 >= pageCount)
            }
        }
        .onChange(of: searchText) { _, _ in page = 0 }
    }
}
