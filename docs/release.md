# Release and distribution

## macOS DMG

HysteriaX is distributed directly as an unsigned downloadable DMG; it is not submitted to the Mac App Store. Release builds do not need an Apple Developer Program membership, Developer ID certificate, Keychain, or notarization credentials. Pushing a `vX.Y.Z` tag builds the universal app, packages it as a compressed UDZO DMG, verifies that the DMG mounts and contains `HysteriaX.app` plus an `Applications` link, then attaches it to the GitHub release.

The Universal app contains `arm64` and `x86_64` slices and targets macOS 26.0. The Xcode release project sets the minimum macOS version to 26.0 for both slices; this is separate from the SwiftPM development manifest. See [compatibility](compatibility.md).

Both CI and the release script run `apps/macos/Tests/runners/verify-macos-app.sh` to check the bundled application icon, two executable slices, bundle minimum, and Mach-O minimum versions before accepting the app.

The Debug and Release targets use the layered [Icon Composer document](../apps/macos/Sources/HysteriaX/Resources/AppIcon.icon). Source artwork and export notes are in [the design directory](../apps/macos/Design/AppIcon/hysteriax-lightning-black.md). Rebuild and relaunch the app to inspect the system-rendered icon after changing it.

Run `scripts/build/release-macos.sh` locally with `HYSTERIAX_RELEASE_VERSION` set to a version such as `1.2.3`. The DMG is written under `dist/` by default; set `HYSTERIAX_RELEASE_OUTPUT_DIR` to choose another output directory. The script does not access Keychain or sign the app or disk image.

The DMG has no signature or notarization ticket. The app is built without a Developer ID identity; modern linkers may still give the Mach-O binary an ad hoc signature, which does not identify a developer or satisfy Gatekeeper. macOS Gatekeeper may warn or block the downloaded app, and users may need to explicitly allow it to open in macOS security settings. This is an expected tradeoff of direct unsigned distribution.

## Container images

The `vX.Y.Z` tag also triggers the [container release workflow](../.github/workflows/release-container.yml), which publishes `ghcr.io/<owner>/hysteriax-server` images for Linux amd64 and arm64. For a registry-based installation, set `HYSTERIAX_IMAGE` to the image name and `HYSTERIAX_VERSION` to a fixed tag such as `vX.Y.Z`. The package must be accessible to the deployment host.

For production changes in this project, build the image locally and transfer it to the target server. Follow the [installation and upgrade sequence](installation.md#database-baseline) to select the image, initialize or migrate the database, and verify startup.

## Subscription-template initialization

Database maintenance seeds published defaults on a fresh installation. Whole-database backups include drafts, published versions, default pointers and the initialization marker. Restore them with the original master key. See [subscription addresses and publishing](subscriptions.md).
