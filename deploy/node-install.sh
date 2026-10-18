set -eu
ROOT=/opt/hysteriax
ETC=/etc/hysteriax
# STAGE is supplied by the management service, never by public configuration.
if [ -e /var/lib/hysteriax-acme ] && [ ! -f /var/lib/hysteriax-acme/.hysteriax-managed ]; then echo 'unmanaged certificate state directory conflict' >&2; exit 50; fi
if [ -e /etc/systemd/system/hysteriax-acme.service ] && [ ! -f "$ROOT/.hysteriax-managed" ]; then echo 'unmanaged certificate service conflict' >&2; exit 50; fi
if getent passwd hysteriax-acme >/dev/null 2>&1 && [ ! -f "$ROOT/.hysteriax-managed" ]; then echo 'unmanaged certificate account conflict' >&2; exit 50; fi
if [ -f "$STAGE/acme.json" ]; then
  command -v setpriv >/dev/null
  command -v flock >/dev/null
  # /tmp may be mounted noexec. Validate from a private executable filesystem.
  CHECK_DIR=$(mktemp -d /opt/.hysteriax-acme-check.XXXXXX)
  install -o root -g root -m 0755 "$STAGE/hysteriax-acme" "$CHECK_DIR/hysteriax-acme"
  if "$CHECK_DIR/hysteriax-acme" check "$STAGE/acme.json"; then rm -rf "$CHECK_DIR"; else rm -rf "$CHECK_DIR"; exit 57; fi
fi
if ! getent group hysteriax >/dev/null 2>&1; then groupadd --system hysteriax; fi
if ! getent passwd hysteriax >/dev/null 2>&1; then useradd --system --home-dir /var/lib/hysteriax --shell /usr/sbin/nologin --gid hysteriax hysteriax; fi
mkdir -p "$ROOT" "$ETC" "$ETC/resources" /var/lib/hysteriax /var/lib/hysteriax-acme
chown root:hysteriax "$ETC/resources"
chmod 0750 "$ETC/resources"
# Hy2 cannot rename the certificate account's directory or publication root.
chown root:root /var/lib/hysteriax
chmod 0755 /var/lib/hysteriax
touch /var/lib/hysteriax-acme/.hysteriax-managed
chmod 0600 /var/lib/hysteriax-acme/.hysteriax-managed
chown root:root /var/lib/hysteriax-acme
chmod 0711 /var/lib/hysteriax-acme
mkdir -p /var/lib/hysteriax/certificates
chown root:hysteriax /var/lib/hysteriax/certificates
chmod 0750 /var/lib/hysteriax/certificates
for resource in "$STAGE"/resources/*; do
  [ -f "$resource" ] || continue
  target="$ETC/resources/$(basename "$resource")"
  if [ -e "$target" ]; then
    if ! cmp -s "$resource" "$target"; then echo 'managed resource id conflicts with existing contents' >&2; exit 49; fi
  else
    install -o root -g hysteriax -m 0640 "$resource" "$target"
  fi
done
# Persist the recovery set before stopping either service.
rm -rf "$ROOT/acme-previous"
mkdir -m 0700 "$ROOT/acme-previous"
[ ! -f "$ROOT/hysteriax-acme" ] || cp -a "$ROOT/hysteriax-acme" "$ROOT/acme-previous/hysteriax-acme"
[ ! -f "$ETC/acme.json" ] || cp -a "$ETC/acme.json" "$ROOT/acme-previous/acme.json"
[ ! -f /etc/systemd/system/hysteriax-acme.service ] || cp -a /etc/systemd/system/hysteriax-acme.service "$ROOT/acme-previous/hysteriax-acme.service"
if [ -f "$ROOT/hysteria" ]; then cp -a "$ROOT/hysteria" "$ROOT/hysteria.previous"; fi
if [ -f "$ETC/config.yaml" ]; then cp -a "$ETC/config.yaml" "$ROOT/config.previous"; fi
if [ -f "$ROOT/activation.json" ]; then cp -a "$ROOT/activation.json" "$ROOT/activation.previous"; else rm -f "$ROOT/activation.previous"; fi
if [ -f /etc/systemd/system/hysteriax.service ]; then cp -a /etc/systemd/system/hysteriax.service "$ROOT/hysteriax.service.previous"; fi
install -m 0700 "$STAGE/rollback.sh" "$ROOT/rollback.sh"
trap 'code=$?; trap - EXIT; if [ "$code" -ne 0 ]; then exec 9>&-; if sh "$ROOT/rollback.sh"; then echo "installation failed; previous service restored" >&2; else echo "installation and rollback failed; explicit recovery required" >&2; fi; fi; exit "$code"' EXIT
trap 'exit 143' HUP INT TERM
systemctl stop hysteriax-acme.service 2>/dev/null || true
systemctl stop hysteriax.service 2>/dev/null || true
if [ ! -f /var/lib/hysteriax-acme/node.lock ]; then install -o root -g root -m 0600 /dev/null /var/lib/hysteriax-acme/node.lock; fi
exec 9<>/var/lib/hysteriax-acme/node.lock
flock -w 10 9
if [ -f "$STAGE/acme.json" ]; then
  . "$STAGE/acme-prepare.sh"
else
  rm -f "$ETC/acme.json" /etc/systemd/system/hysteriax-acme.service
  systemctl disable hysteriax-acme.service 2>/dev/null || true
fi
install -o root -g root -m 0755 "$STAGE/hysteria" "$ROOT/hysteria.new"
mv -f "$ROOT/hysteria.new" "$ROOT/hysteria"
install -o root -g hysteriax -m 0640 "$STAGE/server.yaml" "$ETC/config.yaml.new"
mv -f "$ETC/config.yaml.new" "$ETC/config.yaml"
if [ -f "$STAGE/activation.json" ]; then install -o root -g root -m 0600 "$STAGE/activation.json" "$ROOT/activation.json"; fi
install -o root -g root -m 0644 "$STAGE/hysteriax.service" /etc/systemd/system/hysteriax.service
systemctl daemon-reload
systemctl reset-failed hysteriax.service hysteriax-acme.service 2>/dev/null || true
systemctl enable hysteriax.service >/dev/null
systemctl restart hysteriax.service
systemctl is-active --quiet hysteriax.service
exec 9>&-
if [ -f "$ETC/acme.json" ]; then systemctl enable --now hysteriax-acme.service; fi
touch "$ROOT/.hysteriax-managed"
chown root:root "$ROOT/.hysteriax-managed"
chmod 0600 "$ROOT/.hysteriax-managed"
trap - EXIT
rm -rf "$STAGE"
