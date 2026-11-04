//! Games on handheld firmwares. A firmware ships emulators for the
//! systems it knows (PICO-8 and TIC-80 carts among them), and we ship a
//! LÖVE runtime; most games there are a ROM, a cart or a `.love`.
//! butler's launch targets say which an install holds ([`content_for`]),
//! and this module runs them the way the firmware's own menu does: a ROM
//! or cart through the firmware's launcher, a `.love` through the LÖVE
//! binary. A Linux build is butler's to launch, through the SDL shim;
//! [`native_blocker`] says beforehand when one cannot reach the screen.
//!
//! What every firmware shares is here; what differs is behind
//! [`Firmware`], one implementation per firmware in its own module.
//! Nothing is compiled out on other systems; [`available`] is a runtime
//! check, so the desktop build simply never takes these paths.

mod appimage;
mod content;
mod love;
mod muos;
mod running;
mod spruce;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Result, bail};

pub use appimage::*;
pub use content::*;
pub use love::*;
pub use running::*;

/// What a firmware does differently. The defaults are a firmware with
/// nothing of its own, on which games are our LÖVE and Linux builds.
pub trait Firmware: Sync {
    /// Its name in device info, e.g. `muos`.
    fn name(&self) -> &'static str;

    /// More for device info: the board, a release.
    fn details(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    /// The systems it has emulators for, by dash id.
    fn systems(&self) -> HashMap<String, System> {
        HashMap::new()
    }

    /// Runs `rom` in its emulator for `system` and returns when that
    /// exits. `name` is what the firmware shows for the game; `env` is
    /// the manifest action's. Only reached for a system [`systems`]
    /// listed.
    fn launch_rom(
        &self,
        _name: &str,
        system: &System,
        _rom: &Path,
        _env: &HashMap<String, String>,
    ) -> Result<()> {
        bail!("{} has no launcher for {}", self.name(), system.label)
    }

    /// LÖVE binaries it carries, tried after ours.
    fn love_candidates(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Whether a directory inherited on `LD_LIBRARY_PATH` from its
    /// launcher is safe for games.
    fn keeps_library_dir(&self, _dir: &Path) -> bool {
        true
    }

    /// What it adds to a game's environment.
    fn game_env(&self, _env: &mut Vec<(String, OsString)>) {}

    /// A game has the screen, with this pid.
    fn game_started(&self, _pid: u32) {}

    /// The screen is zitch's again.
    fn game_ended(&self) {}

    /// Where a downloaded release package goes for its installer.
    fn update_archive_dir(&self) -> Option<PathBuf> {
        None
    }
}

/// The firmware this is running on, if any. Each is detected by
/// something only it has at run time.
pub fn firmware() -> Option<&'static dyn Firmware> {
    static FIRMWARE: OnceLock<Option<&'static dyn Firmware>> = OnceLock::new();
    *FIRMWARE.get_or_init(|| muos::detect().or_else(spruce::detect))
}

/// Whether this is a handheld firmware zitch knows.
pub fn available() -> bool {
    firmware().is_some()
}

/// A stand-in firmware for tests: what the device has, set per thread so
/// a test decides instead of the machine it runs on.
#[cfg(test)]
mod fake {
    use std::cell::Cell;
    use std::collections::HashMap;

    use super::{Love, System};

    #[derive(Clone, Copy)]
    pub struct Firmware {
        pub systems: &'static HashMap<String, System>,
        pub love: Option<&'static Love>,
    }

    thread_local! {
        static FIRMWARE: Cell<Option<Firmware>> = const { Cell::new(None) };
    }

    pub fn firmware() -> Option<Firmware> {
        FIRMWARE.with(Cell::get)
    }

