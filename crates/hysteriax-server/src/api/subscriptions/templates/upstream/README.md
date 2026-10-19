# Field catalog sources

- `mihomo.schema.json` is generated from MetaCubeX/mihomo **v1.19.31**, commit `ab405bad5beeeac8b003bb01f60f134f6df54471` (GPL-3.0). Source: https://github.com/MetaCubeX/mihomo/tree/v1.19.31 . Regenerate with `go run scripts/generate-subscription-catalog.go /path/to/mihomo-v1.19.31 > crates/hysteriax-server/src/api/subscriptions/templates/mihomo.schema.json`.
- `singbox.schema.json` is the upstream `docs/schema.json` from SagerNet/sing-box **v1.14.2**, commit `af6e64c3b69e6132ebaee0e1a3d24e93903f6709` (GPL-3.0-or-later). Source: https://github.com/SagerNet/sing-box/tree/v1.14.2 . Copy the pinned schema verbatim when refreshing.

License and copyright notices are retained here. Catalog generation does not invoke a proxy kernel. The native sing-box schema also describes other protocols; editable subscription outbounds remain restricted to policy groups and direct actions. Hysteria2 connection material comes only from authorized deployed nodes. Native schema versions are validation baselines, not a higher minimum supported client version.

The read-only Mihomo `proxies` item schema is generated from the pinned `Hysteria2Option`, including its embedded options and nested ECH/Realm structs. The envelope adds the renderer's `type` and `udp` fields; numeric hop intervals are accepted because the native proxy decoder uses weakly typed input. Unknown extensions still produce warnings, and known fields retain type validation.
