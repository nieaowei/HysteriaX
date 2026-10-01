# macOS release signing and notarization

HysteriaX is distributed directly as a downloadable DMG; it is not submitted to the Mac App Store. Local development builds do not need Developer ID credentials. The release workflow runs when a `vX.Y.Z` tag is pushed, builds a universal macOS app, signs the app and a read-only compressed DMG with Developer ID Application, submits the DMG to Apple notarization, staples the ticket, and attaches the DMG to the GitHub release.

The Universal archive contains `arm64` and `x86_64` slices and targets macOS 26.0. macOS 27 and later run the native Apple silicon slice; Intel Macs can run the Intel slice on macOS 26, Apple's final macOS release for Intel hardware.

Both CI and the release script run `scripts/verify-macos-app.sh` to check the two executable slices, bundle minimum, and Mach-O minimum versions before accepting the app.

Configure these GitHub Actions secrets before pushing a release tag:

| Secret | Value |
|---|---|
| `HYSTERIAX_DEVELOPER_ID_P12_BASE64` | Base64 encoded Developer ID Application certificate export (`.p12`) |
| `HYSTERIAX_DEVELOPER_ID_P12_PASSWORD` | Password used when exporting the `.p12` file |
| `HYSTERIAX_APPLE_TEAM_ID` | Apple Developer team ID |
| `HYSTERIAX_NOTARY_KEY_ID` | App Store Connect API key ID |
| `HYSTERIAX_NOTARY_ISSUER_ID` | App Store Connect issuer ID |
| `HYSTERIAX_NOTARY_KEY_P8_BASE64` | Base64 encoded App Store Connect API key (`.p8`) |

The local release script is `scripts/release-macos.sh`. It keeps the imported certificate and API key in a temporary keychain and directory, removes both at exit, and writes the notarized universal DMG under `dist/`. The image contains `HysteriaX.app` and an `Applications` link for drag-and-drop installation. Xcode verifies the app signature during the archive build; the script signs and verifies the DMG, submits that DMG as the outermost distribution container, staples and validates its ticket, then checks its Gatekeeper assessment.

The App Store Connect API key in the secrets table authenticates to Apple's notarization service; it does not submit the app to App Store Connect for publication or review. Creating the Developer ID certificate and using notarization still require an Apple Developer Program membership.

The signed workflow has not been run in this checkout because the Apple certificate and App Store Connect key are not configured here. The normal local build remains available through `script/build_and_run.sh` without these secrets.

The same `vX.Y.Z` tag also triggers `.github/workflows/release-container.yml`, which publishes versioned and `latest` multi-architecture images to `ghcr.io/<owner>/hysteriax-server` for `linux/amd64` and `linux/arm64`. Set the GHCR package visibility to public before using it as a public installation source. Set `HYSTERIAX_IMAGE=ghcr.io/<owner>/hysteriax-server` and `HYSTERIAX_VERSION=vX.Y.Z` in `.env`, then run `docker compose pull` followed by `docker compose up -d` to deploy that image without building from source.
