# Backup and recovery

PostgreSQL stores encrypted service state; `HYSTERIAX_MASTER_KEY` is held separately in `.env`. Losing that key makes encrypted SSH credentials, node tokens, user credentials, subscription tokens, and configuration snapshots unusable. Back up the key separately from the database archive.

## Backup

From the repository directory, create a verified archive:

```sh
scripts/backup-restore.py backup
```

The command requires a ready API, stops it to make a consistent snapshot, creates a PostgreSQL custom-format dump, and then restarts the API. The mode-600 archive under `backups/` contains the dump, schema version, SHA-256 checksums, and a fingerprint of the master key. It never contains the key. An alternate destination can be passed as an argument.

The archive is a database snapshot. Caddy's named certificate volumes are separate and require their own protected backup when their state matters. Retain the key needed to decrypt the records in each database archive.

## Restore

Restore with the matching encryption key already set in `.env`:

```sh
scripts/backup-restore.py restore backups/hysteriax-postgresql-YYYYMMDDTHHMMSSZ.tar.gz
```

The command verifies archive paths, checksums, format, and key fingerprint, then restores into a temporary PostgreSQL database and checks its schema version. It stops the API only after this validation, renames the current database aside, promotes the restored database, and waits for `/readyz`. If readiness fails, it moves the failed database aside and restores the previous database. The previous database is retained after a successful restore and its name is printed.

SQLite archive format version 1 is not accepted. This release does not include an SQLite importer.

Run the archive unit tests and isolated Compose integration with:

```sh
python3 -m unittest discover -s tests -p 'test_backup_restore.py'
scripts/verify-backup-restore-compose.py
```

The integration restores a real PostgreSQL dump, verifies a stored row, checks API readiness, and exercises staging before promotion. `.dockerignore` excludes `.env*`, backups, and production data from image build context.

## Credential version recovery

Backups include encrypted credential versions, batch items, application completion markers and pinned configuration snapshots. Keep the matching master key outside the database. The credentials upgrade removes legacy secret columns after transactional conversion; downgrade recovery requires the pre-upgrade database backup and original image together. The migration-only mode is suitable for validating a restored copy without starting SSH, quota or deployment workers. Unfinished application jobs recover after restart; already committed applications acknowledge their completion marker instead of changing bindings again.

The [managed mTLS upgrade](managed-mtls.md) removes old user certificate bindings
and invalidates old mTLS rollback snapshots. Restore the pre-upgrade database and
matching image to return to that protocol. Migration-only mode does not perform
remote disconnects or deployments; those durable tasks run only when the service
starts normally. Nodes requiring reconfiguration remain inaccessible until an
explicit off/managed configuration has been deployed successfully.

## 节点 ACME 账户与私钥

管理数据库备份包含 ACME 策略和凭据引用，**不包含节点上生成的 ACME 账户私钥、EAB、服务端私钥和证书版本**。每个节点须单独以加密方式备份 `/etc/hysteriax/acme.json`、`/var/lib/hysteriax-acme/`、`/var/lib/hysteriax/certificates/` 及实际 `acme.dir`（默认 `/var/lib/hysteriax/acme`）。备份前停止证书组件或持有节点本地写锁，保留属主、权限与 `current` 符号链接，避免混合不同版本的证书和私钥。

恢复后先检查目录所有权与有效期，再执行显式同步。不要将私钥、DNS 秘密或 EAB 附在诊断日志中。仅恢复管理数据库不会自动修改既有节点，也不会自动迁移内置 ACME。

Subscription templates, encrypted drafts, immutable publication history, default pointers, and the initialization marker are included in the whole-database backup (migration 0037). Preserve the same master key when restoring. A restart must restore existing publication state rather than re-seed or publish drafts. See [subscription upgrade](subscriptions.md).
