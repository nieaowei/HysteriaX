# Sourced by node-install.sh after the node writer lock has been acquired.
if ! getent group hysteriax-acme >/dev/null 2>&1; then groupadd --system hysteriax-acme; fi
if ! getent passwd hysteriax-acme >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/hysteriax-acme --shell /usr/sbin/nologin --gid hysteriax-acme --groups hysteriax hysteriax-acme
fi
. "$STAGE/acme-layout.sh"
# Reject symlink traversal and foreign ownership before touching legacy storage.
p="$ACME_STORAGE"
while [ "$p" != / ]; do
  if [ -L "$p" ]; then echo 'ACME storage traverses a symlink; migration refused' >&2; exit 56; fi
  p=$(dirname "$p")
done
if [ -e "$ACME_STORAGE" ]; then
  if [ ! -f "$ROOT/.hysteriax-managed" ]; then echo 'existing ACME directory has no project ownership marker' >&2; exit 56; fi
  if find "$ACME_STORAGE" -type l -print -quit | read -r unused; then echo 'ACME storage contains symlinks' >&2; exit 56; fi
  if find "$ACME_STORAGE" ! -user hysteriax ! -user hysteriax-acme -print -quit | read -r unused; then echo 'ACME storage contains files with foreign ownership' >&2; exit 56; fi
else
  mkdir -p "$ACME_STORAGE"
fi
# Keep recovery metadata for migration back to the original built-in ACME.
if [ ! -f "$ROOT/acme-previous/acme.json" ] && [ -d "$ACME_STORAGE" ]; then
  cp "$STAGE/acme-layout.sh" "$ROOT/acme-previous/migration-layout.sh"
fi
chown -R hysteriax-acme:hysteriax-acme "$ACME_STORAGE"
chmod 0700 "$ACME_STORAGE"
mkdir -p "$ACME_STATE" "$ACME_PUBLISH"
chown hysteriax-acme:hysteriax-acme "$ACME_STATE"
chown hysteriax-acme:hysteriax "$ACME_PUBLISH"
chown hysteriax-acme:hysteriax-acme /var/lib/hysteriax-acme/node.lock
chmod 0700 "$ACME_STATE"
chmod 2750 "$ACME_PUBLISH"
if [ -f "$STAGE/acme-LICENSES.tar" ]; then
  mkdir -p "$ROOT/acme-licenses"
  tar -xf "$STAGE/acme-LICENSES.tar" -C "$ROOT/acme-licenses" --no-same-owner
fi
install -o root -g root -m 0755 "$STAGE/hysteriax-acme" "$ROOT/hysteriax-acme.new"
mv -f "$ROOT/hysteriax-acme.new" "$ROOT/hysteriax-acme"
install -o hysteriax-acme -g hysteriax-acme -m 0400 "$STAGE/acme.json" "$ETC/acme.json"
install -o root -g root -m 0644 "$STAGE/hysteriax-acme.service" /etc/systemd/system/hysteriax-acme.service
# ensure runs as the restricted certificate user, sharing the deployment lock.
setpriv --reuid=hysteriax-acme --regid=hysteriax-acme --init-groups --inh-caps=+net_bind_service --ambient-caps=+net_bind_service --bounding-set=-all,+net_bind_service --no-new-privs env HYSTERIAX_ACME_LOCK_FD=9 "$ROOT/hysteriax-acme" ensure "$ETC/acme.json"
