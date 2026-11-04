#!/bin/sh
# zitch as a spruceOS app. The menu runs this from the app folder with no
# arguments and comes back when it exits.

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$APP_DIR" || exit

# The menu discards our output, so the log starts here.
exec >"$APP_DIR/zitch.log" 2>&1

. /mnt/SDCARD/spruce/scripts/appEnv.sh

# The names the emulator configs list devices by, e.g. TRIMUI_BRICK_PRO.
SPRUCE_DEVICE_NAMES="$(device_names | tr '\n' ' ')"
export SPRUCE_DEVICE_NAMES

# Config (butler database, installed games) and the cover cache live next
# to the binary on the SD card.
HOME="$APP_DIR/home"
XDG_CONFIG_HOME="$HOME/.config"
XDG_CACHE_HOME="$HOME/.cache"
export HOME XDG_CONFIG_HOME XDG_CACHE_HOME
mkdir -p "$HOME"

# Name the pad's buttons by their labels, as the prompts are. TrimUI's
# inputd presents an Xbox 360 pad by position, so A/B and X/Y cross over
# on its Nintendo-labelled boards; the other boards carry spruce's own
# label-named map.
case "$SPRUCE_PLATFORM" in
    Brick | BrickPro | SmartPro | SmartProS)
        export SDL_GAMECONTROLLERCONFIG="030000005e0400008e02000014010000,TRIMUI Player1,a:b1,b:b0,x:b3,y:b2,back:b6,guide:b8,start:b7,leftshoulder:b4,rightshoulder:b5,leftstick:b9,rightstick:b10,lefttrigger:a2,righttrigger:a5,leftx:a0,lefty:a1,rightx:a3,righty:a4,dpup:h0.1,dpdown:h0.4,dpleft:h0.8,dpright:h0.2,platform:Linux,"
        ;;
    *)
        export_sdl_gamecontroller_map
        ;;
esac

# Extra flags for development, e.g. a screenshot script; one line, no quotes.
ARGS=""
[ -f "$APP_DIR/args" ] && ARGS=$(cat "$APP_DIR/args")

# shellcheck disable=SC2086
./zitch --fullscreen --low-spec --butler "$APP_DIR/butler" $ARGS
