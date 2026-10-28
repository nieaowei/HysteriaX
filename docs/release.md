# macOS unsigned DMG release

HysteriaX is distributed directly as an unsigned downloadable DMG; it is not submitted to the Mac App Store. Release builds do not need an Apple Developer Program membership, Developer ID certificate, Keychain, or notarization credentials. Pushing a `vX.Y.Z` tag builds the universal app, packages it as a compressed UDZO DMG, verifies that the DMG mounts and contains `HysteriaX.app` plus an `Applications` link, then attaches it to the GitHub release.

The Universal app contains `arm64` and `x86_64` slices and targets macOS 26.0. The Xcode release project sets the minimum macOS version to 26.0 for both slices; this is separate from the SwiftPM development manifest. See [compatibility](compatibility.md).

Both CI and the release script run `scripts/verify-macos-app.sh` to check the two executable slices, bundle minimum, and Mach-O minimum versions before accepting the app.

Run `scripts/release-macos.sh` locally with `HYSTERIAX_RELEASE_VERSION` set to a version such as `1.2.3`. The DMG is written under `dist/` by default; set `HYSTERIAX_RELEASE_OUTPUT_DIR` to choose another output directory. The script does not access Keychain or sign the app or disk image.

The DMG has no signature or notarization ticket. The app is built without a Developer ID identity; modern linkers may still give the Mach-O binary an ad hoc signature, which does not identify a developer or satisfy Gatekeeper. macOS Gatekeeper may warn or block the downloaded app, and users may need to explicitly allow it to open in macOS security settings. This is an expected tradeoff of direct unsigned distribution.

The same `vX.Y.Z` tag also triggers `.github/workflows/release-container.yml`, which publishes versioned and `latest` multi-architecture images to `ghcr.io/<owner>/hysteriax-server` for `linux/amd64` and `linux/arm64`. Set the GHCR package visibility to public before using it as a public installation source. Set `HYSTERIAX_IMAGE=ghcr.io/<owner>/hysteriax-server` and `HYSTERIAX_VERSION=vX.Y.Z` in `.env`. This release uses `0001_baseline.sql`; new installations require an empty database and no automatic upgrade from the former 0001–0074 chain is provided. The verified production database completed explicitly authorized metadata normalization to baseline 1; see [database baseline requirements](installation.md#database-baseline). Retain the matching old image/database/key for the original installation. Build the new image locally and transfer it to the server. For a fresh installation, run `docker compose --profile maintenance run --pull never --rm --no-deps migrate` against an empty database before startup. For the normalized production database, start the corresponding image without replaying the baseline SQL. Do not run old and new management versions against the same database. See [database baseline requirements](installation.md#database-baseline).

## Subscription-template initialization

Use matching server and Mac client versions. The SQL baseline defines template storage, and explicit maintenance initialization creates encrypted published defaults. Replace saved `/sub/{token}/clash.yaml` links with `/sub/{token}?template={id}&platform={target}`; the old route now returns 404. Management responses expose only the canonical `url`, and the Mac client requires an explicit destination platform for native exports. Draft saves do not affect subscribers; publication/rollback do.

Backups include `subscription_templates`, `subscription_template_versions`, and their initialization marker through the existing whole-database archive. Restoring them requires the same master key. Build the container image locally and push/transfer it to the target server according to the project deployment requirement.
