set -eu
# HysteriaX predecessor v2
[ -n "${HOME:-}" ] || exit 125
base="$HOME/.hysteriax-executions-v2"
resource="$base/$resource_hash"
legacy="$HOME/.hysteriax-executions-v1/$resource_hash"
[ ! -e "$legacy" ] && [ ! -L "$legacy" ] || exit 125
for directory in "$base" "$resource"; do
    [ ! -L "$directory" ] || exit 125
    if [ ! -e "$directory" ]; then printf 'empty\n'; exit 0; fi
    [ -d "$directory" ] || exit 125
done
[ ! -L "$resource/lock" ] || exit 125
if [ ! -e "$resource/lock" ]; then
    [ ! -e "$resource/head" ] && [ ! -e "$resource/active" ] && [ ! -e "$resource/fence" ] || exit 125
    printf 'empty\n'; exit 0
fi
[ -f "$resource/lock" ] || exit 125
exec 8<"$resource/lock"
flock -n 8 || exit 125
[ ! -L "$resource/head" ] || exit 125
if [ ! -e "$resource/head" ]; then
    [ ! -e "$resource/active" ] && [ ! -e "$resource/fence" ] || exit 125
    printf 'empty\n'; exit 0
fi
[ -f "$resource/head" ] || exit 125
head -c 38 "$resource/head"
