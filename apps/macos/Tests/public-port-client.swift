import Foundation

@main
struct PublicPortClientTests {
    static func main() {
        var linked = PublicPortDraft(port: 443, listenAddress: ":443")
        precondition(linked.followsListener)
        precondition(linked.portText(listenAddress: ":8443") == "8443")
        precondition(linked.portText(listenAddress: "[::]:5000-5010,6000") == "5000")
        linked.setCustomPort("9443")
        precondition(!linked.followsListener)
        precondition(linked.portText(listenAddress: ":8443") == "9443")
        let reloaded = PublicPortDraft(port: 9443, listenAddress: ":8443")
        precondition(!reloaded.followsListener)
        precondition(reloaded.portText(listenAddress: ":8080") == "9443")
        linked.followsListener = true
        precondition(linked.portText(listenAddress: ":8080") == "8080")
        precondition(linked.portText(listenAddress: ":") == "")
        for address in ["443", ":0", ":65536", ":-443", ":,443", ":abc"] {
            precondition(PublicPortDraft.firstListenerPort(address) == nil)
        }
        precondition(PublicPortDraft.firstListenerPort(" 0.0.0.0:65535 ") == 65535)
        print("Public port defaults: listener changes, hopping, manual overrides, reloads, and invalid ports passed.")
    }
}
