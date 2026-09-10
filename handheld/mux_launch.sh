#!/bin/sh
# HELP: itch.io library
# ICON: zitch
# GRID: zitch

. /opt/muos/script/var/func.sh

SETUP_APP "zitch" ""

APP_DIR="$1"
cd "$APP_DIR" || exit

# Config (butler database, installed games) and the cover cache live next
# to the binary on the SD card, not under muOS's /root.
HOME="$APP_DIR/home"
XDG_CONFIG_HOME="$HOME/.config"
XDG_CACHE_HOME="$HOME/.cache"
export HOME XDG_CONFIG_HOME XDG_CACHE_HOME
mkdir -p "$HOME"

# Extra flags for development, e.g. a screenshot script; one line, no quotes.
ARGS=""
[ -f "$APP_DIR/args" ] && ARGS=$(cat "$APP_DIR/args")

# shellcheck disable=SC2086
./zitch --fullscreen --low-spec --minimize-while-playing --butler "$APP_DIR/butler" $ARGS >"$APP_DIR/zitch.log" 2>&1
