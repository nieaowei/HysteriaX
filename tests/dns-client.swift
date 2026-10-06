import Foundation

@main
struct DNSClientTests {
    static func main() throws {
        let zone = try JSONDecoder().decode(DNSZone.self, from: Data(#"{"id":"zone","connection_id":"connection","provider_zone_id":"remote-zone","name":"example.test","enabled":true,"revision":1}"#.utf8))
        let records = try JSONDecoder().decode([DNSRecord].self, from: Data(#"[{"id":"a","zone_id":"zone","name":"hk.example.test","record_type":"A","content":"8.8.8.8","ttl":1,"proxied":false,"origin":"external","revision":1,"state":"synced","resolution_status":"verified","updated_at":"2026-10-06T00:00:00Z"},{"id":"aaaa","zone_id":"zone","name":"hk.example.test","record_type":"AAAA","content":"2606:4700:4700::1111","ttl":300,"proxied":false,"origin":"hysteriax","revision":1,"state":"synced","resolution_status":"unchecked","updated_at":"2026-10-06T00:00:00Z"}]"#.utf8))
        var draft = DNSAllocationDraft()
        let external = try draft.allocation(zones: [zone], records: records)
        precondition(external == nil)
        draft.mode = "manual"; draft.zoneID = "zone"; draft.hostname = "sg"; draft.ipv4 = "8.8.8.8"
        let allocation = try draft.allocation(zones: [zone], records: records)!
        precondition(allocation.hostname == "sg.example.test")
        let encoded = try JSONEncoder().encode(DNSBindingSetRequest(expectedRevision: 12, allocation: allocation))
        let body = try JSONSerialization.jsonObject(with: encoded) as! [String: Any]
        precondition(body["expected_revision"] as? Int == 12)
        precondition((body["allocation"] as! [String: Any])["idempotency_key"] as? String == draft.idempotencyKey)
        draft.mode = "existing"; draft.selectedHostname = "hk.example.test"
        let existing = try draft.allocation(zones: [zone], records: records)!
        precondition(existing.recordIds == ["a", "aaaa"])
        let endpoint = APIEndpoints.setDNSBinding(id: "node")
        precondition(endpoint.method == "PUT" && endpoint.path == "api/v1/nodes/node/dns-binding")
        let delete = APIEndpoints.deleteDNSRecord(id: "record")
        precondition(delete.method == "DELETE")
        let response = try JSONDecoder().decode(DNSBindingResponse.self, from: Data(#"{"binding":null}"#.utf8))
        precondition(response.binding == nil)
        let receipt = try JSONDecoder().decode(DNSActionReceipt.self, from: Data(#"{"node_id":"node","hostname":"hk.example.test","record_ids":["a"],"job_ids":["job"]}"#.utf8))
        precondition(receipt.hostname == "hk.example.test" && receipt.jobIds == ["job"])
        let published = try JSONDecoder().decode(PublishedConnection.self, from: Data(#"{"public_host":"old.example.test","public_port":443,"listen_addr":":443","tls_sni":null,"tls_skip_verify":false}"#.utf8))
        precondition(published.publicHost == "old.example.test" && published.tlsSNI == nil)
        print("DNS client contract, allocation modes, dual-stack selection and published connection tests passed.")
    }
}
