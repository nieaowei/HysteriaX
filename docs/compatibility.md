# Compatibility profile

## Management client and protocol

The Xcode release project builds a Universal app with `arm64` and `x86_64` slices and a macOS 26.0 deployment target. The separate [SwiftPM manifest](../apps/macos/Package.swift) declares Swift tools 6.4 and macOS 27; use the Xcode release workflow for the Universal distribution. [Release verification](release.md) checks executable slices and deployment versions.

The current server and client use management protocol 2 and API 2.0.0 at `/api/v2`. Use the matching protocol 2 client. Existing subscription and node-authentication URLs retain their contracts. See [upgrade instructions](installation.md#management-protocol-2-upgrade-0034).

## Pinned node and subscription runtimes

- Managed nodes use Hysteria `app/v2.12.3`, with Linux amd64/arm64 digests in [the pinned checksum file](../deploy/hysteria-v2.12.3.sha256).
- Mihomo verification uses v1.19.31. `scripts/verify-mihomo-config.sh` checks its digest before invoking its native validator. The sing-box native export requirements and template/platform selection are described in [subscriptions](subscriptions.md).
- ECH, Gecko, mTLS and port hopping are emitted only for compatible targets. Port hopping requires nftables/iptables and grants `CAP_NET_ADMIN` only to the relevant node service. Enabled Mimic has no supported Mihomo mapping.
- Realm cannot be combined with port hopping. Mihomo Realm subscriptions use `handshake-timeout: 30`; `scripts/verify-realm-live.py` covers the pinned-client workaround with delayed STUN responses. This documents the pinned version's behavior, not the status of later upstream fixes.
- Managed mTLS requires the matching user identities and a node probe identity. A custom `proxy_probe_url` can select an allowed HTTP 200 target when the default loopback route is blocked. Session-only verification is labeled separately from successful TCP forwarding.

## Evidence and limits

Deployment verification covers Debian 12/13 and Ubuntu 22.04/24.04 on amd64/arm64, including root and sudo SSH variants. See the [verification guide](deployment-testing.md) for the available scripts and environment requirements.

Local Docker acceptance verifies behavior; production capacity is measured for the services actually scheduled on each host. Historical observations do not establish the current state of upstream releases or test hosts.
