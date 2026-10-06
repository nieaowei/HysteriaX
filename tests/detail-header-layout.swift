import AppKit
import SwiftUI

private struct Marker: NSViewRepresentable {
    let name: String
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        view.identifier = NSUserInterfaceItemIdentifier(name)
        return view
    }
    func updateNSView(_ view: NSView, context: Context) {}
}

@main struct DetailHeaderLayoutChecks {
    @MainActor static func main() {
        _ = NSApplication.shared
        let host = NSHostingView(rootView: DetailHeaderLayout {
            Marker(name: "title").frame(width: 180, height: 40)
            Marker(name: "actions").frame(width: 180, height: 24)
        }.frame(maxHeight: .infinity, alignment: .top))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 160), styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        defer { window.contentView = nil; window.close() }
        func find(_ name: String, in view: NSView) -> NSView? {
            if view.identifier?.rawValue == name { return view }
            return view.subviews.lazy.compactMap { find(name, in: $0) }.first
        }
        var firstTitle: NSView?
        var firstActions: NSView?
        for width in [600, 300, 600] {
            host.frame.size = NSSize(width: width, height: 160)
            host.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.15))
            let title = find("title", in: host)!
            let actions = find("actions", in: host)!
            let titleRect = title.convert(title.bounds, to: host)
            let actionRect = actions.convert(actions.bounds, to: host)
            precondition(!titleRect.intersects(actionRect), "Title and actions overlap")
            precondition(abs(titleRect.minX) < 1, "Title must stay left aligned")
            if width == 600 {
                precondition(abs(actionRect.maxX - CGFloat(width)) < 1, "Actions must align right in wide layout")
            } else {
                precondition(abs(actionRect.minX) < 1, "Actions must align left in stacked layout")
            }
            if let firstTitle, let firstActions {
                precondition(title === firstTitle && actions === firstActions, "Reflow must retain the same content tree")
            } else {
                firstTitle = title
                firstActions = actions
            }
        }
        print("Wide and stacked header alignment passed.")
    }
}
