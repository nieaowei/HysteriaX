# Subscription formats

## Addresses and authentication

`GET /sub/{token}` selects the format from User-Agent. `format=auto` has the same
behavior. An explicit `format=mihomo|singbox|base64|uri` overrides format detection;
it does not override the capability check. Invalid or repeated format values return 400.
The existing `/sub/{token}/clash.yaml` always returns the established Mihomo YAML,
including its current advanced fields, regardless of UA or format query parameters.
Platform filtering also applies to this fixed YAML endpoint.

Management subscription responses retain `url` (the fixed YAML address) and add
optional `auto_url`. Both addresses share the existing token and revocation rules.
The default macOS copy/rotation action uses `auto_url`, falling back to `url` with
an older server. No token rotation or database migration is needed for this feature.

All public subscription requests, including the selection page, check token
revocation, account enabled state, expiration, and quota. Missing/revoked tokens
return 404 and inactive accounts return 403. Responses include
`Cache-Control: private, no-store`; automatic responses also use `Vary: User-Agent`.
Logs omit tokens and node credentials. Avoid placing subscription addresses in
public logs or screenshots.

## Detection and outputs

| UA token (case insensitive) | Output |
| --- | --- |
| mihomo, clash.meta, clashmeta | Mihomo YAML |
| sing-box, singbox | sing-box JSON |
| shadowrocket, v2rayn, v2rayng | Base64 node links |
| Browser, curl, empty/unknown UA, ordinary Clash | Static selection page |

UA tokens use word boundaries and extract versions immediately after `/` or
whitespace. A wrapper application's version is never used as the sing-box kernel
version. Prerelease/malformed versions are treated as unknown; sing-box JSON then preserves advanced nodes and their required fields.

The selection page presents explicit-format links, clipboard copying, and a
manual selection fallback. It loads no external assets, disables indexing, and
sends no referrer. Copy the chosen link into your client; opening it in a browser
uses an unknown kernel version; sing-box JSON preserves all representable nodes.

Mihomo uses the fixed configuration in `src/api/subscriptions/mihomo-template.yaml`:
local mixed and redirect ports, LAN access, DNS-over-HTTPS/TLS with fake-IP,
the `SELECT`, `PROXY`, and `IPFake` groups, the configured rule providers, and
the template's ordered rules. The assigned nodes and their connection credentials
are generated dynamically into `SELECT`; an empty assignment uses `DIRECT` there.
`allow-lan: true` and `bind-address: '*'` expose the local proxy listeners to
devices that can reach the client machine's network interfaces.
The template deliberately omits the sample's external controller and shared secret.
sing-box uses the policy in `singbox-template.json`, with HTTP port 8080, SOCKS
port 1080, DNS port 1053, Redirect/TProxy listeners and a TUN interface where
supported by the target platform. Its TUN enables automatic routing by default
(`auto_route: true`); Android/iOS omit this option because the VPN service manages routing.
URI output contains one `hysteria2://` link per
line; Base64 encodes exactly that UTF-8 list, including the trailing newline. An empty
URI/Base64 subscription is empty. URI lists carry connection parameters rather than
client routing or bandwidth tuning.

## Capability checks

Only successfully deployed node configuration is rendered; pending desired
configuration never changes subscriptions until deployed. Node ordering and names
are the same as the original YAML renderer.

| Feature | Mihomo | sing-box | URI/Base64 |
| --- | --- | --- | --- |
| Basic Hysteria2, SNI, TLS verify flag, Salamander | Existing renderer | 1.14+ baseline | Supported |
| Port hopping | Existing renderer | 1.14+ baseline, including unknown version | Filtered pending client validation |
| mTLS certificate/key | Existing renderer | Unknown version or sing-box 1.14+ | Cannot embed; filtered |
| ECH config list | Existing renderer | Unknown version or sing-box 1.14.2+ validation profile | Filtered pending client validation |
| Gecko | Existing renderer | Unknown version or sing-box 1.14+ | Filtered pending client validation |
| Realm and STUN/insecure settings | Existing renderer | Unknown version or sing-box 1.14+ | Filtered pending client validation |

sing-box 1.14 enables ChromeParrot (a Chrome-like QUIC ClientHello) for Hysteria2 by
default, and that path of its QUIC stack refuses a TLS configuration carrying a client
certificate (`quic: server-side tls.Config fields are not supported with ChromeParrot`).
Hysteria2 outbounds that carry a client certificate are therefore emitted with
`disable_chrome_parrot: true`; nodes without one keep the default ChromeParrot handshake.

