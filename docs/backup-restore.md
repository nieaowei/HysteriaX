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
