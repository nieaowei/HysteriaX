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
- CA identities with a signing key, from generation or strict chain import. User mTLS identities and node probes are system-issued; manual user certificate import is removed.

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

## Database baseline

The current release supports a fresh PostgreSQL database initialized from `0001_baseline.sql`. It does not upgrade the former migration chain or import legacy credential stores. Existing backups require their matching old image and master key. See [installation](installation.md#database-baseline) for the compatibility boundary and [managed mTLS](managed-mtls.md) for current signing and recovery behavior.

ACME 节点现在由独立 `hysteriax-acme` 进程读取 DNS 凭据并维护一张覆盖全部域名的 SAN 证书，可同时选择托管 mTLS。公共配置及凭据目录继续保留原有引用方式；部署时解析的秘密只写入证书账号的私有配置，不会进入 Hy2 YAML 预览。节点续期与用户/CA 身份续期相互独立。节点账户与私钥的独立备份要求见 [备份恢复](backup-restore.md)，维护及验证命令见 [节点 ACME](node-acme.md)。
