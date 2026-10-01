#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "Usage: scripts/verify-macos-app.sh /path/to/HysteriaX.app" >&2
  exit 2
fi

APP_PATH="$1"
BINARY_PATH="$APP_PATH/Contents/MacOS/HysteriaX"
INFO_PLIST="$APP_PATH/Contents/Info.plist"

[[ -d "$APP_PATH" ]] || { echo "App bundle not found: $APP_PATH" >&2; exit 1; }
[[ -f "$BINARY_PATH" ]] || { echo "App executable not found: $BINARY_PATH" >&2; exit 1; }
[[ -f "$INFO_PLIST" ]] || { echo "App Info.plist not found: $INFO_PLIST" >&2; exit 1; }

ARCHITECTURES="$(lipo -archs "$BINARY_PATH")"
for architecture in arm64 x86_64; do
  case " $ARCHITECTURES " in
    *" $architecture "*) ;;
    *) echo "Universal app is missing the $architecture architecture: $ARCHITECTURES" >&2; exit 1 ;;
  esac
done

MINIMUM_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$INFO_PLIST")"
if [[ "$MINIMUM_VERSION" != "26.0" ]]; then
  echo "Expected LSMinimumSystemVersion 26.0 for Intel/Apple silicon compatibility, found $MINIMUM_VERSION" >&2
  exit 1
fi

for architecture in arm64 x86_64; do
  ARCH_MINIMUM="$(otool -arch "$architecture" -l "$BINARY_PATH" | awk '
    $1 == "cmd" { command = $2 }
    command == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
    command == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
  ')"
  if [[ "$ARCH_MINIMUM" != "26.0" ]]; then
    echo "Expected $architecture minimum deployment version 26.0, found ${ARCH_MINIMUM:-unknown}" >&2
    exit 1
  fi
done

echo "Verified HysteriaX Universal app: arm64 + x86_64, macOS 26.0 minimum."
