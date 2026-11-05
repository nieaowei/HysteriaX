#!/bin/sh
# Download a digest-pinned kernel, or copy it to a test directory for connection checks.
set -eu
if [ "$#" -ne 1 ] && ! { [ "$#" -eq 2 ] && [ "$1" = "--binary" ]; }; then
  echo 'Usage: verify-singbox-config.sh <config.json> | --binary <destination>' >&2
  exit 2
fi
version=1.14.2
case "$(uname -s):$(uname -m)" in
  Darwin:arm64) platform=darwin-arm64; expected=925c5382eca8492b0150f868a6db20b18290a38700e621724b3703fd453e032d ;;
  Darwin:x86_64) platform=darwin-amd64; expected=b0bfb0dc70a5fc708710b9f5ea98b9ee76d40fa4169928d25d73edc4331df2fe ;;
  Linux:x86_64|Linux:amd64) platform=linux-amd64; expected=a684484d7477d1437282ee411f4d131d0340aaad60a7868841ebd5d87dd8a0c6 ;;
  Linux:aarch64|Linux:arm64) platform=linux-arm64; expected=b43a1fb1bda131c6653576741ce527eb2bdeab7c9308ca90ee8b972abb7e4a7f ;;
  *) echo 'Unsupported sing-box validation platform' >&2; exit 2 ;;
esac
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT INT TERM
asset="sing-box-$version-$platform"
curl --retry 3 --connect-timeout 15 --max-time 180 -fsSL "https://github.com/SagerNet/sing-box/releases/download/v$version/$asset.tar.gz" -o "$tmp_dir/singbox.tar.gz"
if command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp_dir/singbox.tar.gz" | awk '{print $1}')
else
  actual=$(sha256sum "$tmp_dir/singbox.tar.gz" | awk '{print $1}')
fi
[ "$actual" = "$expected" ] || { echo 'Pinned sing-box asset digest mismatch' >&2; exit 1; }
tar -xzf "$tmp_dir/singbox.tar.gz" -C "$tmp_dir" "$asset/sing-box"
if [ "$1" = '--binary' ]; then
  cp "$tmp_dir/$asset/sing-box" "$2"
  chmod 0700 "$2"
else
  "$tmp_dir/$asset/sing-box" check -c "$1"
fi
