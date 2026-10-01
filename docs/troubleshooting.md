# Troubleshooting

## The API does not become ready

Run `docker compose logs api`. Check that `HYSTERIAX_MASTER_KEY` decodes to 32 bytes and that the data directory is writable by the service container. On first startup, provide a 256-bit administrator token in `HYSTERIAX_ADMIN_TOKEN`.

## HTTPS certificate issuance fails

Verify public DNS and allow inbound TCP 80 and 443 to Caddy. Check `docker compose logs caddy` for ACME or DNS errors.

## A token no longer works

Administrator, node, assignment, and subscription tokens are independent. A revoked administrator token cannot call management APIs; a revoked subscription token returns 404; a disabled, expired, or over quota user receives a denied Hysteria auth result and a 403 subscription response. Rotate only the credential type that needs replacement.

## A configuration write returns 409

Another client saved a newer revision. Fetch the node or user again, merge the intended edit, and submit its current `revision` as `expected_revision`.

## A node does not appear in a subscription

The subscription includes only assignments on nodes with a successful deployed revision. The current API foundation persists deploy and sync jobs; until the SSH worker stage is complete, queued jobs do not install Hysteria on remote hosts.
