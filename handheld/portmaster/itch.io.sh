#!/bin/bash
# PORTMASTER: zitch.zip, itch.io.sh

XDG_DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}

if [ -d "/opt/system/Tools/PortMaster/" ]; then
  controlfolder="/opt/system/Tools/PortMaster"
elif [ -d "/opt/tools/PortMaster/" ]; then
  controlfolder="/opt/tools/PortMaster"
elif [ -d "$XDG_DATA_HOME/PortMaster/" ]; then
  controlfolder="$XDG_DATA_HOME/PortMaster"
else
  controlfolder="/roms/ports/PortMaster"
fi

# The firmware's own pad mapping, kept when PortMaster has none for this board.
FIRMWARE_PAD="$SDL_GAMECONTROLLERCONFIG"

source "$controlfolder/control.txt"
[ -f "${controlfolder}/mod_${CFW_NAME}.txt" ] && source "${controlfolder}/mod_${CFW_NAME}.txt"
get_controls

GAMEDIR="/$directory/ports/zitch"
cd "$GAMEDIR" || exit 1

> "$GAMEDIR/log.txt" && exec > >(tee "$GAMEDIR/log.txt") 2>&1

# Config (butler database, installed games) and the cover cache live in
# the port folder, so PortMaster's uninstall takes them too.
HOME="$GAMEDIR/home"
XDG_CONFIG_HOME="$HOME/.config"
XDG_CACHE_HOME="$HOME/.cache"
export HOME XDG_CONFIG_HOME XDG_CACHE_HOME
mkdir -p "$HOME"

export SDL_GAMECONTROLLERCONFIG="${sdl_controllerconfig:-$FIRMWARE_PAD}"

# Extra flags for development, e.g. a screenshot script; one line, no quotes.
ARGS=""
[ -f "$GAMEDIR/args" ] && ARGS=$(cat "$GAMEDIR/args")

chmod +x zitch.aarch64 butler love 2>/dev/null

pm_platform_helper "$GAMEDIR/zitch.aarch64"

# PortMaster updates the port, so zitch's own update check stays out.
# shellcheck disable=SC2086
./zitch.aarch64 --fullscreen --low-spec --no-self-update --butler "$GAMEDIR/butler" $ARGS

pm_finish
