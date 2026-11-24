# Troubleshooting

## The API does not become ready

Run `docker compose logs --tail=100 api`. Check database connectivity, that `HYSTERIAX_MASTER_KEY` decodes to 32 bytes, and that the asset-cache directory is writable. Missing schema or initialization requires stopping all API replicas and running the selected image's `--migrate-only` before restart. First maintenance initialization needs a 256-bit administrator token in `HYSTERIAX_ADMIN_TOKEN`. Do not regenerate the master key for an existing database. A database from the former 0001–0074 chain is incompatible with the current baseline; use its matching old image or initialize a separate empty database. Do not alter migration records to bypass checksum checks.

## The API route is missing or the event stream returns 410

For HTTP 404, check the configured service address and request path. The service exposes management API `/api/v2`; use the matching client and stable idempotency keys for writes. HTTP 410 on SSE means the saved cursor is no longer valid; fetch the new stream head and reload state before resuming. See [installation](installation.md#database-baseline).

## HTTPS certificate issuance fails

Verify public DNS and allow inbound TCP 80 and 443 to Caddy. Check `docker compose logs caddy` for ACME or DNS errors.

## A token no longer works

Check which token failed. A revoked administrator token is rejected by management APIs. A revoked subscription URL returns 404. A disabled, expired or over-quota user receives a denied proxy authentication result and a 403 subscription response. User connection passwords and subscription tokens rotate together. After rotation, copy the new address and update the client's saved configuration. See [credentials](credentials.md#user-connection-and-subscription-credentials).

## A configuration write returns 409

A stale revision requires fetching the entity again, merging the intended edit, and submitting the current `revision` as `expected_revision`. Read the error code: an idempotency key reused for different content or conflicting resource work can also return 409. After a transport failure, retry the same intended request with its original key; do not mint a new key to force an uncertain operation through. Transient transaction retries exhausted with 503 can be retried with the same key.

For an authorization conflict, use the link in the Mac client to open the pending task. Wait for it to finish or review its failure and use the linked retry action. See [authorization task recovery](authorization-groups.md#处理挂起的授权任务).

## A subscription opens a web page in the proxy client

Open the canonical address in a browser, select the destination platform, and copy the address from the desired template card. For a client that imports node links, copy the URL from the Base64 or URI card. See [subscription addresses](subscriptions.md#addresses-and-qualification).

## A node does not appear in a subscription

Check the user's enabled state, expiry, quota and authorization-group membership, then confirm the node has a successful deployed revision. Native template exports also apply the selected platform's compatibility and injection rules. Inspect queued, failed or uncertain deployment jobs and any mTLS configuration barrier. The SSH workers execute deployment independently of the Mac client. See [subscriptions](subscriptions.md) and [managed mTLS](managed-mtls.md).

## Older history is missing or database storage keeps growing

Check the retention window for the affected table and inspect API logs for `sampling retention claim finished` and `history retention page committed`. Review skipped rows for active references or unsettled work. Check recent autovacuum and table and index sizes in PostgreSQL. Current quota counters use business state independently of the retained detail rows. For investigations that need older rows, restore a matching backup in isolation. See [retention windows](installation.md#database-history-retention) and [recovery](backup-restore.md).

## macOS client times out while curl succeeds

Check Caddy's `Alt-Svc` response header and the client's CFNetwork logs. Advertising HTTP/3 while UDP 443 is unavailable to the management listener can make URLSession attempt a QUIC connection that times out. The bundled Caddyfile uses HTTP/1.1 and HTTP/2 and returns `Alt-Svc: clear` to remove old alternatives. The Mac client uses a fresh ephemeral session without HTTP response caching. After updating Caddy's configuration, validate and reload it with `docker compose exec -T caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile`, then restart the updated client. TLS certificate validation remains enabled.
