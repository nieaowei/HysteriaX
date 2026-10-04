import AppKit
import SwiftUI

@main struct OverviewResizeChecks {
    @MainActor static func measure(_ name: String, root: AnyView) throws -> [String:Any] {
        let host = NSHostingView(rootView: root)
        let window = NSWindow(contentRect: NSRect(x: 0,y: 0,width: 1000,height: 850),styleMask:[.borderless],backing:.buffered,defer:false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        host.frame = NSRect(x:0,y:0,width:1000,height:850)
        host.layoutSubtreeIfNeeded()
        RunLoop.current.run(until: Date().addingTimeInterval(0.15))
        for width in [1000,900,800,700,900,1000] {
            host.frame.size = NSSize(width: width,height:850)
            host.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.001))
        }
        print("BEGIN \(name)"); fflush(stdout)
        var durations: [Double] = []
        var layouts: [Double] = []
        for cycle in 0..<3 {
            for index in 0..<32 {
                let width = CGFloat(650 + (index < 16 ? index : 31-index) * 28 + cycle % 2)
                let before = DispatchTime.now().uptimeNanoseconds
                host.frame.size = NSSize(width:width,height:850)
                host.layoutSubtreeIfNeeded()
                RunLoop.current.run(until: Date().addingTimeInterval(0.001))
                let laidOut = DispatchTime.now().uptimeNanoseconds
                if let bitmap = host.bitmapImageRepForCachingDisplay(in:host.bounds) { host.cacheDisplay(in:host.bounds,to:bitmap) }
                let rendered = DispatchTime.now().uptimeNanoseconds
                layouts.append(Double(laidOut-before)/1_000_000)
                durations.append(Double(rendered-before)/1_000_000)
            }
        }
        let sorted = durations.sorted()
        let result: [String:Any] = ["name":name,"frames":durations.count,"mean_layout_ms":layouts.reduce(0,+)/Double(layouts.count),"mean_layout_render_ms":durations.reduce(0,+)/Double(durations.count),"p95_layout_render_ms":sorted[Int(Double(sorted.count-1)*0.95)]]
        window.contentView = nil
        window.close()
        return result
    }
    @MainActor static func main() throws {
        _ = NSApplication.shared
        let folder = CommandLine.arguments[1]
        let output = CommandLine.arguments[2]
        func decode<T:Decodable>(_ type: T.Type,_ name:String) throws -> T { try JSONDecoder().decode(type,from:Data(contentsOf:URL(fileURLWithPath:folder+"/"+name+".json"))) }
        let store = ManagementStore(restoreSnapshot:false)
        store.serviceAddress = "https://resize-check.invalid"
        store.nodes = try decode([NodeSummary].self,"nodes")
        store.jobs = try decode([JobSummary].self,"jobs")
        store.users = try decode([UserSummary].self,"users")
        store.overview = try decode(OverviewResponse.self,"overview")
        store.serverMonitoring = try decode(ServerMonitoring.self,"server-monitoring")
        store.supportsOverviewMonitoring = true
        store.cacheOverviewHistory(try decode(OverviewHistory.self,"history-24h"),nodeID:nil)
        defer { UserDefaults.standard.removeObject(forKey:"overviewHistory.\(store.serviceAddress)") }
        var results: [[String:Any]] = []
        for (name,root) in [
            ("overview_after",AnyView(OverviewView(store:store))),
            ("traffic_after",AnyView(OverviewHistoryView(store:store,isActive:false))),
            ("quality_after",AnyView(OverviewHistoryView(store:store,category:.quality,isActive:false))),
            ("jobs_after",AnyView(JobsView(store:store))),
            ("overview_after_repeat",AnyView(OverviewView(store:store)))
        ] {
            let result = try measure(name,root:root)
            results.append(result)
            print(String(data:try JSONSerialization.data(withJSONObject:result,options:[.sortedKeys]),encoding:.utf8)!)
            fflush(stdout)
        }
        try JSONSerialization.data(withJSONObject:results,options:[.prettyPrinted,.sortedKeys]).write(to:URL(fileURLWithPath:output+"/results.json"))
    }
}
