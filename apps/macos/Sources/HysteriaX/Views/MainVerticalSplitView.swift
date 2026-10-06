import SwiftUI

/// The main list and detail panes start with equal heights and resize natively.
struct MainVerticalSplitView<Content: View, Detail: View>: View {
    var hasDetail: Bool
    @ViewBuilder var content: () -> Content
    @ViewBuilder var detail: () -> Detail

    var body: some View {
        if hasDetail {
            VSplitView {
                VStack(spacing: 0) {
                    content()
                }
                .frame(maxWidth: .infinity, minHeight: 180, maxHeight: .infinity)
                // Keep the split pane's identity stable when the selected detail's ID changes.
                VStack(spacing: 0) {
                    detail()
                }
                .frame(maxWidth: .infinity, minHeight: 180, maxHeight: .infinity)
            }
        } else {
            content()
        }
    }
}
