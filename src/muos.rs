//! Games on muOS. The handheld firmware ships RetroArch with a core for
//! each system it knows, and a LÖVE runtime; most games there are a ROM
//! or a `.love`. butler's launch targets say which an install holds
//! ([`content_for`]), and this module runs them the way the firmware's
//! own menu does: a ROM through its launch script and RetroArch, a
//! `.love` through its LÖVE binary. A Linux build is butler's to launch,
//! through the SDL shim; [`native_blocker`] says beforehand when one
//! cannot reach the screen.
//!
//! Nothing here is compiled out on other systems; [`available`] is a
//! runtime check for the firmware's script, so the desktop build simply
//! never takes these paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

use crate::butlerd::types::{Engine, Flavor, LaunchStrategy, LaunchTarget, LinuxInfo};

const LAUNCH_SCRIPT: &str = "/opt/muos/script/mux/launch.sh";
/// What the frontend writes before running the launch script: the
/// content to run, the CPU governor while it runs, and a colour filter.
const ROM_GO: &str = "/tmp/rom_go";
const GOV_GO: &str = "/tmp/gov_go";
const FLT_GO: &str = "/tmp/flt_go";
/// The governor the firmware goes back to after content.
const DEFAULT_GOVERNOR: &str = "/opt/muos/device/config/cpu/default";
/// What the firmware's R2+Select+B panic combo kills (`proc_die.sh`).
/// The app launcher set it to zitch; while a game has the screen it must
/// name the game, or the combo kills zitch under it and orphans the game.
/// A name is looked up with busybox `pgrep -x`, which matches the whole
/// `argv[0]`, so a process started by path only matches by pid.
const FOREGROUND_PROCESS: &str = "/opt/muos/config/system/foreground_process";
/// The firmware's LÖVE 11.5, shipped for its Moonlight client. The binary
/// links `libs/liblove-11.5.so` and the system SDL2.
const LOVE_DIR: &str = "/opt/muos/share/application/Moonlight";
/// What that LÖVE reports, and the major version it runs games for.
const LOVE_VERSION: &str = "11.5";
/// The firmware's C library; a build wanting a newer one fails to load.
const GLIBC_VERSION: (u32, u32) = (2, 38);

/// Routes a game's own SDL2 into the firmware's; see handheld/sdl-dynapi.c.
/// Deployed next to our binary.
const SDL_SHIM: &str = "libzitch-sdl.so";

/// Something in an install folder the firmware can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Rom {
        path: PathBuf,
        system: System,
    },
    /// A `.love` file, or a folder with `main.lua` at its root.
    Love {
        path: PathBuf,
    },
}

impl Content {
    /// What a file is, by name, if the firmware can run it.
    pub fn for_file(path: &Path) -> Option<Content> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if ext == "love" {
            return Some(Content::Love {
                path: path.to_path_buf(),
            });
        }
        System::for_file(path).map(|system| Content::Rom {
            path: path.to_path_buf(),
            system,
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Content::Rom { system, .. } => system.label(),
            Content::Love { .. } => "LÖVE",
        }
    }

    fn path(&self) -> &Path {
        match self {
            Content::Rom { path, .. } | Content::Love { path } => path,
        }
    }
}

/// Whether the firmware can run a file with this name. Used before an
/// install, when the name is all there is; once installed, butler's
/// targets decide ([`content_for`]).
pub fn runs_here(path: &Path) -> bool {
    Content::for_file(path).is_some()
}

/// The payload flavors the firmware has runtimes for, in dash's words,
/// for `Launch.GetTargets`.
pub fn runtimes() -> Vec<String> {
    let mut runtimes = vec!["love".to_string()];
    runtimes.extend(System::ALL.iter().map(|s| format!("rom:{}", s.dash_id())));
    runtimes
}

