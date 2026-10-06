#!/usr/bin/env bash
# Screenshot regression check. `baseline` captures every scene at every
# size into target/shots/baseline; `check` captures them again into
# target/shots/current and compares each against its baseline. A scene
# whose pixels differ by more than the allowance fails the check, and a
# diff image lands beside it.
#
#   scripts/shots.sh baseline
#   scripts/shots.sh check
#   ONLY=settings scripts/shots.sh check   # one scene
#   DESKTOP=1 scripts/shots.sh check      # on the desktop, no Xvfb
#
# Scenes are a name and the screenshot script that reaches them.
set -u

APP=${APP:-kitch}
ZITCH=${ZITCH:-./target/debug/zitch}
SIZES=${SIZES:-"640x480 1280x720"}
# Pixels allowed to differ, as a fraction of the image, before a scene fails.
ALLOW=${ALLOW:-0.002}
OUT=target/shots
# Each capture opens a window. Under Xvfb it opens on a display of its
# own, at a fixed size, with no mouse or keyboard reaching it, and
# nothing shows on the desktop. DESKTOP=1 runs on the desktop instead.
HEADLESS=()
if [ -z "${DESKTOP:-}" ] && command -v xvfb-run >/dev/null; then
    HEADLESS=(xvfb-run -a -s "-screen 0 1920x1080x24")
fi

SCENES=(
    "library|focus:0,wait:300"
    "collections|nexttab,wait:1500"
    "downloads|nexttab,nexttab,wait:500"
    "game|focus:0,enter,wait:1500"
    "game-shots|focus:0,enter,down,wait:1000"
    "drawer|guide"
    "prompt|prompt"
    "report|report"
    "settings|settings"
    "types|types"
    "qr|focus:0,enter,qr"
    "rating|y,right,enter"
)

mode=${1:-check}
case $mode in
    baseline) dir=$OUT/baseline ;;
    check) dir=$OUT/current ;;
    *) echo "usage: $0 baseline|check" >&2; exit 2 ;;
esac
mkdir -p "$dir"

capture() {
    local name=$1 script=$2 size=$3
    local file=$dir/$name-$size.png
    rm -f "$file"
    timeout 120 "${HEADLESS[@]}" "$ZITCH" --app-name "$APP" --emulate "$size" \
        --screenshot "$file" --screenshot-script "$script" >/dev/null 2>&1
    if [ ! -f "$file" ]; then
        echo "FAIL  $name $size: no screenshot written"
        return 1
    fi
}

failed=0
for entry in "${SCENES[@]}"; do
    name=${entry%%|*}
    script=${entry#*|}
    if [ -n "${ONLY:-}" ] && [ "$name" != "$ONLY" ]; then
        continue
    fi
    for size in $SIZES; do
        capture "$name" "$script" "$size" || { failed=1; continue; }
        if [ "$mode" = check ]; then
            base=$OUT/baseline/$name-$size.png
            if [ ! -f "$base" ]; then
                echo "NEW   $name $size: no baseline"
                continue
            fi
            diff=$OUT/current/$name-$size.diff.png
            # AE counts differing pixels; fuzz ignores tiny color shifts.
            differing=$(compare -metric AE -fuzz 2% "$base" "$dir/$name-$size.png" "$diff" 2>&1 | awk '{print $1}')
            total=$(magick identify -format '%[fx:w*h]' "$base")
            fraction=$(awk -v d="$differing" -v t="$total" 'BEGIN { printf "%.4f", d / t }')
            if awk -v f="$fraction" -v a="$ALLOW" 'BEGIN { exit !(f > a) }'; then
                echo "FAIL  $name $size: $differing pixels differ ($fraction), see $diff"
                failed=1
            else
                echo "ok    $name $size ($differing pixels)"
                rm -f "$diff"
            fi
        else
            echo "saved $name $size"
        fi
    done
done
exit $failed
