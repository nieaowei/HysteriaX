#!/usr/bin/env bash
set -euo pipefail

: "${HYSTERIAX_RELEASE_VERSION:?Set HYSTERIAX_RELEASE_VERSION to the release version without a leading v}"

RELEASE_VERSION="${HYSTERIAX_RELEASE_VERSION#v}"
if [[ ! "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][A-Za-z0-9.-]+)?$ ]]; then
  echo "HYSTERIAX_RELEASE_VERSION must be a version such as 1.2.3 or 1.2.3-rc.1" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP_DIR="$(mktemp -d)"
ARCHIVE_PATH="$TEMP_DIR/HysteriaX.xcarchive"
APP_PATH="$ARCHIVE_PATH/Products/Applications/HysteriaX.app"
DMG_STAGING_DIR="$TEMP_DIR/dmg-root"
VERIFY_MOUNT="$TEMP_DIR/dmg-mounted"
OUTPUT_DIR="${HYSTERIAX_RELEASE_OUTPUT_DIR:-$REPO_ROOT/dist}"
OUTPUT_DMG="$OUTPUT_DIR/HysteriaX-$RELEASE_VERSION-macOS-universal.dmg"

cleanup() {
  if mount | grep -F " on $VERIFY_MOUNT (" >/dev/null; then
    hdiutil detach "$VERIFY_MOUNT" -quiet || true
  fi
  rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

mkdir -p "$OUTPUT_DIR"

xcodebuild \
  -quiet \
  -project "$REPO_ROOT/apps/macos/HysteriaX.xcodeproj" \
  -scheme HysteriaX \
  -configuration Release \
  -sdk macosx \
  -destination 'generic/platform=macOS' \
  -archivePath "$ARCHIVE_PATH" \
  -derivedDataPath "$TEMP_DIR/derived-data" \
  CODE_SIGNING_ALLOWED=NO \
  CODE_SIGNING_REQUIRED=NO \
  CODE_SIGN_IDENTITY='' \
  MARKETING_VERSION="$RELEASE_VERSION" \
  CURRENT_PROJECT_VERSION="${GITHUB_RUN_NUMBER:-1}" \
  archive

"$REPO_ROOT/scripts/verify-macos-app.sh" "$APP_PATH"

mkdir -p "$DMG_STAGING_DIR"
ditto "$APP_PATH" "$DMG_STAGING_DIR/HysteriaX.app"
ln -s /Applications "$DMG_STAGING_DIR/Applications"
mkdir -p "$VERIFY_MOUNT"
rm -f "$OUTPUT_DMG"
diskutil image create from \
  --volumeName "HysteriaX $RELEASE_VERSION" \
  --format UDZO \
  "$DMG_STAGING_DIR" \
  "$OUTPUT_DMG"
DMG_SIGNATURE_INFO="$(codesign -dv --verbose=2 "$OUTPUT_DMG" 2>&1 || true)"
if [[ "$DMG_SIGNATURE_INFO" != *"code object is not signed at all"* ]]; then
  echo "Could not confirm the DMG is unsigned" >&2
  exit 1
fi
diskutil image attach --nobrowse --readOnly --mountPoint "$VERIFY_MOUNT" "$OUTPUT_DMG" >/dev/null
[[ -d "$VERIFY_MOUNT/HysteriaX.app" ]] || { echo "DMG is missing HysteriaX.app" >&2; exit 1; }
[[ -L "$VERIFY_MOUNT/Applications" ]] || { echo "DMG is missing the Applications link" >&2; exit 1; }
hdiutil detach "$VERIFY_MOUNT" -quiet
echo "Created unsigned universal macOS disk image: $OUTPUT_DMG"
