//! Games on muOS. The handheld firmware ships RetroArch with a core for
//! each system it knows (PICO-8 and TIC-80 carts among them), and we
//! ship a LÖVE runtime; most games there are a ROM, a cart or a `.love`.
//! butler's launch targets say which an install holds ([`content_for`]),
//! and this module runs them the way the firmware's own menu does: a ROM
//! or cart through its launch script and RetroArch, a `.love` through
//! the LÖVE binary. A Linux build is butler's to launch,
//! through the SDL shim; [`native_blocker`] says beforehand when one
//! cannot reach the screen.
//!
//! Nothing here is compiled out on other systems; [`available`] is a
//! runtime check for the firmware's script, so the desktop build simply
//! never takes these paths.

mod compat;
mod firmware;
mod running;

pub use compat::*;
pub use firmware::*;
pub use running::*;

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
            assign: id.to_uppercase(),
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

    const ASSIGN_JSON: &str = r#"{ "nes": "Nintendo NES - Famicom", "gba": "Nintendo Game Boy Advance",
  "md": "Sega Mega Drive - Genesis", "a2600": "Atari 2600", "pico8": "PICO-8", "n64": "Nintendo N64" }"#;

    const NES_GLOBAL: &str = "[global]\nname=Nintendo NES - Famicom\ndefault=mu-fceumm\n\
catalogue=Nintendo NES - Famicom\nlookup=0\n\n[friendly]\nNintendo NES - Famicom\nNES\nFamicom\n";

    const NES_LAUNCHER: &str = "[mu-fceumm]\nname=mu-FCEUmm\ncore=fceumm_libretro.so\n\n\
[launch]\nprep=\nexec=/opt/muos/script/launch/mu-general.sh\ndone=\n";

    fn fixture() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("Nintendo NES - Famicom/global.ini", NES_GLOBAL),
            ("Nintendo NES - Famicom/mu-fceumm.ini", NES_LAUNCHER),
            (
                "Atari 2600/global.ini",
                "[global]\nname=Atari 2600\ndefault=stella\n",
            ),
            (
                "Atari 2600/stella.ini",
                "[stella]\nname=Stella\ncore=stella_libretro.so\n",
            ),
            (
                "Nintendo Game Boy Advance/global.ini",
                "[global]\nname=Nintendo Game Boy Advance\ndefault=mgba\n",
            ),
        ])
    }

    fn lookup(id: &str) -> Option<System> {
        let files = fixture();
        resolve(id, &parse_assign(ASSIGN_JSON), |rel| {
            files.get(rel).map(|s| s.to_string())
        })
    }

    #[test]
    fn assign_json_maps_ids_to_folders() {
        let aliases = parse_assign(ASSIGN_JSON);
        assert_eq!(aliases["nes"], "Nintendo NES - Famicom");
        assert_eq!(aliases["md"], "Sega Mega Drive - Genesis");
        assert!(!aliases.contains_key("nds"));
        assert!(parse_assign("not json").is_empty());
    }

    #[test]
    fn ini_values_come_from_their_section() {
        assert_eq!(
            ini_value(NES_GLOBAL, "global", "default"),
            Some("mu-fceumm")
        );
        assert_eq!(
            ini_value(NES_GLOBAL, "global", "name"),
            Some("Nintendo NES - Famicom")
        );
        assert_eq!(ini_value(NES_GLOBAL, "friendly", "default"), None);
        assert_eq!(
            ini_value(NES_LAUNCHER, "mu-fceumm", "core"),
            Some("fceumm_libretro.so")
        );
        assert_eq!(ini_value(NES_LAUNCHER, "launch", "prep"), Some(""));
        assert_eq!(ini_value(NES_LAUNCHER, "mu-fceumm", "exec"), None);
    }

    #[test]
    fn ids_resolve_through_the_assign_metadata() {
        assert_eq!(
            lookup("nes"),
            Some(System {
                id: "nes".into(),
                assign: "Nintendo NES - Famicom".into(),
                launcher: "mu-fceumm".into(),
                core: "fceumm_libretro.so".into(),
                label: "Nintendo NES - Famicom".into(),
            })
        );
        // a26 is a2600 in assign.json.
        let a26 = lookup("a26").unwrap();
        assert_eq!(a26.id, "a26");
        assert_eq!(a26.assign, "Atari 2600");
        assert_eq!(a26.core, "stella_libretro.so");
        // Not in assign.json at all.
        assert_eq!(lookup("nds"), None);
        // In assign.json, but no folder.
        assert_eq!(lookup("md"), None);
        // A folder whose default launcher ini is missing.
        assert_eq!(lookup("gba"), None);
    }

    const LIBRETRO_JSON: &str = r#"{
      "Nintendo Game Boy Advance": {
        "name": "Nintendo Game Boy Advance",
        "default": "mgba",
        "friendly": ["gba"],
        "bios": [{"file": "gba_bios.bin"}],
        "cores": {
          "gpsp": {"name": "gpSP", "core": "gpsp_libretro.so"},
          "mgba": {"name": "mGBA", "core": "mgba_libretro.so", "governor": "performance"}
        }
      },
      "Nintendo NES - Famicom": {"default": "nestopia", "cores": {}}
    }"#;

    const EXTERNAL_JSON: &str = r#"{
      "Nintendo N64": {
        "default": "mupen64plus - standalone - glide",
        "cores": {
          "mupen64plus - standalone - glide": {"core": "ext-mupen64plus-gliden64", "launcher": "mupen64plus.sh"}
        }
      }
    }"#;

    #[test]
    fn ids_resolve_through_the_manifests() {
        let aliases = parse_assign(ASSIGN_JSON);
        let manifests = vec![
            (
                "mu-",
                parse_manifest("libretro.json", LIBRETRO_JSON).unwrap(),
            ),
            (
                "ext-",
                parse_manifest("external.json", EXTERNAL_JSON).unwrap(),
            ),
        ];
        assert_eq!(
            resolve_manifest("gba", &aliases, &manifests, None),
            Some(System {
                id: "gba".into(),
                assign: "Nintendo Game Boy Advance".into(),
                launcher: "mu-mgba".into(),
                core: "mgba_libretro.so".into(),
                label: "Nintendo Game Boy Advance".into(),
            })
        );
        // Only in the second manifest.
        let n64 = resolve_manifest("n64", &aliases, &manifests, None).unwrap();
        assert_eq!(n64.core, "ext-mupen64plus-gliden64");
        assert_eq!(n64.launcher, "ext-mupen64plus - standalone - glide");
        // Not in assign.json at all.
        assert_eq!(resolve_manifest("nds", &aliases, &manifests, None), None);
        // In assign.json, in no manifest.
        assert_eq!(resolve_manifest("md", &aliases, &manifests, None), None);
        // Listed, but its default core is not.
        assert_eq!(resolve_manifest("nes", &aliases, &manifests, None), None);
        assert!(parse_manifest("x.json", "not json").is_none());
    }

    #[test]
    fn a_core_for_other_boards_is_left_out() {
        let aliases = parse_assign(r#"{"3ds": "Nintendo 3DS"}"#);
        let external = r#"{
          "Nintendo 3DS": {
            "default": "azahar - standalone",
            "cores": {
              "azahar - standalone": {
                "core": "ext-azahar",
                "launcher": "azahar.sh",
                "require": {"device": ["rg-vita-pro"]}
              }
            }
          }
        }"#;
        let manifests = vec![("ext-", parse_manifest("external.json", external).unwrap())];
        assert_eq!(
            resolve_manifest("3ds", &aliases, &manifests, Some("rg40xx-h")),
            None
        );
        assert_eq!(resolve_manifest("3ds", &aliases, &manifests, None), None);
        let vita = resolve_manifest("3ds", &aliases, &manifests, Some("rg-vita-pro")).unwrap();
        assert_eq!(vita.core, "ext-azahar");
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
}
