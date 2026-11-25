import AppKit
import SwiftUI

/// The main list and detail panes start with equal heights and resize natively.
struct MainVerticalSplitView<Content: View, Detail: View>: View {
    var hasDetail: Bool
    @ViewBuilder var content: () -> Content
    @ViewBuilder var detail: () -> Detail

    var body: some View {
        VSplitView {
            VStack(spacing: 0) {
                content()
            }
            .frame(maxWidth: .infinity, minHeight: 180, maxHeight: .infinity)
            if hasDetail {
                // Keep the split pane's identity stable when the selected detail's ID changes.
                VStack(spacing: 0) {
                    detail()
                }
                .frame(maxWidth: .infinity, minHeight: 180, maxHeight: .infinity)
                .background(SplitDividerInitializer())
            }
        }
    }
}

private struct SplitDividerInitializer: NSViewRepresentable {
    func makeNSView(context: Context) -> ProbeView { ProbeView() }
    func updateNSView(_ nsView: ProbeView, context: Context) {}

    final class ProbeView: NSView {
        private var didInitialize = false
        private var pendingReset = false

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            scheduleInitialSplit()
        }

        override func layout() {
            super.layout()
            if !didInitialize {
                scheduleInitialSplit()
            }
        }

        private func scheduleInitialSplit() {
            guard !didInitialize, !pendingReset, window != nil else { return }
            pendingReset = true
            DispatchQueue.main.async { [weak self] in
                guard let self, !self.didInitialize, self.window != nil else { return }
                self.pendingReset = false
                var current: NSView? = self.superview
                while let view = current {
                    if let splitView = view as? NSSplitView {
                        if splitView.arrangedSubviews.count == 2 && splitView.bounds.height > 0 {
                            self.didInitialize = true
                            splitView.setPosition(floor((splitView.bounds.height - splitView.dividerThickness) / 2), ofDividerAt: 0)
                        }
                        return
                    }
                    current = view.superview
                }
            }
        }
    }
}
