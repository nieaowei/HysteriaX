import SwiftUI

extension DNSRecord {
    var recordTypeColor: Color {
        switch recordType {
        case "A": .blue
        case "AAAA": .indigo
        case "CNAME": .purple
        case "TXT": .orange
        case "MX": .teal
        case "NS": .cyan
        default: .secondary
        }
    }

    var syncColor: Color {
        switch state {
        case "synced": .green
        case "pending": .blue
        case "failed", "remote_missing": .orange
        default: .secondary
        }
    }

    var resolutionColor: Color {
        switch resolutionStatus {
        case "verified": .green
        case "pending": .orange
        case "proxied": .purple
        default: .secondary
        }
    }

    var originColor: Color { origin == "hysteriax" ? .blue : .secondary }
}

struct DNSRecordTypeBadge: View {
    let record: DNSRecord

    var body: some View {
        Text(record.recordType)
            .font(.caption2.monospaced().weight(.semibold))
            .foregroundStyle(record.recordTypeColor)
            .padding(.horizontal, 5).padding(.vertical, 1)
            .background(record.recordTypeColor.opacity(0.12), in: Capsule())
    }
}
