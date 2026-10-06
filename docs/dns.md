# DNS records and node domains

DNS management is available from the macOS **DNS 记录** sidebar page. The first
provider is Cloudflare. A, AAAA and CNAME records can be created, edited and
deleted; other record types are visible but read-only. Existing provider records
can be edited directly without a separate adoption step.

## Connect a zone

1. Save a Cloudflare DNS credential in the credential center.
2. Open **DNS 记录 → 连接与域名…** and create a named connection using that credential.
3. Verify the connection to discover accessible zones, then enable the zones to
   manage and refresh their records.

The token needs zone discovery/read access and DNS record write access for the
selected zone. Verification checks readable resources; a successful read does
not prove write permission. Write tasks report a provider authorization failure
if editing is denied. Secrets remain in the encrypted credential vault and are
never returned through DNS APIs or cached in the client.

DNS connections reference exact credential versions. Publishing a new version
creates a DNS connection batch item, verifies the candidate against enabled
zones, and switches the connection only after success. Failure preserves the old
version. Retry from the latest credential batch. Existing ACME references keep
using their own credential publication/application workflow.

## Assign a node domain

Node creation offers four address modes:

- External domain/IP preserves the existing workflow.
- Automatic allocation selects a zone, optional prefix and public IPv4/IPv6.
  Names use `<prefix>-<stable node ID suffix>.<zone>` (default prefix `node`).
- Manual allocation creates a chosen subdomain with A/AAAA records.
- Existing records bind one CNAME or a same-name A/AAAA pair without changing
  their targets or TTL.

Node allocation creates DNS-only records. A node owns one primary connection
hostname; a hostname cannot be bound to multiple nodes. SSH addresses are
independent from public DNS targets. There is no background IP discovery or
DDNS: subsequent target changes are explicit edits.

**Domain assignment does not enable or modify ACME.** After creating the node,
open its proxy configuration, choose ACME or an uploaded TLS identity, and use
**使用已分配域名** to add the assigned hostname to the ACME domain list. Existing
SNI values and certificate choices are retained. DNS-01 TXT challenge records
remain the responsibility of Hysteria's ACME implementation.

The proxy configuration's **公开连接与域名** section supports reassignment and
switching to an external address. Domain changes save independently and reload
the proxy configuration; save other draft changes first.

## Writes, checks and publication

DNS writes run as persisted tasks in the management service, independent of the
Mac. Resource locks serialize writes within a zone and preserve node task
ordering. Operations have stable idempotency keys and retain the credential
version selected when queued. A new explicit retry pins the connection version verified at its own enqueue time, while retaining the original operation history. Transport errors, HTTP 429 and HTTP 5xx retry with
bounded delays (including HTTP-date Retry-After); delays beyond one day require manual handling. Invalid input, permissions and external changes require review.

A provider timeout can happen after a record was created. Recovery searches for
the operation's provider comment before creating anything else. Unrelated
existing records are not overwritten. Updates compare the provider's current
editable fields with the stored baseline; refresh and review if someone has
changed the record externally. A refresh can reconcile terminal failed edits
without overwriting a queued/running write.

“已写入” and “解析已验证” are separate states. Checks compare authoritative DNS and
the management service's resolver, recording the observed answers and time.
CNAME flattening is checked against target address results. A known proxied
CNAME target is not accepted as a direct node endpoint. Checks wait up to ten
minutes using delayed tasks, then require a new check. Verification does not
promise that every client cache has expired.

Subscriptions use a **published connection snapshot**, not pending target
addresses. Deployment, synchronization and rollback probe the executed revision
and publish its endpoint only after the DNS and existing node health/traffic
checks succeed. A failed new-domain deployment keeps the previous subscription
endpoint. Normal service restarts still follow the existing deployment workflow;
this is not a zero-downtime migration guarantee.

Directly changing an active hostname's IP affects existing clients immediately
as their caches refresh. Preserving the published hostname does not preserve its
old IP after such a DNS edit.

## Unbinding and deletion

Unbinding requires a replacement public address and retains DNS records. Records
used by a current binding or published endpoint cannot be deleted; complete the
address switch first. Renaming/changing the type/enabling proxy on such a record
also requires reassignment first. Node uninstall and management-record removal
retain DNS records for explicit cleanup. Pending or uncertain node DNS writes
must finish or be reconciled before node removal.

Connection deletion is refused while zones, operation history or credential
batches reference it. Credential deletion is also protected by DNS connections.
Public record snapshots remain visible offline, but detail reads and writes
require a connected management service. Older services show an upgrade message.

## API and verification

The `/api/v1/version` feature `dns_management` gates the client UI. Connections,
zones and records are under `/api/v1/dns`; node bindings are under
`/api/v1/nodes/{id}/dns-binding`. Remote operations return HTTP 202 and task
identifiers. Mutation requests use expected revisions and idempotency keys;
reusing a key for a different request returns HTTP 409. DNS tasks appear in the
normal task/event APIs with resource type, ID and name. See the OpenAPI contract
for typed request and response definitions.

Verification:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/generate-swift-api-models.rb --check
scripts/verify-dns-client.sh
python3 scripts/verify-dns-ui.py
scripts/verify-acme-dns-client.sh
scripts/verify-subscription-client.sh
scripts/verify-node-package-client.sh
./script/build_and_run.sh --verify
```

Rust database tests require an isolated `TEST_DATABASE_URL`. Provider tests use
a local HTTP fixture, including a provider commit followed by an error response.
Live verification must use a dedicated subdomain prefix in an explicitly chosen
zone and clean up only the resources it created.