/// What a runtime target from butler is here. `None` for targets of
/// other strategies; an error for a payload of a kind the firmware was
/// told it runs but this one it cannot.
pub fn content_for(target: &LaunchTarget) -> Option<Result<Content, String>> {
    let strategy = target.strategy.as_ref()?;
    if strategy.strategy != LaunchStrategy::Runtime {
        return None;
    }
    let path = PathBuf::from(&strategy.full_target_path);
    let candidate = strategy.candidate.as_ref()?;
    let engine = candidate.engine.as_ref();
    Some(match candidate.flavor {
        Flavor::Love => {
            let version = engine.and_then(|e| e.version.as_deref()).unwrap_or("");
            if version.starts_with("0.") {
                Err(format!(
                    "made for LÖVE {version}; this device has LÖVE {LOVE_VERSION}"
                ))
            } else {
                Ok(Content::Love { path })
            }
        }
        Flavor::ROM => {
            let system = engine
                .filter(|e| e.engine == Engine::ROM)
                .and_then(|e| e.details.as_ref()?.get("system")?.as_str())
                .unwrap_or("");
            match System::from_dash(system) {
                Some(system) => Ok(Content::Rom { path, system }),
                None => Err(format!("no emulator for {system} on this device")),
            }
        }
        other => Err(format!("no runtime for {other:?} on this device")),
    })
}

/// Why a Linux build cannot run here, from what butler read out of its
/// executable, or `None` when nothing says it cannot. The screen is the
/// usual problem: only the firmware's SDL2 can open it, so a build must
/// link that, or bundle an SDL2 the shim can redirect there.
pub fn native_blocker(info: &LinuxInfo) -> Option<String> {
    if let Some(version) = info.glibc_version.as_deref()
        && let Some(needed) = parse_version(version)
        && needed > GLIBC_VERSION
    {
        return Some(format!(
            "needs glibc {version}; this device has {}.{}",
            GLIBC_VERSION.0, GLIBC_VERSION.1
        ));
    }
    let bundled = info.sdl_bundled.unwrap_or(false);
    let dynamic_api = info.sdl_dynamic_api.unwrap_or(false);
    match info.sdl.as_deref() {
        Some("2") if bundled && !dynamic_api => Some(
            "has its own SDL2 built in without the dynamic API, so it cannot reach this device's screen"
                .to_string(),
        ),
        Some("2") => None,
        Some(other) => Some(format!(
            "uses SDL{other}, which cannot reach this device's screen yet"
        )),
        None => {
            let display = info.display.as_deref().unwrap_or(&[]);
            if display.is_empty() {
                None
            } else {
                Some(format!(
                    "draws through {} rather than SDL2, which this device does not have",
                    display.join(", ")
                ))
            }
        }
    }
}

fn parse_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

/// An emulated system the firmware has a core for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Nes,
    Snes,
    GameBoy,
    GameBoyColor,
    GameBoyAdvance,
    MegaDrive,
    Commodore64,
    Amiga,
    Nintendo64,
}

impl System {
    const ALL: [System; 9] = [
        System::Nes,
        System::Snes,
        System::GameBoy,
        System::GameBoyColor,
        System::GameBoyAdvance,
        System::MegaDrive,
        System::Commodore64,
        System::Amiga,
        System::Nintendo64,
    ];

    /// dash's id for the system, as `Launch.GetTargets` names ROMs.
    fn dash_id(self) -> &'static str {
        match self {
            System::Nes => "nes",
            System::Snes => "snes",
            System::GameBoy => "gb",
            System::GameBoyColor => "gbc",
            System::GameBoyAdvance => "gba",
            System::MegaDrive => "md",
            System::Commodore64 => "c64",
            System::Amiga => "amiga",
            System::Nintendo64 => "n64",
        }
    }

    fn from_dash(id: &str) -> Option<System> {
        System::ALL.iter().copied().find(|s| s.dash_id() == id)
    }

    /// The system a file is a ROM for, by extension.
    pub fn for_file(path: &Path) -> Option<System> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "nes" | "unf" | "unif" => System::Nes,
            "sfc" | "smc" | "swc" => System::Snes,
            "gb" => System::GameBoy,
            "gbc" => System::GameBoyColor,
            "gba" => System::GameBoyAdvance,
            "md" | "gen" | "smd" => System::MegaDrive,
            "prg" | "d64" | "t64" | "crt" | "d81" => System::Commodore64,
            "adf" | "hdf" | "lha" => System::Amiga,
            "z64" | "n64" | "v64" => System::Nintendo64,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            System::Nes => "NES",
            System::Snes => "Super Nintendo",
            System::GameBoy => "Game Boy",
            System::GameBoyColor => "Game Boy Color",
            System::GameBoyAdvance => "Game Boy Advance",
            System::MegaDrive => "Mega Drive",
            System::Commodore64 => "Commodore 64",
            System::Amiga => "Amiga",
            System::Nintendo64 => "Nintendo 64",
        }
    }

    /// The firmware's name for the system (a folder under
    /// `/opt/muos/share/info/assign`), the launcher ini in it, and the
    /// libretro core that ini names. These are the `default=` cores of
    /// muOS 2601; a user who reassigned a system in the menu is not
    /// followed, since that assignment is per ROM folder.
    fn assignment(self) -> (&'static str, &'static str, &'static str) {
        match self {
            System::Nes => ("Nintendo NES - Famicom", "fceumm", "fceumm_libretro.so"),
            System::Snes => ("Nintendo SNES - SFC", "snes9x", "snes9x_libretro.so"),
            System::GameBoy => ("Nintendo Game Boy", "gambatte", "gambatte_libretro.so"),
            System::GameBoyColor => (
                "Nintendo Game Boy Color",
                "gambatte",
                "gambatte_libretro.so",
            ),
            System::GameBoyAdvance => ("Nintendo Game Boy Advance", "mgba", "mgba_libretro.so"),
            System::MegaDrive => (
                "Sega Mega Drive - Genesis",
                "genesis plus gx",
                "genesis_plus_gx_libretro.so",
            ),
            System::Commodore64 => ("Commodore C64", "vice x64 fast", "vice_x64_libretro.so"),
            System::Amiga => ("Commodore Amiga", "puae 2021", "puae2021_libretro.so"),
            System::Nintendo64 => (
                "Nintendo N64",
                "mupen64plus next",
                "mupen64plus_next_libretro.so",
            ),
        }
    }
}

