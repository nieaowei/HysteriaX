set -eu
LC_ALL=C; export LC_ALL
# Receipt privacy must not change the installation command's creation modes.
payload_umask=$(umask)
umask 077
[ -n "${HOME:-}" ] && [ -d "$HOME" ] || exit 125
base="$HOME/.hysteriax-executions-v2"
legacy="$HOME/.hysteriax-executions-v1/$resource_hash"
[ ! -e "$legacy" ] && [ ! -L "$legacy" ] || exit 125
private_dir() {
    [ ! -L "$1" ] || exit 125
    if [ ! -e "$1" ]; then mkdir -m 700 "$1"; fi
    [ -d "$1" ] || exit 125
    chmod 700 "$1"
}
private_dir "$base"
resource="$base/$resource_hash"
private_dir "$resource"
private_dir "$resource/ops"
private_dir "$resource/retired"
[ ! -L "$resource/lock" ] || exit 125
[ ! -e "$resource/lock" ] || [ -f "$resource/lock" ] || exit 125
exec 8>>"$resource/lock"
flock -n 8 || exit 125
current_head=empty
if [ -e "$resource/head" ] || [ -L "$resource/head" ]; then
    [ -f "$resource/head" ] && [ ! -L "$resource/head" ] || exit 125
    current_head=$(head -c 38 "$resource/head")
else
    # A partial or legacy resource cannot masquerade as a new empty resource.
    [ ! -e "$resource/active" ] && [ ! -e "$resource/fence" ] || exit 125
fi
[ "$current_head" = "$expected_head" ] || exit 125
op="$resource/ops/$op_id"
[ ! -e "$op" ] && [ ! -L "$op" ] || exit 125
previous=''
if [ -e "$resource/active" ] || [ -L "$resource/active" ]; then
    [ -f "$resource/active" ] && [ ! -L "$resource/active" ] || exit 125
    previous=$(cat "$resource/active")
    case "$previous" in ''|*[!0-9a-f-]*) exit 125;; esac
    [ ${#previous} -eq 36 ] || exit 125
    printf '%s\n' "$previous" | grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' || exit 125
    [ ! -L "$resource/ops/$previous" ] && [ -d "$resource/ops/$previous" ] || exit 125
    [ -f "$resource/ops/$previous/result.json" ] && [ ! -L "$resource/ops/$previous/result.json" ] || exit 125
fi
[ ! -e "$resource/retired/$epoch" ] && [ ! -L "$resource/retired/$epoch" ] || exit 125
old_epoch=''
if [ -e "$resource/fence" ] || [ -L "$resource/fence" ]; then
    [ -f "$resource/fence" ] && [ ! -L "$resource/fence" ] || exit 125
    read -r old_epoch old_generation old_binding < "$resource/fence"
    case "$old_epoch" in ''|*[!0-9a-f-]*) exit 125;; esac
    [ ${#old_epoch} -eq 36 ] || exit 125
    printf '%s\n' "$old_epoch" | grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' || exit 125
    case "$old_generation" in ''|*[!0-9]*) exit 125;; esac
    [ "$old_generation" -gt 0 ] || exit 125
    case "$old_binding" in ''|*[!0-9a-f]*) exit 125;; esac
    [ ${#old_binding} -eq 64 ] || exit 125
    if [ "$epoch" = "$old_epoch" ]; then
        [ "$generation" -ge "$old_generation" ] || exit 125
        if [ "$generation" -eq "$old_generation" ]; then
            [ "$binding" = "$old_binding" ] || exit 125
        fi
    fi
fi
mkdir -m 700 "$op"
# No other application request can legitimately follow an unknown step. Keep
# incomplete active directories forever; retire only a completed predecessor.
if [ -n "$previous" ]; then rm -rf -- "$resource/ops/$previous"; fi
if [ -n "$old_epoch" ] && [ "$old_epoch" != "$epoch" ]; then
    : > "$resource/retired/$old_epoch"
fi
printf '%s %s %s\n' "$epoch" "$generation" "$binding" > "$op/fence.new"
mv -f -- "$op/fence.new" "$resource/fence"
printf '%s\n' "$intent" > "$op/intent.json"
printf '%s\n' "$op_id" > "$op/active.new"
mv -f -- "$op/active.new" "$resource/active"
printf '%s\n' "$op_id" > "$op/head.new"
mv -f -- "$op/head.new" "$resource/head"
cleanup() {
    exec 6>&- 7>&-
    rm -f -- "$op/stdout.fifo" "$op/stderr.fifo" "$op/stdout" "$op/stderr" "$op/result.new"
}
trap cleanup EXIT
if [ "$payload_lock" -eq 1 ]; then
    # The upload has a remote deadline. Keep the journal alive on disconnect
    # so a short read or timed-out receiver can still leave a completion receipt.
    trap '' HUP INT TERM
else
    trap 'exit 125' HUP INT TERM
fi
mkfifo -m 600 "$op/stdout.fifo" "$op/stderr.fifo"
# Parent-held read/write ends keep reader opens bounded even on early failure.
exec 6<>"$op/stdout.fifo" 7<>"$op/stderr.fifo"
(head -c "$((cap+1))" < "$op/stdout.fifo" > "$op/stdout" 6>&- 7>&- 8>&-) &
out_pid=$!
(head -c "$((cap+1))" < "$op/stderr.fifo" > "$op/stderr" 6>&- 7>&- 8>&-) &
err_pid=$!
set +e
# Uploads inherit the resource lock until their bounded stdin consumer exits.
# A disconnected sender must never leave a writer running after a recheck.
(umask "$payload_umask"; if [ "$payload_lock" -eq 0 ]; then exec 8>&-; fi
 sh -c "$command" 6>&- 7>&-) > "$op/stdout.fifo" 2> "$op/stderr.fifo"
code=$?
set -e
exec 6>&- 7>&-
wait "$out_pid"
wait "$err_pid"
out_bytes=$(wc -c < "$op/stdout")
err_bytes=$(wc -c < "$op/stderr")
if [ "$((out_bytes+err_bytes))" -gt "$cap" ]; then
    cat "$op/stdout"
    cat "$op/stderr" >&2
    exit 125
fi
out_digest=$(sha256sum "$op/stdout"); out_digest=${out_digest%% *}
err_digest=$(sha256sum "$op/stderr"); err_digest=${err_digest%% *}
printf '{"protocol":2,"context":%s,"state":"completed","exit_code":%s,"stdout_bytes":%s,"stderr_bytes":%s,"stdout_sha256":"%s","stderr_sha256":"%s"}\n' "$intent" "$code" "$out_bytes" "$err_bytes" "$out_digest" "$err_digest" > "$op/result.new"
mv -f -- "$op/result.new" "$op/result.json"
cat "$op/stdout"
cat "$op/stderr" >&2
exit "$code"
