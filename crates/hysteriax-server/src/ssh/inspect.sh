set -eu
[ -n "${HOME:-}" ] || exit 125
base="$HOME/.hysteriax-executions-v2"
resource="$base/$resource_hash"
op="$resource/ops/$op_id"
for directory in "$base" "$resource" "$resource/ops" "$op"; do
    [ -d "$directory" ] && [ ! -L "$directory" ] || exit 125
done
[ -f "$resource/lock" ] && [ ! -L "$resource/lock" ] || exit 125
exec 8<"$resource/lock"
flock -n 8 || exit 125
for file in "$resource/active" "$resource/fence" "$resource/head"; do
    [ -f "$file" ] && [ ! -L "$file" ] || exit 125
done
[ "$(cat "$resource/active")" = "$op_id" ] || exit 125
[ "$(head -c 38 "$resource/head")" = "$op_id" ] || exit 125
read -r active_epoch active_generation active_binding < "$resource/fence"
[ "$active_epoch" = "$epoch" ] && [ "$active_generation" = "$generation" ] && [ "$active_binding" = "$binding" ] || exit 125
[ -f "$op/result.json" ] && [ ! -L "$op/result.json" ] || exit 125
head -c 16385 "$op/result.json"
