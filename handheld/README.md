# RG35XX H (muOS)

muOS has no X11 or Wayland, so zitch is built with the SDL2 host
(`--no-default-features --features sdl-host`, `src/host_sdl.rs`) and linked
against the SDL2 that ships with the firmware. Everything else is static.

## Setup

- ssh access to the device as root. The Makefile defaults to
  `root@192.168.4.107`, override with `HANDHELD=root@<ip>`.
- `aarch64-linux-gnu-gcc` and a rustup toolchain with the
  `aarch64-unknown-linux-gnu` target. `CARGO_CROSS` in the Makefile points at
  `~/.cargo/bin/cargo +stable`, change it if rustup is your system cargo.
- `make handheld-sysroot` pulls libc, libm, the loader, libgcc_s and libSDL2
  off the device into `target/handheld-sysroot/lib`. We link against those
  because the cross package's glibc is newer than the device's (2.38) and
  the binary won't load otherwise. Run it again after `cargo clean` or a
  firmware update.
- butler: the linux-arm64 build from https://broth.itch.zone/butler works.
  Copy `butler`, `7z.so` and `libc7zip.so` into
  `/mnt/mmc/MUOS/application/zitch/` on the device. make doesn't do this.

## Build, deploy, run

```
make handheld          # cross-compile (release)
make handheld-deploy   # copy binary + mux_launch.sh into the muOS Applications menu
make handheld-shot     # deploy, launch on the device, fetch /tmp/zitch-handheld.png
make handheld-shot ARGS="--screenshot-script wait:10000,capture"
```

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
