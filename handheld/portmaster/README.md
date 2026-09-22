## Notes

itch.io client for handhelds. Sign in with your phone, browse your
library, install and play. LÖVE games and Linux arm64 builds run on any
firmware. On muOS, ROMs and PICO-8 carts launch through the firmware's
libretro cores; the same for other firmwares is in progress.

Source: https://github.com/itchio/zitch

## Controls

| Button | Action |
|--|--|
| D-Pad / Left stick | Move |
| A | Select / Play |
| B | Back |
| Y | Search |
| L1 / R1 | Switch tab |
| Start | Menu |

## Compile

```shell
git clone https://github.com/itchio/zitch
cd zitch
make handheld-sysroot HANDHELD=root@<device>   # once; pulls the firmware's libc and SDL2
make handheld-port
```

`target/zitch.zip` is the port. See `handheld/README.md` in the repo for
the cross toolchain.

## Licenses

zitch is MIT. Bundled: butler (MIT), LÖVE (zlib), LuaJIT (MIT), Kenney
Input Prompts (CC0). Copies are in `zitch/licenses/`.
