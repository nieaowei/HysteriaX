# Backup and recovery

PostgreSQL stores encrypted service state; `HYSTERIAX_MASTER_KEY` is held separately in `.env`. Losing that key makes encrypted SSH credentials, node tokens, user credentials, subscription tokens, and configuration snapshots unusable. Back up the key separately from the database archive.

## Backup

From the repository directory, create a verified archive:

```sh
scripts/backup-restore.py backup
```

The command requires a ready API, stops all API replicas to make a consistent snapshot, creates a PostgreSQL custom-format dump, and then restarts the replicas. The mode-600 archive under `backups/` contains the dump, schema version, SHA-256 checksums, and a fingerprint of the master key. It never contains the key. An alternate destination can be passed as an argument.

The archive is a database snapshot. Caddy's named certificate volumes are separate and require their own protected backup when their state matters. Retain the key needed to decrypt the records in each database archive.

## Restore

Only backups from the new `0001_baseline.sql` history and its compatible successors are supported by this release. Former 0001–0074 databases require their matching old image and master key; they cannot be migrated or restored into this baseline. Version numbers alone do not establish compatibility: ordinary startup verifies checksums too.

Restore with the matching encryption key already set in `.env`:

```sh
scripts/backup-restore.py restore backups/hysteriax-postgresql-YYYYMMDDTHHMMSSZ.tar.gz
```

The command verifies archive paths, checksums, format, and key fingerprint, then restores into a temporary PostgreSQL database and checks its schema version. It stops every API replica only after this validation. Before renaming either database, it compares the external execution, verification, DNS, disconnect and remote-task journals against the current database using sorted row hashes. A missing journal in an older schema matches only an empty current journal; shared original columns are compared across schema versions.

If those journals differ or cannot be checked, automatic restore is refused and the current database is retained. This includes confirmed and uncertain external changes after the backup: rotating an epoch cannot recreate their missing intent records or undo their remote effects. The script resumes the retained current database only after maintenance succeeds; existing uncertainty barriers remain in place. There is no bypass flag. Reconcile the external resources and prepare a coordinated recovery before retrying; do not delete journal rows or clear blocked leases to make the check pass. Database-only updates that do not alter those journals can still be restored.

When the check passes, the command renames the current database aside, promotes the restored database, clears Redis, runs `--migrate-only` with all replicas stopped, and only then starts the API and waits for `/readyz`. Maintenance rotates execution, cache and event epochs without deleting historical events. If maintenance or readiness fails, it moves the failed database aside and restores the previous database, which must also pass maintenance before restart. If both maintenance attempts fail, the API stays stopped. The previous database is retained after a successful restore and its name is printed.

SQLite archive format version 1 is not accepted. This release does not include an SQLite importer.

Run the archive unit tests and isolated Compose integration with:

```sh
python3 -m unittest discover -s tests -p 'test_backup_restore.py'
scripts/verify-backup-restore-compose.py
```

The integration uses three full API containers, restores a real PostgreSQL dump, checks retained business rows and ciphertext, and exercises maintenance before startup and rollback. It also changes an isolated HTTP service after backup and verifies that both confirmed and unconfirmed intents prevent restore without repeating the request or losing the current database. This tests refusal, not provider-specific remote rollback. `.dockerignore` excludes `.env*`, backups, and production data from image build context.

## Credential version recovery

Backups include encrypted credential versions, batch items, application completion markers and pinned configuration snapshots. Keep the matching master key outside the database. The migration-only mode is suitable for validating a restored copy without starting SSH, quota or deployment workers. Work that never dispatched an external mutation can recover after lease expiry. Uncertain external work remains quarantined for explicit verification; already committed applications acknowledge their completion marker instead of changing bindings again.

## 节点 ACME 账户与私钥

管理数据库备份包含 ACME 策略和凭据引用，**不包含节点上生成的 ACME 账户私钥、EAB、服务端私钥和证书版本**。每个节点须单独以加密方式备份 `/etc/hysteriax/acme.json`、`/var/lib/hysteriax-acme/`、`/var/lib/hysteriax/certificates/` 及实际 `acme.dir`（默认 `/var/lib/hysteriax/acme`）。备份前停止证书组件或持有节点本地写锁，保留属主、权限与 `current` 符号链接，避免混合不同版本的证书和私钥。

恢复后先检查目录所有权与有效期，再执行显式同步。不要将私钥、DNS 秘密或 EAB 附在诊断日志中。仅恢复管理数据库不会自动修改既有节点，也不会自动迁移内置 ACME。

Subscription templates, encrypted drafts, immutable publication history, default pointers, and the initialization marker are included in the whole-database backup (the current baseline). Preserve the same master key when restoring. A restart must restore existing publication state rather than re-seed or publish drafts. See [subscription upgrade](subscriptions.md).
