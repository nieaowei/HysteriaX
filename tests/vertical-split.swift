import AppKit
import SwiftUI

@Observable @MainActor final class Selection {
    var record = 0
    var hasDetail = false
}
struct Record: Identifiable { let id: Int; let name: String }
struct CheckView: View {
    var selection: Selection
    var body: some View {
        MainVerticalSplitView(hasDetail: selection.hasDetail) {
            Table([Record(id: 0, name: "First"), Record(id: 1, name: "Second")]) {
                TableColumn("Name", value: \.name)
            }
        } detail: {
            VStack(spacing: 0) {
                Text("Record \(selection.record)").padding(selection.record == 0 ? 16 : 30)
                Divider()
                ScrollView { Text(String(repeating: "Detail\n", count: selection.record == 0 ? 10 : 30)) }
            }
            .id(selection.record)
        }
    }
}
@main struct SplitSelectionChecks {
    @MainActor static func main() {
        _ = NSApplication.shared
        let selection = Selection()
        let host = NSHostingView(rootView: CheckView(selection: selection))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 800), styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        defer { window.contentView = nil; window.close() }
        window.orderFront(nil)
        func settle() {
            host.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        func splits(_ view: NSView) -> [NSSplitView] {
            (view as? NSSplitView).map { [$0] } ?? view.subviews.flatMap(splits)
        }
        func measure(_ label: String) -> [CGFloat] {
            let result = splits(host).first!.arrangedSubviews.map { $0.frame.height }
            print("\(label): \(result)"); fflush(stdout)
            return result
        }
        settle()
        selection.hasDetail = true
        settle()
        let first = measure("First")
        precondition(first.count == 2 && abs(first[0] - first[1]) < 2, "Expected initial 1:1 split")
        selection.record = 1
        settle()
        let second = measure("Second")
        precondition(abs(first[0] - second[0]) < 2, "Selection changed divider")
        splits(host).first!.setPosition(300, ofDividerAt: 0)
        settle()
        let dragged = measure("Dragged")
        precondition(abs(dragged[0] - 300) < 2, "Divider must be adjustable")
        selection.record = 0
        settle()
        let after = measure("After switching")
        precondition(abs(dragged[0] - after[0]) < 2, "Selection reset dragged divider")
    }
}
