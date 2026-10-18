# Node ACME dependency notices

HysteriaX is MIT licensed; see `LICENSE`. The component uses CertMagic v0.25.4 and the eight libdns provider versions used by unmodified Hysteria v2.12.3. The module graph and checksums are locked in `go.mod` / `go.sum`. Provider mapping follows the upstream MIT-licensed Hysteria application.

Dependency license and notice texts are reproduced below by module. Regenerate with `python3 scripts/update-node-acme-notices.py` after a dependency change. Pebble is an optional test tool and is not linked into the distributed component.

| Module | Version | License / notices |
| --- | --- | --- |
| `github.com/caddyserver/certmagic` | `v0.25.4` | [LICENSE.txt](third_party/github.com_caddyserver_certmagic@v0.25.4_LICENSE.txt) |
| `golang.org/x/sys` | `v0.47.0` | [LICENSE](third_party/golang.org_x_sys@v0.47.0_LICENSE) |
| `github.com/caddyserver/zerossl` | `v0.1.5` | [LICENSE](third_party/github.com_caddyserver_zerossl@v0.1.5_LICENSE) |
| `github.com/klauspost/cpuid/v2` | `v2.3.0` | [LICENSE](third_party/github.com_klauspost_cpuid_v2@v2.3.0_LICENSE) |
| `github.com/libdns/libdns` | `v1.1.1` | [LICENSE](third_party/github.com_libdns_libdns@v1.1.1_LICENSE) |
| `github.com/mholt/acmez/v3` | `v3.1.6` | [LICENSE](third_party/github.com_mholt_acmez_v3@v3.1.6_LICENSE) |
| `golang.org/x/crypto` | `v0.50.0` | [LICENSE](third_party/golang.org_x_crypto@v0.50.0_LICENSE) |
| `golang.org/x/text` | `v0.36.0` | [LICENSE](third_party/golang.org_x_text@v0.36.0_LICENSE) |
| `golang.org/x/net` | `v0.53.0` | [LICENSE](third_party/golang.org_x_net@v0.53.0_LICENSE) |
| `github.com/miekg/dns` | `v1.1.72` | [LICENSE](third_party/github.com_miekg_dns@v1.1.72_LICENSE) |
| `github.com/zeebo/blake3` | `v0.2.4` | [LICENSE](third_party/github.com_zeebo_blake3@v0.2.4_LICENSE) |
| `go.uber.org/multierr` | `v1.11.0` | [LICENSE.txt](third_party/go.uber.org_multierr@v1.11.0_LICENSE.txt) |
| `go.uber.org/zap` | `v1.28.0` | [LICENSE](third_party/go.uber.org_zap@v1.28.0_LICENSE) |
| `go.uber.org/zap/exp` | `v0.3.0` | [LICENSE](third_party/go.uber.org_zap_exp@v0.3.0_LICENSE) |
| `github.com/libdns/cloudflare` | `v0.2.2` | [LICENSE](third_party/github.com_libdns_cloudflare@v0.2.2_LICENSE) |
| `github.com/libdns/duckdns` | `v0.3.0` | [LICENSE.md](third_party/github.com_libdns_duckdns@v0.3.0_LICENSE.md) |
| `github.com/libdns/gandi` | `v1.1.0` | [LICENSE](third_party/github.com_libdns_gandi@v1.1.0_LICENSE) |
| `github.com/libdns/godaddy` | `v1.1.0` | [LICENSE](third_party/github.com_libdns_godaddy@v1.1.0_LICENSE) |
| `github.com/libdns/namecheap` | `v1.0.0` | [LICENSE](third_party/github.com_libdns_namecheap@v1.0.0_LICENSE) |
| `github.com/libdns/njalla` | `v0.0.0-20250815081032-a55a87a8f20f` | [LICENSE](third_party/github.com_libdns_njalla@v0.0.0-20250815081032-a55a87a8f20f_LICENSE) |
| `github.com/libdns/porkbun` | `v1.1.0` | [LICENSE](third_party/github.com_libdns_porkbun@v1.1.0_LICENSE) |
| `github.com/google/go-querystring` | `v1.1.0` | [LICENSE](third_party/github.com_google_go-querystring@v1.1.0_LICENSE) |
| `github.com/hashicorp/go-cleanhttp` | `v0.5.2` | [LICENSE](third_party/github.com_hashicorp_go-cleanhttp@v0.5.2_LICENSE) |
| `github.com/hashicorp/go-retryablehttp` | `v0.7.7` | [LICENSE](third_party/github.com_hashicorp_go-retryablehttp@v0.7.7_LICENSE) |
| `github.com/vultr/govultr/v3` | `v3.20.0` | [LICENSE](third_party/github.com_vultr_govultr_v3@v3.20.0_LICENSE) |
| `golang.org/x/oauth2` | `v0.30.0` | [LICENSE](third_party/golang.org_x_oauth2@v0.30.0_LICENSE) |
| `github.com/libdns/vultr/v2` | `v2.0.4` | [LICENSE](third_party/github.com_libdns_vultr_v2@v2.0.4_LICENSE) |
