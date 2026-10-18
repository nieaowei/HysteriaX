set -eu
ROOT=/opt/hysteriax
ETC=/etc/hysteriax
systemctl stop hysteriax-acme.service 2>/dev/null || true
systemctl stop hysteriax.service 2>/dev/null || true
exec 9>/var/lib/hysteriax-acme/node.lock
flock -w 10 9
for spec in 'hysteriax-acme:binary' 'acme.json:config' 'hysteriax-acme.service:unit'; do
  file=${spec%:*}
  case ${spec#*:} in binary) dest="$ROOT/$file";; config) dest="$ETC/$file";; unit) dest="/etc/systemd/system/$file";; esac
  if [ -f "$ROOT/acme-previous/$file" ]; then cp -a "$ROOT/acme-previous/$file" "$dest"; else rm -f "$dest"; fi
done
if [ -f "$ROOT/config.previous" ]; then
  cp -a "$ROOT/config.previous" "$ETC/config.yaml"
  if [ -f "$ROOT/activation.previous" ]; then cp -a "$ROOT/activation.previous" "$ROOT/activation.json"; else rm -f "$ROOT/activation.json"; fi
  [ ! -f "$ROOT/hysteria.previous" ] || cp -a "$ROOT/hysteria.previous" "$ROOT/hysteria"
  [ ! -f "$ROOT/hysteriax.service.previous" ] || cp -a "$ROOT/hysteriax.service.previous" /etc/systemd/system/hysteriax.service
  if [ ! -f "$ETC/acme.json" ] && [ -f "$ROOT/acme-previous/migration-layout.sh" ]; then
    . "$ROOT/acme-previous/migration-layout.sh"
    chown -R hysteriax:hysteriax "$ACME_STORAGE"
  fi
  # A previous policy keeps its latest valid generation. Never force an expired leaf.
  if [ -f "$ETC/acme.json" ]; then setpriv --reuid=hysteriax-acme --regid=hysteriax-acme --init-groups --inh-caps=+net_bind_service --ambient-caps=+net_bind_service --bounding-set=-all,+net_bind_service --no-new-privs env HYSTERIAX_ACME_LOCK_FD=9 "$ROOT/hysteriax-acme" ensure "$ETC/acme.json"; fi
  systemctl daemon-reload
systemctl reset-failed hysteriax.service hysteriax-acme.service 2>/dev/null || true
  exec 9>&-
  if [ -f "$ETC/acme.json" ]; then systemctl enable --now hysteriax-acme.service; else systemctl disable hysteriax-acme.service 2>/dev/null || true; fi
  systemctl restart hysteriax.service
  systemctl is-active --quiet hysteriax.service
else
  systemctl disable hysteriax.service hysteriax-acme.service 2>/dev/null || true
  rm -f /etc/systemd/system/hysteriax.service /etc/systemd/system/hysteriax-acme.service
  systemctl daemon-reload
systemctl reset-failed hysteriax.service hysteriax-acme.service 2>/dev/null || true
  rm -rf "$ROOT" "$ETC" /var/lib/hysteriax /var/lib/hysteriax-acme
  userdel hysteriax-acme 2>/dev/null || true
  groupdel hysteriax-acme 2>/dev/null || true
  userdel hysteriax 2>/dev/null || true
  groupdel hysteriax 2>/dev/null || true
fi
