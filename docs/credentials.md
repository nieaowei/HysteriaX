# Credential center

The macOS **凭据** page lists operational credentials together with administrator,
user-connection and subscription tokens. Node and user forms select existing
credentials or create them nearby. Settings links to this page; the local login
token continues to live in Keychain.

## Managed material

- SSH private keys and optional passphrases, or SSH passwords.
- TLS certificate pairs (certificate chain and matching private key), independent
  CA certificates with the CA basic constraint, and ECH key files.
- Complete ACME DNS provider credentials and versioned API tokens.
- CA identities with a signing key, generated or imported with the required chain. The service issues managed mTLS user identities and node probes.

Operations use encrypted immutable versions. References have the form
`credential://<id>/<version>/<field>`. Certificates and keys in a TLS identity
use `certificate` and `private_key`; CA and ECH material uses `content`; DNS
references use the provider's config-field names. Ordinary ACL/GeoIP/GeoSite
files remain node resources. Proxy configuration selects one TLS certificate-pair version and an ECH reference. Managed mTLS selects a CA identity; the service generates clientCA trust files. Its
**创建…** buttons open the shared credential editor and select the new version
after creation. Version management remains in the credential center. The configuration-resource uploader accepts only ACL, GeoIP and GeoSite.
Existing remote-file selections are preserved when opening the form.

Imported secrets are not viewable or exportable. Public certificates, subjects,
domains, fingerprints and validity times remain visible. Administrator tokens
are stored as digests and shown only at creation; existing subscriber downloads
and authorized subscription exports still include the material clients need.
Node callback/statistics credentials remain internally managed. The service
master key and database bootstrap password remain external configuration.

## Publication and failure handling

**发布并更新全部引用** creates a new version and automatically schedules every
current reference. It does not rewrite historical configuration versions.

The operator first authorizes new SSH public keys, changes remote passwords, or
creates replacement third-party tokens. The product does not modify remote SSH
authorization, account passwords or provider token lifecycles. SSH application
verifies the new credential and pinned host key before replacing its binding.
Failure retains the old binding, although a manually changed remote password
may already have made that old credential unusable.

TLS/ECH/DNS changes create new node configuration revisions and deploy them.
The center distinguishes target references from successfully deployed ones,
and shows each batch item's actual job result. One failed node does not undo
other successes. Ordinary credential retry applies only failed items from the latest, unarchived
version, rechecking current optimistic revisions. Newer ordinary publications
supersede queued older work. CA updates instead use persisted generation, phase
and actual-reference guards; continuous rotations and delayed trust cleanup can coexist. Rollback resolves historical credential versions;
it continues using the currently verified SSH connection.

Managed mTLS signs one shared identity per user and CA credential ID. Ordinary
renewal and manual reissue do not change business user revisions or disconnect
clients; previously issued certificates remain valid until natural expiry. Administrator/user/subscription tokens keep their existing
business rotation and revocation behavior. The API prevents revoking the token
used by the current request or the last active administrator token.

Archiving keeps existing use available and prevents new bindings/publications.
Deletion is refused while current/deployed configurations, saved history or
unfinished tasks reference the credential. Deleting a user archives their
shared mTLS identity and retains unexpired leaves for old-trust retention. Public metadata can be cached offline; detail reads and
all writes require a live connection.

## Expiry and verification

Uploaded certificate metadata supplies expiry dates. API-token reminder dates
can be entered manually; unspecified expiry is shown as unknown. With system
notifications enabled, running connected clients notify at 30/14/7 days and
after expiry, deduplicating by service, credential version and threshold. The
center supplies DNS credentials to the node-local `hysteriax-acme` service. The node handles issuance and renewal independently, while management collects its status over SSH.

Management API 2.0.0 uses `/api/v2`. Upgrade clients and server together. Administrative writes require a stable `Idempotency-Key`. Credential CRUD and publication
are under `/credentials`; references are under `/credentials/{id}/references`;
batch state and retry are under `/credential-batches/{id}`. Node writes accept
SSH credential IDs/versions and mtls off/managed settings with an exact currently
published CA ID/version. Direct personal node-assignment and certificate-binding
routes are removed; authorization groups issue shared identities automatically.
Public node writes reject hand-written tls.clientCA and inline secret fields. See the OpenAPI contract for
request and response definitions.

Verification commands:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/generate/generate-swift-api-models.rb --check
apps/macos/Tests/runners/verify-node-package-client.sh
apps/macos/Tests/runners/verify-subscription-client.sh
python3 tests/integration/verify-credentials-live.py
```

Live checks use disposable PostgreSQL schemas and two disposable systemd nodes;
set `TEST_DATABASE_URL` and install `tests/requirements.txt`. The live
credential test covers shared TLS updates, pinned rollback, partial failure and
retry, consecutive publication, unauthorized SSH keys, encrypted SSH-key
passphrases, and manually prepared shared-password changes.

## Certificate maintenance

On ACME nodes, the independent `hysteriax-acme` process maintains one SAN certificate covering the configured domains. DNS secrets used during deployment go into the certificate account's private configuration. That renewal runs independently of [managed mTLS](managed-mtls.md). Back up node ACME accounts and keys as described in [recovery](backup-restore.md#节点-acme-账户与私钥).

CA generation publication makes prepared user certificates available atomically through `mtls_user_version_publication`. The maintenance scheduler then updates renewal deadlines.

## User connection and subscription credentials

Both secret types live in `user_credentials`, distinguished by `kind`. New users receive an independent connection password and subscription token in one transaction. `POST /api/v2/users/{id}/credentials/rotate` revokes both old values, issues both replacements, increments the user revision once, and queues disconnection of existing sessions. Its encrypted idempotency receipt replays the committed pair. `GET /api/v2/users/{id}/subscription` reads the current subscription; independent subscription rotation is removed. The Mac user menu exposes one joint rotation action. After rotation, copy the new subscription URL and update clients.

The credential directory shows one connection entry per user and a history of subscription tokens. Eligible revoked values are pruned after 90 days. Active credentials remain available. See [database history retention](installation.md#database-history-retention).
