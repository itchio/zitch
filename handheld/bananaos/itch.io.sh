#!/bin/bash
# zitch on BananaOS (RG DS, RG DS Plus). Any script in /data/roms/ports is a
# Ports entry: the menu runs it through /usr/libexec/banana/run-port, which
# gives it the top screen (SDL_KMSDRM_FIRST) and HOME=/data/home, and takes
# the screen back when it exits. The app lives in the zitch folder beside it.

APP_DIR="$(cd "$(dirname "$0")" && pwd)/zitch"
cd "$APP_DIR" || exit 1

# run-port discards nothing, but nobody sees it either; the log goes here.
exec >"$APP_DIR/zitch.log" 2>&1

# Config (login, butler's database, installed games) and the cover cache
# live under /data/home, which a redeploy of the app folder leaves alone.
HOME="/data/home/zitch"
XDG_CONFIG_HOME="$HOME/.config"
XDG_CACHE_HOME="$HOME/.cache"
XDG_DATA_HOME="$HOME/.local/share"
export HOME XDG_CONFIG_HOME XDG_CACHE_HOME XDG_DATA_HOME
mkdir -p "$HOME"

# The RG DS pad (retrogame_joypad), as SDL_GameControllerDB has it. The
# device tree puts the A label on BTN_EAST (b1) and B on BTN_SOUTH (b0), so
# this map already names buttons by their labels, as the prompts do.
export SDL_GAMECONTROLLERCONFIG="190000004b4800000111000000010000,retrogame_joypad,a:b1,b:b0,back:b8,dpdown:b14,dpleft:b15,dpright:b16,dpup:b13,leftshoulder:b4,leftstick:b11,lefttrigger:b6,leftx:a0,lefty:a1,rightshoulder:b5,rightstick:b12,righttrigger:b7,rightx:a2,righty:a3,start:b9,x:b2,y:b3,platform:Linux,"

# Extra flags for development, e.g. a screenshot script; one line, no quotes.
ARGS=""
[ -f "$APP_DIR/args" ] && ARGS=$(cat "$APP_DIR/args")

# shellcheck disable=SC2086
./zitch --fullscreen --low-spec --butler "$APP_DIR/butler" $ARGS
