# Credential center

The macOS **凭据** page lists operational credentials together with administrator,
user-connection and subscription tokens. Node and user forms select existing
credentials or create them nearby. Settings links to this page; the local login
token continues to live in Keychain.

## Managed material

- SSH private keys and optional passphrases, or SSH passwords.
- TLS certificate pairs (certificate chain and matching private key), independent
  CA certificates with the CA basic constraint, and ECH key files.
- Standalone TLS private-key and leaf-certificate types are removed.
- Complete ACME DNS provider credentials, and general API tokens. The first
  provider integration is the existing ACME DNS configuration; general API
  tokens can be stored and versioned without adding new provider integrations.
- User mTLS certificate/key pairs, restricted to their owning user.

Operations use encrypted immutable versions. References have the form
`credential://<id>/<version>/<field>`. Certificates and keys in a TLS identity
use `certificate` and `private_key`; CA and ECH material uses `content`; DNS
references use the provider's config-field names. Ordinary ACL/GeoIP/GeoSite
files remain node resources. Proxy configuration selects one TLS certificate-pair version, and independent CA and ECH references. Its
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
other successes. Retry applies only failed items from the latest, unarchived
credential version, rechecking current optimistic revisions. Newer publications
supersede queued older work. Rollback resolves historical credential versions;
it continues using the currently verified SSH connection.

mTLS changes update the relevant user's assignments and queue disconnection of
old client sessions. Administrator/user/subscription tokens keep their existing
business rotation and revocation behavior. The API prevents revoking the token
used by the current request or the last active administrator token.

Archiving keeps existing use available and prevents new bindings/publications.
Deletion is refused while current/deployed configurations, saved history or
unfinished tasks reference the credential. Deleting a user archives their
owned mTLS material. Public metadata can be cached offline; detail reads and
all writes require a live connection.

## Expiry and verification

Uploaded certificate metadata supplies expiry dates. API-token reminder dates
can be entered manually; unspecified expiry is shown as unknown. With system
notifications enabled, running connected clients notify at 30/14/7 days and
after expiry, deduplicating by service, credential version and threshold. The
center does not take over remote ACME issuance or renewal monitoring.

Management API 1.0.0 keeps `/api/v1` and changes its contract in place; upgrade clients and server together. Credential CRUD and publication
are under `/credentials`; references are under `/credentials/{id}/references`;
batch state and retry are under `/credential-batches/{id}`. Node writes accept
SSH credential IDs/versions, and assignment writes accept mTLS IDs/versions.
Those positions reject inline secret fields. See the OpenAPI contract for
request and response definitions.

Verification commands:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/generate-swift-api-models.rb --check
scripts/verify-node-package-client.sh
scripts/verify-subscription-client.sh
python3 scripts/verify-credentials-live.py
```

Live checks use disposable PostgreSQL schemas and two disposable systemd nodes;
set `TEST_DATABASE_URL` and install `scripts/requirements-test.txt`. The live
credential test covers shared TLS updates, pinned rollback, partial failure and
retry, consecutive publication, unauthorized SSH keys, encrypted SSH-key
passphrases, and manually prepared shared-password changes.

The TLS-pair upgrade changes v1 directly. Startup consolidates split certificate
and key references across desired, deployed and historical configuration,
preserving runtime bytes and artifact filenames. It removes the old standalone
objects after conversion and retains unused CA certificates as `ca_certificate`.
A referenced invalid pair aborts migration. Restore the pre-upgrade database and
matching image for rollback; old private-key objects cannot be recovered through
the upgraded API. See [0008](plans/0008_tls_identity_consolidation_plan.md).
