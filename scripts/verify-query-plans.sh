#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${TEST_DATABASE_URL:?Set TEST_DATABASE_URL to a disposable PostgreSQL database}"
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"
cd "$ROOT"

# Run only these database performance checks; other ignored tests need SSH or snapshots.
for test in \
  api::query_optimizer_tests::selective_history_search_and_status_paging_use_indexes \
  analytics::followup_tests::aligned_online_history_skips_cycles_until_an_edge_or_dirty_minute_needs_them \
  analytics::followup_tests::rollup_refresh_reads_only_dirty_ranges_and_skips_empty_queues; do
  cargo test --locked -p hysteriax-server "$test" -- --ignored --exact --test-threads=1
done
