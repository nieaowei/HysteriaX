set -eu
umask 077
[ -n "${HOME:-}" ] || exit 125
base="$HOME/.hysteriax-executions-v2"
resource="$base/$resource_hash"
legacy="$HOME/.hysteriax-executions-v1/$resource_hash"
[ ! -e "$legacy" ] && [ ! -L "$legacy" ] || exit 125
original="$resource/ops/$op_id"
for directory in "$base" "$resource" "$resource/ops" "$resource/retired"; do
    [ -d "$directory" ] && [ ! -L "$directory" ] || exit 125
done
[ -f "$resource/lock" ] && [ ! -L "$resource/lock" ] || exit 125
exec 8<"$resource/lock"
flock -n 8 || exit 125
checks="$resource/rechecks"
[ ! -L "$checks" ] || exit 125
if [ ! -e "$checks" ]; then mkdir -m 700 "$checks"; fi
[ -d "$checks" ] || exit 125
check="$checks/$check_id"
[ ! -L "$check" ] || exit 125
if [ ! -e "$check" ]; then mkdir -m 700 "$check"; fi
[ -d "$check" ] || exit 125
# A response-lost verification is idempotent by its exact immutable nonce.
if [ -e "$check/result.json" ] || [ -L "$check/result.json" ]; then
    [ -f "$check/result.json" ] && [ ! -L "$check/result.json" ] || exit 125
    head -c 16385 "$check/result.json"; exit 0
fi
if [ -e "$check/prepared.json" ] || [ -L "$check/prepared.json" ]; then
    [ -f "$check/prepared.json" ] && [ ! -L "$check/prepared.json" ] || exit 125
    [ -f "$check/request.sha256" ] && [ ! -L "$check/request.sha256" ] || exit 125
    [ "$(cat "$check/request.sha256")" = "$request_hash" ] || exit 125
else
    [ -d "$original" ] && [ ! -L "$original" ] || exit 125
    for file in "$resource/head" "$resource/active" "$resource/fence" "$original/result.json"; do
        [ -f "$file" ] && [ ! -L "$file" ] || exit 125
    done
    [ "$(head -c 38 "$resource/head")" = "$op_id" ] || exit 125
    [ "$(cat "$resource/active")" = "$op_id" ] || exit 125
    read -r observed_epoch observed_generation observed_binding < "$resource/fence"
    [ "$observed_epoch" = "$epoch" ] && [ "$observed_generation" = "$generation" ] && [ "$observed_binding" = "$binding" ] || exit 125
    observed_hash=$(sha256sum "$original/result.json"); observed_hash=${observed_hash%% *}
    [ "$observed_hash" = "$receipt_hash" ] || exit 125
    printf '%s\n' "$request_hash" > "$check/request.sha256"
    printf '{"protocol":2,"request":%s,"completion":%s,"state":"fenced"}\n' "$request" "$(cat "$original/result.json")" > "$check/prepared.new"
    mv -f -- "$check/prepared.new" "$check/prepared.json"
fi
for file in "$resource/head" "$resource/active" "$resource/fence"; do
    [ -f "$file" ] && [ ! -L "$file" ] || exit 125
done
[ "$(cat "$resource/active")" = "$op_id" ] || exit 125
observed_head=$(head -c 38 "$resource/head")
read -r observed_epoch observed_generation observed_binding < "$resource/fence"
if [ "$observed_head" = "$op_id" ]; then
    if [ "$observed_epoch" = "$epoch" ] && [ "$observed_generation" = "$generation" ] && [ "$observed_binding" = "$binding" ]; then
        [ ! -e "$resource/retired/$target_epoch" ] && [ ! -L "$resource/retired/$target_epoch" ] || exit 125
        if [ "$target_epoch" != "$epoch" ]; then : > "$resource/retired/$epoch"; fi
        printf '%s %s %s\n' "$target_epoch" "$target_generation" "$target_binding" > "$check/fence.new"
        mv -f -- "$check/fence.new" "$resource/fence"
    else
        [ "$observed_epoch" = "$target_epoch" ] && [ "$observed_generation" = "$target_generation" ] && [ "$observed_binding" = "$target_binding" ] || exit 125
    fi
    printf '%s\n' "$check_id" > "$check/head.new"
    mv -f -- "$check/head.new" "$resource/head"
else
    [ "$observed_head" = "$check_id" ] && [ "$observed_epoch" = "$target_epoch" ] && [ "$observed_generation" = "$target_generation" ] && [ "$observed_binding" = "$target_binding" ] || exit 125
fi
cp -- "$check/prepared.json" "$check/result.new"
mv -f -- "$check/result.new" "$check/result.json"
cat "$check/result.json"
