# macOS unsigned DMG release

HysteriaX is distributed directly as an unsigned downloadable DMG; it is not submitted to the Mac App Store. Release builds do not need an Apple Developer Program membership, Developer ID certificate, Keychain, or notarization credentials. Pushing a `vX.Y.Z` tag builds the universal app, packages it as a compressed UDZO DMG, verifies that the DMG mounts and contains `HysteriaX.app` plus an `Applications` link, then attaches it to the GitHub release.

The Universal app contains `arm64` and `x86_64` slices and targets macOS 26.0. macOS 27 and later run the native Apple silicon slice; Intel Macs can run the Intel slice on macOS 26, Apple's final macOS release for Intel hardware.

Both CI and the release script run `scripts/verify-macos-app.sh` to check the two executable slices, bundle minimum, and Mach-O minimum versions before accepting the app.

Run `scripts/release-macos.sh` locally with `HYSTERIAX_RELEASE_VERSION` set to a version such as `1.2.3`. The DMG is written under `dist/` by default; set `HYSTERIAX_RELEASE_OUTPUT_DIR` to choose another output directory. The script does not access Keychain or sign the app or disk image.

The DMG has no signature or notarization ticket. The app is built without a Developer ID identity; modern linkers may still give the Mach-O binary an ad hoc signature, which does not identify a developer or satisfy Gatekeeper. macOS Gatekeeper may warn or block the downloaded app, and users may need to explicitly allow it to open in macOS security settings. This is an expected tradeoff of direct unsigned distribution.

The same `vX.Y.Z` tag also triggers `.github/workflows/release-container.yml`, which publishes versioned and `latest` multi-architecture images to `ghcr.io/<owner>/hysteriax-server` for `linux/amd64` and `linux/arm64`. Set the GHCR package visibility to public before using it as a public installation source. Set `HYSTERIAX_IMAGE=ghcr.io/<owner>/hysteriax-server` and `HYSTERIAX_VERSION=vX.Y.Z` in `.env`, then run `docker compose pull` followed by `docker compose up -d` to deploy that image without building from source.
