# A resumed mTLS activation can already have the target Hy2 configuration.
# Recheck certificate validity and restart maintenance without replacing the
# previous recovery set or publishing subscriptions early.
set -eu
if ! cmp -s "$STAGE/acme.json" /etc/hysteriax/acme.json; then
  echo 'certificate component configuration differs from the activation target; inspect and recover the node' >&2
  exit 58
fi
systemctl stop hysteriax-acme.service 2>/dev/null || true
exec 9<>/var/lib/hysteriax-acme/node.lock
flock -w 10 9
setpriv --reuid=hysteriax-acme --regid=hysteriax-acme --init-groups --inh-caps=+net_bind_service --ambient-caps=+net_bind_service --bounding-set=-all,+net_bind_service --no-new-privs env HYSTERIAX_ACME_LOCK_FD=9 /opt/hysteriax/hysteriax-acme ensure /etc/hysteriax/acme.json
exec 9>&-
systemctl reset-failed hysteriax-acme.service 2>/dev/null || true
systemctl enable --now hysteriax-acme.service
