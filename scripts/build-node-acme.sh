#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUTPUT=${1:-"$ROOT/dist/node-acme"}
mkdir -p "$OUTPUT"
OUTPUT=$(CDPATH= cd -- "$OUTPUT" && pwd)
cd "$ROOT/components/hysteriax-acme"
for arch in amd64 arm64; do
  CGO_ENABLED=0 GOOS=linux GOARCH=$arch go build -mod=readonly -trimpath -ldflags='-s -w -buildid=' -o "$OUTPUT/hysteriax-acme-linux-$arch" .
done
COPYFILE_DISABLE=1 tar --no-xattrs -cf "$OUTPUT/LICENSES.tar" -C "$ROOT/components/hysteriax-acme" LICENSE THIRD_PARTY_NOTICES.md third_party
cd "$OUTPUT"
shasum -a 256 hysteriax-acme-linux-amd64 hysteriax-acme-linux-arm64 LICENSES.tar > SHA256SUMS
