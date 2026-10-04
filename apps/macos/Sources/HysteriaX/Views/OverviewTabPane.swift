import SwiftUI

struct OverviewTabPane<Content: View>: View {
    let state: OverviewTabState
    let tabID: String
    let scrollRequest: UUID?
    @ViewBuilder var content: () -> Content
    @State private var position: ScrollPosition
    @State private var restoring: Bool
    @State private var isMounted = false
    private let savedOffset: CGPoint

    init(state: OverviewTabState, tabID: String, scrollRequest: UUID?, @ViewBuilder content: @escaping () -> Content) {
        self.state = state
        self.tabID = tabID
        self.scrollRequest = scrollRequest
        self.content = content
        savedOffset = state.scrollOffset
        var initial = ScrollPosition()
        initial.scrollTo(point: state.scrollOffset)
        _position = State(initialValue: initial)
        _restoring = State(initialValue: state.scrollOffset.y > 0)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) { content() }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 28).padding(.top, 8).padding(.bottom, 24)
        }
        .accessibilityIdentifier("overview.scroll.\(tabID)")
        .scrollPosition($position)
        .onAppear { isMounted = true }
        .onDisappear { isMounted = false }
        .onScrollGeometryChange(for: OverviewScrollSnapshot.self) { geometry in
            OverviewScrollSnapshot(offset: CGPoint(x: 0,y: max(0,geometry.contentOffset.y+geometry.contentInsets.top)),
                                   maximumOffset: max(0,geometry.contentSize.height-geometry.containerSize.height+geometry.contentInsets.top+geometry.contentInsets.bottom))
        } action: { _, snapshot in
            guard isMounted else { return }
            if restoring {
                let target = min(savedOffset.y,snapshot.maximumOffset)
                guard abs(snapshot.offset.y-target) < 1 else { return }
                restoring = false
            }
            state.scrollOffset = snapshot.offset
        }
        .onScrollPhaseChange { _, phase in
            if phase == .interacting { restoring = false }
        }
        .task { handleRequest() }
        .onChange(of: scrollRequest) { _, _ in handleRequest() }
    }

    private func handleRequest() {
        guard let scrollRequest, scrollRequest != state.handledQuotaRequest else { return }
        restoring = false
        position.scrollTo(id: "overview.quota", anchor: .top)
        state.handledQuotaRequest = scrollRequest
    }
}

private struct OverviewScrollSnapshot: Equatable {
    let offset: CGPoint
    let maximumOffset: CGFloat
}
