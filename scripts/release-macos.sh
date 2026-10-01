#!/usr/bin/env bash
set -euo pipefail

: "${HYSTERIAX_RELEASE_VERSION:?Set HYSTERIAX_RELEASE_VERSION to the release version without a leading v}"
: "${HYSTERIAX_DEVELOPER_ID_P12_BASE64:?Set the base64 encoded Developer ID Application certificate}"
: "${HYSTERIAX_DEVELOPER_ID_P12_PASSWORD:?Set the certificate export password}"
: "${HYSTERIAX_APPLE_TEAM_ID:?Set the Apple Developer team ID}"
: "${HYSTERIAX_NOTARY_KEY_ID:?Set the App Store Connect API key ID}"
: "${HYSTERIAX_NOTARY_ISSUER_ID:?Set the App Store Connect issuer ID}"
: "${HYSTERIAX_NOTARY_KEY_P8_BASE64:?Set the base64 encoded App Store Connect API key}"

RELEASE_VERSION="${HYSTERIAX_RELEASE_VERSION#v}"
if [[ ! "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][A-Za-z0-9.-]+)?$ ]]; then
  echo "HYSTERIAX_RELEASE_VERSION must be a version such as 1.2.3 or 1.2.3-rc.1" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP_DIR="$(mktemp -d)"
KEYCHAIN_PATH="$TEMP_DIR/hysteriax-release.keychain-db"
KEYCHAIN_PASSWORD="$(openssl rand -hex 32)"
CERTIFICATE_PATH="$TEMP_DIR/developer-id.p12"
NOTARY_KEY_PATH="$TEMP_DIR/AuthKey.p8"
ARCHIVE_PATH="$TEMP_DIR/HysteriaX.xcarchive"
APP_PATH="$ARCHIVE_PATH/Products/Applications/HysteriaX.app"
DMG_STAGING_DIR="$TEMP_DIR/dmg-root"
OUTPUT_DIR="${HYSTERIAX_RELEASE_OUTPUT_DIR:-$REPO_ROOT/dist}"
OUTPUT_DMG="$OUTPUT_DIR/HysteriaX-$RELEASE_VERSION-macOS-universal.dmg"

cleanup() {
  security delete-keychain "$KEYCHAIN_PATH" >/dev/null 2>&1 || true
  rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

mkdir -p "$OUTPUT_DIR"
printf '%s' "$HYSTERIAX_DEVELOPER_ID_P12_BASE64" | /usr/bin/base64 -D > "$CERTIFICATE_PATH"
printf '%s' "$HYSTERIAX_NOTARY_KEY_P8_BASE64" | /usr/bin/base64 -D > "$NOTARY_KEY_PATH"

security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security set-keychain-settings -lut 21600 "$KEYCHAIN_PATH"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security import "$CERTIFICATE_PATH" -k "$KEYCHAIN_PATH" -P "$HYSTERIAX_DEVELOPER_ID_P12_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH" >/dev/null
security list-keychains -d user -s "$KEYCHAIN_PATH"
security find-identity -v -p codesigning "$KEYCHAIN_PATH" | grep 'Developer ID Application' >/dev/null

xcodebuild \
  -project "$REPO_ROOT/apps/macos/HysteriaX.xcodeproj" \
  -scheme HysteriaX \
  -configuration Release \
  -sdk macosx \
  -destination 'generic/platform=macOS' \
  -archivePath "$ARCHIVE_PATH" \
  -derivedDataPath "$TEMP_DIR/derived-data" \
  CODE_SIGN_STYLE=Manual \
  CODE_SIGN_IDENTITY='Developer ID Application' \
  DEVELOPMENT_TEAM="$HYSTERIAX_APPLE_TEAM_ID" \
  ENABLE_HARDENED_RUNTIME=YES \
  MARKETING_VERSION="$RELEASE_VERSION" \
  CURRENT_PROJECT_VERSION="${GITHUB_RUN_NUMBER:-1}" \
  archive

"$REPO_ROOT/scripts/verify-macos-app.sh" "$APP_PATH"

codesign --verify --deep --strict --verbose=2 "$APP_PATH"
mkdir -p "$DMG_STAGING_DIR"
ditto "$APP_PATH" "$DMG_STAGING_DIR/HysteriaX.app"
ln -s /Applications "$DMG_STAGING_DIR/Applications"
diskutil image create from \
  --volumeName "HysteriaX $RELEASE_VERSION" \
  --format UDZO \
  "$DMG_STAGING_DIR" \
  "$OUTPUT_DMG"
codesign \
  --sign 'Developer ID Application' \
  --timestamp \
  --identifier 'com.hysteriax.app.disk-image' \
  "$OUTPUT_DMG"
codesign --verify --verbose=2 "$OUTPUT_DMG"
xcrun notarytool submit "$OUTPUT_DMG" \
  --key "$NOTARY_KEY_PATH" \
  --key-id "$HYSTERIAX_NOTARY_KEY_ID" \
  --issuer "$HYSTERIAX_NOTARY_ISSUER_ID" \
  --wait
xcrun stapler staple "$OUTPUT_DMG"
xcrun stapler validate "$OUTPUT_DMG"
spctl --assess --type open --context context:primary-signature --verbose "$OUTPUT_DMG"
echo "Created notarized universal macOS disk image: $OUTPUT_DMG"
