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

Once we get a bit further along, we'll publish signed builds to itch.io, and on
[Broth](https://broth.itch.zone).
