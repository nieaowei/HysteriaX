#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-run}"
APP_NAME="HysteriaX"
BUNDLE_ID="com.hysteriax.app"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT="$ROOT_DIR/apps/macos/HysteriaX.xcodeproj"
DERIVED_DATA="$ROOT_DIR/apps/macos/.build/xcode-run"
APP_BUNDLE="$DERIVED_DATA/Build/Products/Debug/$APP_NAME.app"
APP_BINARY="$APP_BUNDLE/Contents/MacOS/$APP_NAME"

project_app_pids() {
    /bin/ps -ww -axo pid=,command= |
      /usr/bin/awk -v binary="$APP_BINARY" '
        {
          pid = $1
          sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "", $0)
          if ($0 == binary || index($0, binary " ") == 1) print pid
        }'
}

stop_project_app() {
  while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    kill "$pid" >/dev/null 2>&1 || true
  done < <(project_app_pids)
}

stop_project_app
xcodebuild \
  -project "$PROJECT" \
  -scheme "$APP_NAME" \
  -configuration Debug \
  -sdk macosx \
  -destination "platform=macOS" \
  -derivedDataPath "$DERIVED_DATA" \
  build CODE_SIGNING_ALLOWED=NO >/dev/null

open_app() { /usr/bin/open -n "$APP_BUNDLE"; }

case "$MODE" in
  run)
    open_app
    ;;
  --debug|debug)
    lldb -- "$APP_BINARY"
    ;;
  --logs|logs)
    open_app
    /usr/bin/log stream --info --style compact --predicate "process == \"$APP_NAME\""
    ;;
  --telemetry|telemetry)
    open_app
    /usr/bin/log stream --info --style compact --predicate "subsystem == \"$BUNDLE_ID\""
    ;;
  --verify|verify)
    open_app
    for _ in {1..20}; do
      if [[ -n "$(project_app_pids)" ]]; then
        exit 0
      fi
      sleep 0.25
    done
    echo "Project-built $APP_NAME did not launch: $APP_BINARY" >&2
    exit 1
    ;;
  *)
    echo "usage: $0 [run|--debug|--logs|--telemetry|--verify]" >&2
    exit 2
    ;;
esac
