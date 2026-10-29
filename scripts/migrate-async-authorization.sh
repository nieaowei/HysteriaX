#!/usr/bin/env bash
# Upgrade a stopped test environment, including the user-level password backfill.
set -euo pipefail
: "${TEST_DATABASE_URL:?Set TEST_DATABASE_URL to the test database to upgrade}"
: "${HYSTERIAX_MASTER_KEY:?Use the existing master key for this test environment}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
DATABASE_URL="$TEST_DATABASE_URL" cargo run -p hysteriax-server -- --migrate-only