The complete sing-box template requires 1.14.0 or newer. Known sing-box kernels
below 1.14.0 receive HTTP 422 (`subscription_incompatible`) even when the user has
no nodes. There is no legacy template or older-version field downgrade. ECH nodes
still require the independently validated 1.14.2 profile for known kernels.

## Target platform

Both automatic subscriptions and `/sub/{token}/clash.yaml` accept
`platform=auto|linux|openwrt|windows|macos|android|ios` (`openwrt` aliases Linux).
Invalid or repeated platform parameters return 400. Explicit platform overrides UA
platform detection; omitted/auto uses OS tokens from the UA. Android and iOS take
precedence over Linux/macOS tokens in mobile UAs. Downloaders without OS tokens
retain the original template. Platform and kernel version are independent: setting
`platform` never bypasses the kernel version check.

Examples: `/sub/{token}?format=singbox&platform=windows` and
`/sub/{token}/clash.yaml?platform=android`. Browser UAs describe the downloading
machine, which may differ from the destination; use an explicit platform when
exporting for another device. A bare `sing-box` UA has no platform information.

| Target | sing-box filtering | Mihomo filtering |
| --- | --- | --- |
| Unknown / Linux / OpenWrt | Original listeners and TUN options | Original template |
| macOS | Remove TProxy, TUN `auto_redirect` and fixed `interface_name` | Keep Redirect |
| Windows | Remove TProxy, Redirect and TUN `auto_redirect` | Remove `redir-port` |
| Android / iOS | Remove TProxy, Redirect and TUN `interface_name`, `auto_redirect`, `auto_route` (managed by VPN service) | Remove `redir-port` |

Node credentials, DNS rules, routing policy, and proxy selection groups are
preserved. The DNS listener remains present, so `dns-in` route references remain
valid. Filtering preserves the template
settings; it does not configure external Redirect/TProxy firewall rules.
Platform-specific output is isolated in the response cache, and both subscription
endpoints include `Vary: User-Agent`. This is configuration filtering, not a claim
of runtime validation on every OS or GUI client.

Mihomo output is validated with v1.19.31. Auto requests identifying an older Mihomo
version conservatively filter mTLS/ECH/Gecko/Realm nodes; the fixed legacy YAML
endpoint preserves its original contract. A known sing-box version older than 1.14
is rejected, including empty subscriptions. For explicit sing-box exports from the management
app or a browser, no kernel version is available. Unknown sing-box kernel versions retain all representable nodes, including complete mTLS,
ECH, Gecko and Realm settings. Recognized older kernels continue to apply their known
compatibility limits. No extra version parameter is required.

sing-box only represents integral Mbps bandwidth limits. Limits that cannot be
converted exactly to its integer fields cause the node to be filtered rather than
rounded or silently dropped. Realm output omits the incompatible direct server and
port fields, and keeps its HTTP-client TLS policy separate from the Hysteria TLS policy.

Partially incompatible subscriptions return compatible nodes with the count in
`X-HysteriaX-Filtered-Nodes`. No synthetic warning nodes are added. When there are
nodes but all are incompatible, the response is 422 with error code
`subscription_incompatible`. A genuinely unassigned user receives the normal empty
subscription instead. Closed-source clients' advanced URI extensions remain disabled
until validated on actual clients.

## Validation and rollout

`verify-subscription.sh` uses an isolated PostgreSQL schema and checks old addresses,
automatic detection, pages, explicit formats, filtering, revoked/inactive accounts,
Unicode link encoding, and empty subscriptions. It parses YAML with digest-pinned
Mihomo v1.19.31 and advanced JSON with digest-pinned sing-box v1.14.2. A temporary
local Hysteria2 server and HTTP target verify real SOCKS5 traffic through the generated
sing-box configuration. `verify-subscription-client.sh` checks Swift DTO fallback,
format URLs/extensions, and rejecting HTML during export.

Publish the server first, then the macOS app. No migration is necessary, and old
subscription addresses continue to work. Requests log the selected client, version,
format, and filtering reason, without including tokens or connection secrets.

References: [sing-box Hysteria2](https://sing-box.sagernet.org/configuration/outbound/hysteria2/),
[sing-box TLS](https://sing-box.sagernet.org/configuration/shared/tls/),
[Hysteria URI scheme](https://v2.hysteria.network/docs/developers/URI-Scheme/).