/// Whether this is a muOS device: its launch script is present.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| Path::new(LAUNCH_SCRIPT).exists())
}

/// What the games butler launches need in their environment here: the
/// SDL shim, and a locale, which the firmware leaves unset and games
/// read without checking.
pub fn game_env() -> Vec<(String, OsString)> {
    if !available() {
        return Vec::new();
    }
    let mut env = Vec::new();
    let shim = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(SDL_SHIM)))
        .filter(|path| path.is_file());
    match shim {
        Some(shim) => env.push(("SDL_DYNAMIC_API".to_string(), shim.into_os_string())),
        None => log::warn!("{SDL_SHIM} is missing; Linux builds will not find the screen"),
    }
    if std::env::var_os("LANG").is_none() {
        env.push(("LANG".to_string(), "en_US.UTF-8".into()));
    }
    env
}

/// Runs the content and returns when it exits. `name` is what the
/// firmware shows in its overlays and history.
pub fn launch(name: &str, content: &Content) -> Result<()> {
    log::info!(
        "launching {} as {}",
        content.path().display(),
        content.label()
    );
    let result = match content {
        Content::Rom { path, system } => launch_rom(name, *system, path),
        Content::Love { path } => launch_love(path),
    };
    // RetroArch's launcher script names itself here and never puts the
    // app back.
    set_foreground("zitch");
    result
}

fn set_foreground(process: &str) {
    if let Err(error) = std::fs::write(FOREGROUND_PROCESS, process) {
        log::warn!("setting {FOREGROUND_PROCESS}: {error}");
    }
}

/// Runs a `.love` (or a folder) in the firmware's LÖVE. Its own SDL
/// window takes the screen, like RetroArch's. The panic combo gets its
/// pid, as there is no launcher script to name it.
fn launch_love(path: &Path) -> Result<()> {
    let dir = Path::new(LOVE_DIR);
    let mut child = Command::new(dir.join("love"))
        .arg(path)
        .current_dir(dir)
        .env("LD_LIBRARY_PATH", dir.join("libs"))
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .spawn()
        .with_context(|| format!("running {}", dir.join("love").display()))?;
    set_foreground(&child.id().to_string());
    let status = child
        .wait()
        .with_context(|| format!("waiting for {}", dir.join("love").display()))?;
    if !status.success() {
        bail!("love exited with {status}");
    }
    Ok(())
}

