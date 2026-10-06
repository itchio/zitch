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

zitch also runs on Linux handhelds through an SDL2 host. muOS is the
first firmware supported (RG35XX H, RG40XX H, TrimUI Brick Pro), and a
PortMaster port installs on others. See
[handheld/README.md](handheld/README.md) for building, deploying and
testing on the device.

## Credits

Button glyphs are from Kenney's [Input Prompts](https://kenney.nl/assets/input-prompts) pack (CC0); see `assets/prompts/LICENSE-kenney.txt`.
