# Backup and recovery

SQLite WAL files and the encryption master key must be backed up together. The key is stored outside the database by design. Losing it makes encrypted SSH credentials, node tokens, user credentials, subscription tokens, and configuration snapshots unusable.

## Backup

From the repository directory, create a verified archive:

```sh
scripts/backup-restore.py backup
```

The command checks that the API is ready, stops it while archiving the complete `data/` directory (SQLite database, WAL sidecars, and encrypted resources), verifies SQLite integrity and file hashes, then restarts the API and waits for readiness. It writes a mode-600 archive under `backups/` by default; an alternate destination can be passed as an argument.

The archive contains a SHA-256 fingerprint of the encryption key, not the key itself. Protect the archive as a secret and keep a separate encrypted copy of `HYSTERIAX_MASTER_KEY`. Do not store both in the same unprotected location. Caddy's named certificate volumes are outside `data/` and need their own protected backup if preserving certificate state is required.

When the service detects pending schema migrations on an existing SQLite file, it first writes a standalone database snapshot to `backups/migrations/` (or `HYSTERIAX_MIGRATION_BACKUP_DIR`). The snapshot is checked with SQLite `quick_check` and stored with owner-only directory/file permissions. Keep the snapshot until the upgraded service is healthy. It protects the database schema and rows; it does not replace the full data/resource and master-key backup described above.

## Restore

Restore with the matching encryption key already set in `.env`:

```sh
scripts/backup-restore.py restore backups/hysteriax-data-YYYYMMDDTHHMMSSZ.tar.gz
```

The command validates archive paths and checksums, runs SQLite's integrity check, and refuses an archive whose key fingerprint differs from the current `.env`. It stops the API only after validation, preserves the current `data/` directory as `data.before-restore-<UTC timestamp>/`, restores the archive, then waits for `/readyz`. If readiness fails, it puts the previous directory back and attempts to restart the service.

Do not combine a database from one backup with a master key from another installation. Before schema upgrades, run the full backup command and keep its verification output; the automatic pre-migration snapshot is an additional recovery point for the SQLite database.

Run the archive unit tests and the isolated Docker Compose integration with:

```sh
python3 -m unittest discover -s tests -p 'test_backup_restore.py'
scripts/verify-backup-restore-compose.py
```

The integration builds the API image, exercises backup and restore against a temporary container and directories, verifies a restored resource file, and waits for `/readyz` after each restart. `.dockerignore` excludes production `data/`, `backups/`, and `.env*` from the image build context.