/// Runs `rom` in the firmware's emulator for `system`.
fn launch_rom(name: &str, system: System, rom: &Path) -> Result<()> {
    let (assign, launcher, core) = system.assignment();
    let dir = rom
        .parent()
        .and_then(Path::to_str)
        .context("ROM path is not a directory path")?;
    let file = rom
        .file_name()
        .and_then(|f| f.to_str())
        .context("ROM path has no file name")?;
    // launch.sh reads nine lines: name, core, system, two it ignores,
    // launcher, then the folder in two parts it joins, and the file.
    let rom_go = format!("{name}\n{core}\n{assign}\n\n\n{launcher}\n{dir}\n\n{file}\n");
    std::fs::write(ROM_GO, rom_go).with_context(|| format!("writing {ROM_GO}"))?;
    let governor = std::fs::read_to_string(DEFAULT_GOVERNOR).unwrap_or_else(|_| "ondemand".into());
    std::fs::write(GOV_GO, governor.trim()).with_context(|| format!("writing {GOV_GO}"))?;
    std::fs::write(FLT_GO, "").with_context(|| format!("writing {FLT_GO}"))?;
    // The app's own config and cache live next to its binary (see
    // mux_launch.sh); the emulator has to find the firmware's instead, or
    // it starts with no button mappings.
    let status = Command::new("/bin/sh")
        .arg(LAUNCH_SCRIPT)
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .status()
        .with_context(|| format!("running {LAUNCH_SCRIPT}"))?;
    // The script's status is that of its last housekeeping line (a test
    // for a paired Discord PC that usually fails), not the emulator's, so
    // it only tells us whether the script ran at all.
    match status.code() {
        Some(_) => {
            log::debug!("{LAUNCH_SCRIPT} exited with {status}");
            Ok(())
        }
        None => bail!("{LAUNCH_SCRIPT} was killed by a signal"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::butlerd::types::{Candidate, EngineInfo, StrategyResult};

    #[test]
    fn extension_picks_the_system() {
        assert_eq!(
            System::for_file(Path::new("Bobl 1.2.NES")),
            Some(System::Nes)
        );
        assert_eq!(
            System::for_file(Path::new("tobudx.gb")),
            Some(System::GameBoy)
        );
        assert_eq!(
            System::for_file(Path::new("a/b/game.gbc")),
            Some(System::GameBoyColor)
        );
        assert_eq!(System::for_file(Path::new("game.SFC")), Some(System::Snes));
        assert_eq!(System::for_file(Path::new("setup.exe")), None);
        assert_eq!(System::for_file(Path::new("README")), None);
    }

    #[test]
    fn love_by_file_name() {
        assert!(runs_here(Path::new("x-moon-love11.5.love")));
        assert!(!runs_here(Path::new("xmoon-win32.zip")));
    }

    fn runtime_target(flavor: Flavor, engine: Option<EngineInfo>, path: &str) -> LaunchTarget {
        LaunchTarget {
            action: None,
            host: Default::default(),
            strategy: Some(StrategyResult {
                strategy: LaunchStrategy::Runtime,
                full_target_path: path.to_string(),
                candidate: Some(Candidate {
                    flavor,
                    engine,
                    ..Default::default()
                }),
            }),
        }
    }

    fn rom_engine(system: &str) -> EngineInfo {
        EngineInfo {
            engine: Engine::ROM,
            version: None,
            details: Some(HashMap::from([(
                "system".to_string(),
                serde_json::Value::String(system.to_string()),
            )])),
        }
    }

    #[test]
    fn targets_become_content() {
        let gba = runtime_target(Flavor::ROM, Some(rom_engine("gba")), "/g/game.gba");
        assert_eq!(
            content_for(&gba),
            Some(Ok(Content::Rom {
                path: PathBuf::from("/g/game.gba"),
                system: System::GameBoyAdvance
            }))
        );
        let nds = runtime_target(Flavor::ROM, Some(rom_engine("nds")), "/g/game.nds");
        assert!(matches!(content_for(&nds), Some(Err(_))));

        let love = EngineInfo {
            engine: Engine::Love,
            version: Some("11.5".into()),
            details: None,
        };
        let new = runtime_target(Flavor::Love, Some(love.clone()), "/g/game.love");
        assert_eq!(
            content_for(&new),
            Some(Ok(Content::Love {
                path: PathBuf::from("/g/game.love")
            }))
        );
        let old = runtime_target(
            Flavor::Love,
            Some(EngineInfo {
                version: Some("0.8.0".into()),
                ..love
            }),
            "/g/old.love",
        );
        assert!(matches!(content_for(&old), Some(Err(_))));

        let mut native = runtime_target(Flavor::NativeLinux, None, "/g/bin");
        native.strategy.as_mut().unwrap().strategy = LaunchStrategy::Native;
        assert_eq!(content_for(&native), None);
    }

    #[test]
    fn native_builds_are_judged_by_their_sdl() {
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
    }
}
