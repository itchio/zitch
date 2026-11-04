#!/bin/sh
# zitch as a spruceOS app. The menu runs this from the app folder with no
# arguments and comes back when it exits.

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$APP_DIR" || exit

# The menu discards our output, so the log starts here.
exec >"$APP_DIR/zitch.log" 2>&1

. /mnt/SDCARD/spruce/scripts/appEnv.sh

# Config (butler database, installed games) and the cover cache live next
# to the binary on the SD card.
HOME="$APP_DIR/home"
XDG_CONFIG_HOME="$HOME/.config"
XDG_CACHE_HOME="$HOME/.cache"
export HOME XDG_CONFIG_HOME XDG_CACHE_HOME
mkdir -p "$HOME"

# Extra flags for development, e.g. a screenshot script; one line, no quotes.
ARGS=""
[ -f "$APP_DIR/args" ] && ARGS=$(cat "$APP_DIR/args")

# shellcheck disable=SC2086
./zitch --fullscreen --low-spec --butler "$APP_DIR/butler" $ARGS
