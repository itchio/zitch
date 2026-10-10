# zitch

This is an experimental itch.io frontend implemented in in Rust using egui.

Since the itch app uses daemon architecture with butlerd, this frontend simply
needs to spawn up the butlerd instance and it can utilize all the same
functionality that our primary app provides. See [Building your own itch.io app
launcher](https://itch.io/docs/butler/launcher-integration.html)

The goal is not to replace our official Electron app, but to explore some other ideas, namely:

* a TV or full screen mode app that can be operated via a controller and looks good in full screen
* the foundation for a game overlay, something that can render on top of the screen while you're in game to provide app functionality

If you have any other ideas that could be fun, drop them in the issues tracker.

Builds for Linux, macOS, Windows and handhelds are on the GitHub releases
page. Desktop builds need butler, found under the itch app's config
directory or on PATH.

## Handhelds

zitch also runs on Linux handhelds through an SDL2 host. For the initial set of
releases we are specifically targeting [muOS](https://muos.dev/) as it provides
a good foundation for development, a rich set of built in "cores" for retro
console emulation, and a framework for installing "apps" into the device
(muxapp). We hope to support many more custom firmwares. If you have on in mind
please open an issue here requesting it so we can gauge interest.

Testing is being done of the following handhelds:

* Ambernic RG35XX H
* Ambernic RG40XX H
* TrimUI Brick Pro

See [handheld/README.md](handheld/README.md) for building, deploying and
testing on devices.

### Running Linux builds on handhelds

A lot of itch.io games ship a Linux build, and on an arm64 handheld
those can run as-is. The problem is the screen: games bundle their own
SDL2, built for X11 or Wayland, and the handheld has neither. zitch
ships an SDL shim (`handheld/sdl-dynapi.c`) and launches Linux builds
with `SDL_DYNAMIC_API` pointing at it. SDL2's dynamic API lets the
game's own copy hand every call to another library, so the shim routes
them into the firmware's libSDL2, which knows how to draw to the panel.

As we test various games we've been expanding the shim's functionality to
handle more cases:

- McPixel 3 (https://devolverdigital.itch.io/mcpixel-3): the routing
  itself. The firmware's jump table isn't laid out like upstream, so
  entries are matched by name.
- Anodyne (https://han-tani.itch.io/anodyne): FNA hands the driver GLSL
  ES 1.00 shaders that write to more than one color target, which the
  Mali driver rejects; the shim rewrites those as GLSL ES 3.00.
- Undrium (https://bitglint.itch.io/undrium): MonoGame games have no
  SDL2 of their own and use the firmware's, so the shim hooks that too,
  and answers the few desktop GL calls MonoGame makes on what is really
  an ES context.

Before offering a Linux build, zitch checks what butler found in it and
says why when it can't run: 32-bit or x86, SDL2 built without the
dynamic API, SDL3, GLFW or raw X11, or a glibc newer than the
firmware's. Details in [handheld/README.md](handheld/README.md).

### Building a Linux build that runs on handhelds

If you're a developer looking to target zitch compatibility here's a short guide:

- Use SDL2 and leave the dynamic API on (it is by default). Any SDL2
  from 2.0 to 2.28 works, static or shared, in the executable or in a
  bundled library. SDL3, GLFW, raw X11/Wayland and Godot's own display
  code can't be redirected to the screen.
- The GL context is OpenGL ES 3.2 whatever profile you ask for. Parse
  the version string with the `OpenGL ES` prefix in mind. Shaders need
  to be GLSL ES 1.00 or 3.00. Multiple color targets need `#version 300
  es` with `layout(location = N) out`, not `gl_FragData[1]`. Avoid
  desktop-only calls: `glPolygonMode`, `glGetTexImage`, `glMapBuffer`,
  `glDrawBuffer`, and the double versions of `glClearDepth` and
  `glDepthRange`. Read textures back through a framebuffer and
  `glReadPixels`. Requesting `SDL_GL_CONTEXT_PROFILE_ES` on every
  platform and keeping one renderer is the simplest way to get this.
- arm64 only, built against glibc 2.38 or older (what the muOS versions
  we currently target ship; zitch checks this and refuses newer). Bundle
  or statically link everything but libc and libm. The firmware has
  libSDL2, libopenal.so.1, libGLESv2 and libEGL and not much else.
  Nothing in the build should need libX11, libpulse or libasound.
- Audio through SDL or OpenAL, both work. Don't use PulseAudio directly.
- Input through SDL's GameController API, the pad shows up as a
  standard controller.
- `LANG` is unset, don't crash on it. Use `SDL_WINDOW_FULLSCREEN_DESKTOP`
  and take whatever size you get. Exit 0 on a normal quit, a non-zero
  status is reported as a crash.
- Ship one folder, no installer, with the executable bit kept (zips made
  on Windows lose it, `butler push` or a tar.gz keep it). Put `arm64` or
  `aarch64` in the upload's file name, zitch picks uploads by name
  rather than itch's platform tags.

Tested so far: LÖVE runs without the shim (the firmware has the
runtime), FNA and plain SDL2 + GL ES games run as is, MonoGame
DesktopGL runs with the shim. Godot, Unity and GameMaker don't: no SDL
path, or no arm64 Linux export.

### Designing for handhelds

- 640x480 is the target minimum resolution, but resolution can also be high
  DPI. (eg. TrimUI Brick Pro is 1024x768). Preferably you game can adapt to
  various resolutions starting from the minimum.
- Everything has to work on a pad. There's no mouse, no keyboard and no
  text entry
- Have a quit option in the game. There's no window close button
- Low-power ARM chips, games on an SD card. Test on device and optimize
- Prefer saves under `$HOME` or `$XDG_DATA_HOME`. Files a game writes into its
  install folder survive updates (butler keeps anything it didn't
  install itself) but go with an uninstall.
- Should support offline play, not require internet connection
- Consider an autosave, as players may power off mid-game

## Credits

Button glyphs are from Kenney's [Input Prompts](https://kenney.nl/assets/input-prompts) pack (CC0); see `assets/prompts/LICENSE-kenney.txt`.
