# RG35XX H (muOS)

muOS has no X11 or Wayland, so zitch is built with the SDL2 host
(`--no-default-features --features sdl-host`, `src/host_sdl.rs`) and linked
against the SDL2 that ships with the firmware. Everything else is static.


## Setup

- ssh access to the device as root. The Makefile defaults to
  `root@DEVICE_IP`, override with `HANDHELD=root@<ip>`.
- `aarch64-linux-gnu-gcc` and a rustup toolchain with the
  `aarch64-unknown-linux-gnu` target. `CARGO_CROSS` in the Makefile points at
  `~/.cargo/bin/cargo +stable`, change it if rustup is your system cargo.
- `make handheld-sysroot` pulls libc, libm, the loader, libgcc_s and libSDL2
  off the device into `target/handheld-sysroot/lib`. We link against those
  because the cross package's glibc is newer than the device's (2.38) and
  the binary won't load otherwise. Run it again after `cargo clean` or a
  firmware update.
- butler: the linux-arm64-head build from https://broth.itch.zone/butler works
  (the versioned linux-arm64 channel lags master).
  Copy `butler`, `7z.so` and `libc7zip.so` into
  `/mnt/mmc/MUOS/application/zitch/` on the device. `handheld-deploy`
  doesn't do this; `handheld-muxapp` downloads them.

## Build, deploy, run

```
make handheld          # cross-compile (release)
make handheld-deploy   # copy binary + mux_launch.sh + LÖVE into the muOS Applications menu
make handheld-shot     # deploy, launch on the device, fetch /tmp/zitch-handheld.png
make handheld-shot ARGS="--screenshot-script wait:10000,capture"
make handheld-muxapp   # package with butler and LÖVE as target/zitch.muxapp
```

## Packaging

A `.muxapp` is what muOS's Archive Manager installs: a plain zip whose
top level is the app folder, unpacked with `unzip -o` into the
Applications directory (`script/mux/extract.sh` in MustardOS/internal).
Paths must be relative with no `..`, entries must be regular files or
directories, and the executable bits come from the zip. To install, copy
`zitch.muxapp` to `/mnt/mmc/ARCHIVE` on the device and pick it under
Applications > Archive Manager; the result is the same folder
`handheld-deploy` writes, plus butler.

CI builds one on an arm64 runner (`muos` job in `.github/workflows/build.yml`)
without the device sysroot, so the binary links against the runner's
glibc; a check there fails the job if it needs a symbol version newer than
the device's 2.38. The firmware's SDL2 is still what it loads at run time.

`handheld-shot` launches the app the same way picking it from the menu does:
write `/tmp/app_go` and `/tmp/act_go`, kill `muxfrontend` (SIGKILL, it
ignores SIGTERM), and `frontend.sh` runs `mux_launch.sh`. When zitch exits
the frontend comes back by itself.

Don't run zitch over ssh while the frontend is still up. They both draw
into the same framebuffer and the frontend's background bleeds through.

Log: `/mnt/mmc/MUOS/application/zitch/zitch.log`. Config and the butler db
are in `home/.config/zitch` in that folder, covers in `home/.cache/zitch`.

`mux_launch.sh` reads extra flags from an `args` file next to it (one line,
no quotes). `handheld-shot` writes and removes it.

## Running games

The device runs ROMs and engine files, and Linux builds through the SDL
shim below. On muOS zitch
judges an upload by its file name instead of itch's platform tags, and
takes an archive with no platform tags too, since that is how most
uploads arrive; butler unpacks it, and Play asks butler's
`Launch.GetTargets` what it holds, naming the runtimes the firmware has
(`muos::runtimes`). A ROM, cart or LÖVE payload comes back as a
`runtime` target with dash's engine record, an arm64 build as a
`native` one. Every runnable choice is offered together, Linux builds
first: the developer's own build is the real engine where the
firmware's core may be a reimplementation (a PICO-8 Raspberry Pi
export is the official player, fake-08 rejects newer cart syntax).
One choice launches at once; more and the user picks. The pick goes to
butler's `Launch` as the target. A payload comes back in a
`RuntimeLaunch` request, which zitch answers by running it
(`src/muos.rs`) and replying when it exits; a Linux build butler
launches itself, through the SDL shim. butler tracks the run either
way, so play time and the itch.io session are recorded. Quitting from
zitch ends the process group the payload runs in. A Linux build's
deep-probe record is checked before it is offered
(`muos::native_blocker`): a 32-bit ARM or x86 build, a bundled SDL2
without the dynamic API, SDL3, a GLFW or X11 build, or a glibc newer
than the firmware's is left out with the reason, which becomes the
failure when nothing else can run. Every game gets an Install button.
Bare files butler has no installer for (`.nes`, `.gba`, ...) are queued
with `ignoreInstallers` so they install as a copy.

