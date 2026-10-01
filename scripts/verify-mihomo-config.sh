#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "Usage: scripts/verify-mihomo-config.sh <config.yaml>" >&2
  exit 2
fi
config_path=$1
if [ ! -f "$config_path" ]; then
  echo "Config file not found: $config_path" >&2
  exit 2
fi

version=v1.19.31
platform=$(uname -s)
architecture=$(uname -m)
case "$platform:$architecture" in
  Darwin:arm64)
    asset=mihomo-darwin-arm64-v1.19.31.gz
    expected=d131f44b3deb2a8356f7ac75048ad67a10d53243323951c4f3cda7b672922963
    ;;
  Darwin:x86_64)
    asset=mihomo-darwin-amd64-v1.19.31.gz
    expected=3546681ebef3415e5dcbe7210a61aa80748136e95e6552768fd883df345508ed
    ;;
  Linux:x86_64|Linux:amd64)
    asset=mihomo-linux-amd64-v1.19.31.gz
    expected=d5e74bbddbdfff49a1aef7775bf5911da59f0d7196ed509a0ac914b3653dd5f1
    ;;
  Linux:aarch64|Linux:arm64)
    asset=mihomo-linux-arm64-v1.19.31.gz
    expected=9e0f11afbf38426b8bd88fdc594678f8161c57eccb4e1b77acb12b493904f1d4
    ;;
  *)
    echo "Unsupported Mihomo validation platform: $platform $architecture" >&2
    exit 2
    ;;
esac

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT INT TERM
asset_url="https://github.com/MetaCubeX/mihomo/releases/download/$version/$asset"
curl -fsSL "$asset_url" -o "$tmp_dir/mihomo.gz"
if command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp_dir/mihomo.gz" | awk '{print $1}')
else
  actual=$(sha256sum "$tmp_dir/mihomo.gz" | awk '{print $1}')
fi
if [ "$actual" != "$expected" ]; then
  echo "Pinned Mihomo $version asset digest mismatch" >&2
  exit 1
fi
gzip -dc "$tmp_dir/mihomo.gz" > "$tmp_dir/mihomo"
chmod 0700 "$tmp_dir/mihomo"
"$tmp_dir/mihomo" -t -f "$config_path"
