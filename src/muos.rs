//! Games on muOS. The handheld firmware ships RetroArch with a core for
//! each system it knows, and a LÖVE runtime; a game there is a ROM or a
//! `.love`, not a Linux binary. This module tells them apart by file name
//! and runs them the way the firmware's own menu does: a ROM through its
//! launch script and RetroArch, a `.love` through its LÖVE binary.
//!
//! Nothing here is compiled out on other systems; [`available`] is a
//! runtime check for the firmware's script, so the desktop build simply
//! never takes these paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

const LAUNCH_SCRIPT: &str = "/opt/muos/script/mux/launch.sh";
/// What the frontend writes before running the launch script: the
/// content to run, the CPU governor while it runs, and a colour filter.
const ROM_GO: &str = "/tmp/rom_go";
const GOV_GO: &str = "/tmp/gov_go";
const FLT_GO: &str = "/tmp/flt_go";
/// The governor the firmware goes back to after content.
const DEFAULT_GOVERNOR: &str = "/opt/muos/device/config/cpu/default";
/// How deep to look inside an install folder.
const SEARCH_DEPTH: usize = 3;
/// What the firmware's R2+Select+B panic combo kills (`proc_die.sh`).
/// The app launcher set it to zitch; while a game has the screen it must
/// name the game, or the combo kills zitch under it and orphans the game.
/// A name is looked up with busybox `pgrep -x`, which matches the whole
/// `argv[0]`, so a process started by path only matches by pid.
const FOREGROUND_PROCESS: &str = "/opt/muos/config/system/foreground_process";
/// The firmware's LÖVE 11.5, shipped for its Moonlight client. The binary
/// links `libs/liblove-11.5.so` and the system SDL2.
const LOVE_DIR: &str = "/opt/muos/share/application/Moonlight";

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

/// Whether the firmware can run a file with this name.
pub fn runs_here(path: &Path) -> bool {
    Content::for_file(path).is_some()
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

/// The first thing in an install folder the firmware can run.
pub fn find_content(folder: &Path) -> Option<Content> {
    fn walk(dir: &Path, depth: usize) -> Option<Content> {
        if dir.join("main.lua").is_file() {
            return Some(Content::Love {
                path: dir.to_path_buf(),
            });
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in &entries {
            let path = entry.path();
            if path.is_file()
                && let Some(content) = Content::for_file(&path)
            {
                return Some(content);
            }
        }
        if depth == 0 {
            return None;
        }
        entries
            .iter()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .find_map(|p| walk(&p, depth - 1))
    }
    walk(folder, SEARCH_DEPTH)
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
    use super::*;

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
    fn finds_a_rom_below_the_folder() {
        let dir = std::env::temp_dir().join(format!("zitch-rom-{}", std::process::id()));
        let nested = dir.join("game").join("rom");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("manual.pdf"), b"").unwrap();
        std::fs::write(nested.join("game.gba"), b"").unwrap();
        assert_eq!(
            find_content(&dir),
            Some(Content::Rom {
                path: nested.join("game.gba"),
                system: System::GameBoyAdvance
            })
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn love_by_file_or_by_folder() {
        assert!(runs_here(Path::new("x-moon-love11.5.love")));
        assert!(!runs_here(Path::new("xmoon-win32.zip")));
        let dir = std::env::temp_dir().join(format!("zitch-love-{}", std::process::id()));
        let game = dir.join("game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("main.lua"), b"").unwrap();
        assert_eq!(find_content(&dir), Some(Content::Love { path: game }));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