Each runtime below is launched without zitch's HOME and XDG variables, or
it starts with the app's config dir instead of the firmware's. The muOS
panic combo (R2 + Select + B) kills the process named in
`/opt/muos/config/system/foreground_process`; zitch names the game there
while it runs and itself again after.

### ROMs and fantasy console carts (RetroArch)

Files: `.nes`, `.sfc`/`.smc`, `.gb`, `.gbc`, `.gba`, `.md`/`.gen`,
`.prg`/`.d64`/`.crt` (C64), `.adf`/`.hdf` (Amiga), `.z64`/`.n64`, and
`.p8`/`.p8.png` PICO-8 carts, which the firmware plays with the fake-08
core under its `PICO-8` system, no official PICO-8 binary needed, and
`.tic` TIC-80 carts, played with the tic80 core under `TIC-80`. butler
reports a cart as a `pico8-cart` or `tic80-cart` payload rather than a
ROM, so it is asked for by that name and mapped to the same launch path.

Launched the way the muOS menu launches one: write `/tmp/rom_go`,
`/tmp/gov_go` and `/tmp/flt_go`, run `/opt/muos/script/mux/launch.sh`,
wait for RetroArch to exit. The core per system is the firmware's
`default=` from `/opt/muos/share/info/assign/<system>/global.ini`. The
script always exits 1 (its last line is a Discord check), so its status
is ignored. Quit with Menu held + Start.

### LÖVE

Files: `.love`, or a folder with `main.lua` at its root.

Runs in the LÖVE 11.5 deployed next to `zitch` (`love` with `libs/` on
`LD_LIBRARY_PATH`). muOS has no LÖVE of its own; the binaries ride
inside whichever bundled app is written in LÖVE (Moonlight and RGB
Controller on FUNKY_JACARANDA, 2048 Plus on Andromeda), so
`make handheld-love` fetches the same blobs from a pinned commit of
MustardOS/internal, md5-checked, and the deploy and muxapp targets ship
them. Without them `muos::love` falls back to whatever the firmware
carries. The game's own pad handling applies; there is no
keyboard mapping helper (Moonlight runs `gptokeyb2` for its GUI). Quit
with the game's own quit or the panic combo.

### Linux builds

Native arm64 builds go through butler's launcher as on a desktop, with
two additions to their environment (`muos::game_env`):

- `SDL_DYNAMIC_API` naming `libzitch-sdl.so`, built from
  `handheld/sdl-dynapi.c` by `make handheld` and deployed next to the
  binary. Games carry their own SDL2, built with the X11, Wayland and
  KMSDRM backends the device lacks, so they can't open the screen. SDL2's
  dynamic API lets that copy hand every call to another library instead;
  the shim points it at the firmware's libSDL2, whose `mali` backend can.
  The firmware library's jump table doesn't match upstream's order, so
  entries are matched by name: the shim reads each SDL stub in the game
  to find its slot, checks that against `handheld/sdl-dynapi-procs.h`
  (`make handheld-sdl-procs` refreshes it from SDL's source), and fills
  the rest from that list. It refuses a game whose stubs disagree, and a
  dynamically linked SDL2 asking to be replaced by itself. It opens the
  firmware's library at `/usr/lib` or `/usr/lib/aarch64-linux-gnu`;
  `ZITCH_SDL_LIB` names another.
- `LANG=en_US.UTF-8` when unset. The firmware sets no locale and games
  read it without checking.

Only an SDL2 built with the dynamic API on (the default) can be routed.
A game with it off, or on SDL3 or another windowing library, still
fails to open a display. Quit with the game's own quit.

## Sign in

First run needs `ARGS="--api-key-file <path on device>"`, the file is just
the key. butler saves the profile in its db after that. Delete the key file
from the device once it's in.

## Framebuffer dump

`--screenshot` reads back from GL. To see what's actually on the panel:

```
ssh root@<ip> 'head -c 1228800 /dev/fb0' > fb.raw
python3 -c "from PIL import Image; Image.frombytes('RGBA',(640,480),open('fb.raw','rb').read(),'raw','BGRA').save('fb.png')"
```

## Device notes

- muOS 2601, kernel 4.9, glibc 2.38, 4x Cortex-A53, 1 GB RAM.
- No `/dev/dri`. `/dev/fb0` is 640x480 32bpp. SDL2 2.28 with a custom `mali`
  video driver on top of the Mali blob. GLES 3.2, Mali-G31.
- One evdev controller, `muOS-Keys`. `SETUP_SDL_ENVIRONMENT` exports the
  gamecontrollerdb mapping so SDL sees a normal game controller. Triggers
  come through as axes.
- `SETUP_APP` (in `/opt/muos/script/var/func.sh`) sets HOME and
  XDG_CONFIG_HOME to /root. `mux_launch.sh` overrides them after.