    /// Runs `f` on a device with these systems and this LÖVE.
    pub fn with<T>(systems: Vec<System>, love: Option<Love>, f: impl FnOnce() -> T) -> T {
        let systems: HashMap<String, System> =
            systems.into_iter().map(|s| (s.id.clone(), s)).collect();
        let firmware = Firmware {
            systems: Box::leak(Box::new(systems)),
            love: love.map(|l| &*Box::leak(Box::new(l))),
        };
        let before = FIRMWARE.with(|cell| cell.replace(Some(firmware)));
        let out = f();
        FIRMWARE.with(|cell| cell.set(before));
        out
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::butlerd::types::{Arch, Candidate, Engine, EngineInfo, Flavor, LinuxInfo};

    /// A device with nothing: no emulators, no LÖVE.
    fn bare<T>(f: impl FnOnce() -> T) -> T {
        fake::with(Vec::new(), None, f)
    }

    fn system(id: &str) -> System {
        System {
            id: id.to_string(),
            name: id.to_uppercase(),
            launcher: format!("mu-{id}"),
            core: format!("{id}_libretro.so"),
            label: id.to_uppercase(),
        }
    }

    fn love_11_5() -> Love {
        Love {
            binary: PathBuf::from("/app/love"),
            libs: PathBuf::from("/app/libs"),
            version: "11.5".to_string(),
        }
    }

    #[test]
    fn extension_picks_the_id() {
        assert_eq!(id_for_file(Path::new("Bobl 1.2.NES")), Some("nes"));
        assert_eq!(id_for_file(Path::new("tobudx.gb")), Some("gb"));
        assert_eq!(id_for_file(Path::new("a/b/game.gbc")), Some("gbc"));
        assert_eq!(id_for_file(Path::new("game.SFC")), Some("snes"));
        assert_eq!(id_for_file(Path::new("knuckles.32x")), Some("32x"));
        assert_eq!(id_for_file(Path::new("pitfall.a26")), Some("a26"));
        assert_eq!(id_for_file(Path::new("game.lnx")), Some("lynx"));
        assert_eq!(id_for_file(Path::new("power_pong.p8.png")), Some(PICO8));
        assert_eq!(id_for_file(Path::new("cart.P8")), Some(PICO8));
        assert_eq!(id_for_file(Path::new("island.tic")), Some(TIC80));
        assert_eq!(id_for_file(Path::new("game.bin")), None);
        assert_eq!(id_for_file(Path::new("game.cue")), None);
        assert_eq!(id_for_file(Path::new("game.iso")), None);
        assert_eq!(id_for_file(Path::new("game.chd")), None);
        assert_eq!(id_for_file(Path::new("cover.png")), None);
        assert_eq!(id_for_file(Path::new("setup.exe")), None);
        assert_eq!(id_for_file(Path::new("README")), None);
    }

    #[test]
    fn love_by_file_name() {
        assert!(runs_here(Path::new("x-moon-love11.5.love")));
        assert!(!runs_here(Path::new("xmoon-win32.zip")));
    }

    fn payload(flavor: Flavor, engine: Option<EngineInfo>) -> Candidate {
        Candidate {
            flavor,
            engine,
            ..Default::default()
        }
    }

    #[test]
    fn disc_images_are_named_only_by_their_header() {
        assert!(is_disc_image(Path::new("game.cue")));
        assert!(is_disc_image(Path::new("game.CHD")));
        assert!(is_disc_image(Path::new("a/b/game.iso")));
        assert!(!is_disc_image(Path::new("game.bin")));
        assert!(!is_disc_image(Path::new("game.nes")));
        assert!(!is_disc_image(Path::new("cover.png")));
        assert!(!is_disc_image(Path::new("README")));
        assert!(!bare(|| disc_runs_here(Path::new("game.cue"))));
        assert!(fake::with(vec![system("psx")], None, || {
            disc_runs_here(Path::new("game.cue"))
        }));
        assert!(!fake::with(vec![system("psx")], None, || {
            disc_runs_here(Path::new("game.nes"))
        }));
    }

    #[test]
    fn a_rom_with_no_system_says_so() {
        let unread = payload(
            Flavor::ROM,
            Some(EngineInfo {
                engine: Engine::ROM,
                version: None,
                details: Some(HashMap::from([(
                    "system".to_string(),
                    serde_json::Value::String(String::new()),
                )])),
            }),
        );
        assert_eq!(
            content_for(&unread, PathBuf::from("/g/game.chd")),
            Err("could not tell which system this is for".to_string())
        );
        // and with no details at all, rather than "no emulator for "
        let bare = payload(Flavor::ROM, None);
        assert_eq!(
            content_for(&bare, PathBuf::from("/g/game.chd")),
            Err("could not tell which system this is for".to_string())
        );
    }

    #[test]
    fn old_love_games_are_turned_away() {
        assert_eq!(love_blocker("11.5", "11.5"), None);
        assert_eq!(love_blocker("11.3", "11.5"), None);
        assert_eq!(love_blocker("11.5", "11.5.1"), None);
        assert_eq!(love_blocker("", "11.5"), None);
        assert_eq!(love_blocker("11.6", "11.5"), None);
        assert_eq!(love_blocker("12.0", "11.5"), None);
        assert!(love_blocker("0.8.0", "11.5").is_some());
        assert!(love_blocker("0.10.2", "11.5").unwrap().contains("11.5"));
    }

    #[test]
    fn love_platforms_up_to_ours() {
        assert_eq!(
            love_platforms_for("11.2"),
            ["love:11.0", "love:11.1", "love:11.2", "love:12.0"]
        );
        assert!(love_platforms_for("").is_empty());
    }

    #[test]
    fn payloads_without_a_runtime_are_turned_away() {
        let love = payload(
            Flavor::Love,
            Some(EngineInfo {
                engine: Engine::Love,
                version: Some("11.5".into()),
                details: None,
            }),
        );
        assert_eq!(
            bare(|| content_for(&love, PathBuf::from("/g/game.love"))),
            Err("this device has no LÖVE".to_string())
        );
        assert!(matches!(
            fake::with(Vec::new(), Some(love_11_5()), || {
                content_for(&love, PathBuf::from("/g/game.love"))
            }),
            Ok(Content::Love { .. })
        ));
        // A native build has no runtime on any firmware.
        let native = payload(Flavor::NativeLinux, None);
        assert!(bare(|| content_for(&native, PathBuf::from("/g/bin"))).is_err());
    }

    #[test]
    fn pico8_web_exports_are_turned_away() {
        let export = payload(
            Flavor::Pico8Cart,
            Some(EngineInfo {
                engine: Engine::Pico8,
                version: Some("0.2.2".into()),
                details: Some(HashMap::from([(
                    "format".to_string(),
                    serde_json::Value::String("js".into()),
                )])),
            }),
        );
        assert_eq!(
            content_for(&export, PathBuf::from("/g/game.js")),
            Err("PICO-8 web exports are not supported yet".to_string())
        );
    }

    #[test]
    fn scanned_native_builds_must_be_arm64_linux() {
        let fits = |json: serde_json::Value| {
            let target: ScannedTarget = serde_json::from_value(json).unwrap();
            scanned_target_fits(&target, (2, 38))
        };
        // the site's layout: snake_case outside, butler's camelCase inside
        assert!(fits(serde_json::json!({
            "path": "bin/game", "depth": 2, "flavor": "linux", "arch": "arm64",
            "linux_info": { "arch": "arm64", "glibcVersion": "2.31", "sdl": "2", "imports": ["libSDL2-2.0.so.0"] }
        })));
        assert!(fits(
            serde_json::json!({ "path": "game", "flavor": "linux", "arch": "arm64" })
        ));
        assert!(!fits(
            serde_json::json!({ "path": "game", "flavor": "linux" })
        ));
        assert!(!fits(
            serde_json::json!({ "path": "game", "flavor": "linux", "arch": "amd64" })
        ));
        assert!(!fits(
            serde_json::json!({ "path": "game", "flavor": "linux", "arch": "arm" })
        ));
        assert!(!fits(serde_json::json!({
            "path": "game", "flavor": "linux", "arch": "arm64",
            "linux_info": { "arch": "arm64", "glibcVersion": "2.39" }
        })));
        assert!(!fits(serde_json::json!({
            "path": "game", "flavor": "linux", "arch": "arm64",
            "linux_info": { "arch": "arm64", "os": "freebsd" }
        })));
        assert!(!fits(
            serde_json::json!({ "path": "game.exe", "flavor": "windows", "arch": "arm64" })
        ));
        // A LÖVE game without a version is never known to fit; one with a
        // version fits where there is a LÖVE for it.
        let unversioned = serde_json::json!({ "path": "game.love", "flavor": "love" });
        let versioned = serde_json::json!({
            "path": "game.love", "flavor": "love",
            "engine": { "engine": "love", "version": "11.4" }
        });
        assert!(!bare(|| fits(unversioned.clone())));
        assert!(!bare(|| fits(versioned.clone())));
        assert!(fake::with(Vec::new(), Some(love_11_5()), || {
            !fits(unversioned.clone()) && fits(versioned.clone())
        }));
        assert!(!fits(
            serde_json::json!({ "path": "x.bin", "flavor": "some-new-flavor" })
        ));
    }

    #[test]
    fn native_builds_are_judged_by_their_sdl() {
        let native_blocker = |info: &LinuxInfo| blocker(info, (2, 38));
        let info = |sdl: Option<&str>, bundled: bool, dynamic: bool| LinuxInfo {
            sdl: sdl.map(str::to_string),
            sdl_bundled: Some(bundled),
            sdl_dynamic_api: Some(dynamic),
            ..Default::default()
        };
        assert_eq!(native_blocker(&info(Some("2"), false, false)), None);
        assert_eq!(native_blocker(&info(Some("2"), true, true)), None);
        assert!(native_blocker(&info(Some("2"), true, false)).is_some());
        assert!(native_blocker(&info(Some("3"), true, true)).is_some());
        assert_eq!(native_blocker(&info(None, false, false)), None);
        assert!(
            native_blocker(&LinuxInfo {
                display: Some(vec!["glfw".into(), "x11".into()]),
                ..Default::default()
            })
            .is_some()
        );
        assert!(
            native_blocker(&LinuxInfo {
                sdl: Some("2".into()),
                glibc_version: Some("2.39".into()),
                ..Default::default()
            })
            .is_some()
        );
        assert_eq!(
            native_blocker(&LinuxInfo {
                sdl: Some("2".into()),
                glibc_version: Some("2.34".into()),
                ..Default::default()
            }),
            None
        );
        assert_eq!(
            native_blocker(&LinuxInfo {
                arch: Some(Arch::Arm64),
                sdl: Some("2".into()),
                ..Default::default()
            }),
            None
        );
        assert!(
            native_blocker(&LinuxInfo {
                arch: Some(Arch::Arm),
                sdl: Some("2".into()),
                ..Default::default()
            })
            .is_some()
        );
        assert!(
            native_blocker(&LinuxInfo {
                arch: Some(Arch::Amd64),
                ..Default::default()
            })
            .is_some()
        );
    }

    #[test]
    fn an_appimage_is_offered_as_its_unpacked_apprun() {
        use crate::butlerd::types::{LaunchStrategy, LaunchTarget, StrategyResult};
        let native = |path: &str, flavor: Flavor, arch: Option<Arch>| LaunchTarget {
            strategy: Some(StrategyResult {
                strategy: LaunchStrategy::Native,
                full_target_path: format!("/games/x/{path}"),
                candidate: Some(Candidate {
                    path: path.to_string(),
                    flavor,
                    linux_info: arch.map(|arch| LinuxInfo {
                        arch: Some(arch),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let image = native("Game.AppImage", Flavor::NativeLinux, Some(Arch::Arm64));
        let run = native("Game.AppImage.d/AppRun", Flavor::Script, None);
        let inner = |arch| {
            native(
                "Game.AppImage.d/usr/bin/Game",
                Flavor::NativeLinux,
                Some(arch),
            )
        };

        let (choices, unrunnable) =
            bare(|| launch_choices(&[image.clone(), run.clone(), inner(Arch::Arm64)]));
        assert!(unrunnable.is_empty(), "{unrunnable:?}");
        let labels: Vec<(&str, &str)> = choices
            .iter()
            .map(|c| (c.label.as_str(), c.target.as_str()))
            .collect();
        assert_eq!(
            labels,
            [("Game.AppImage (Linux build)", "Game.AppImage.d/AppRun")]
        );

        let (choices, unrunnable) = bare(|| launch_choices(&[image, run, inner(Arch::Arm)]));
        assert!(choices.is_empty());
        assert_eq!(unrunnable.len(), 1);
        assert!(
            unrunnable[0].starts_with("Game.AppImage is a 32-bit"),
            "{unrunnable:?}"
        );
    }
}
