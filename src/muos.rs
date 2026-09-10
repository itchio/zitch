//! ROMs on muOS. The handheld firmware ships RetroArch and a core for
//! each system it knows; a game there is an upload whose file is a ROM,
//! not a Linux binary. This module tells the two apart by file name and
//! runs a ROM the way the firmware's own menu does: write the launch
//! files it reads, run its launch script, wait for the emulator to exit.
//!
//! Nothing here is compiled out on other systems; [`available`] is a
//! runtime check for the firmware's script, so the desktop build simply
//! never takes these paths.

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
/// How deep to look for a ROM inside an install folder.
const SEARCH_DEPTH: usize = 3;

/// An emulated system the firmware has a core for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Nes,
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

/// The first ROM in an install folder, if any file in it is one.
pub fn find_rom(folder: &Path) -> Option<(PathBuf, System)> {
    fn walk(dir: &Path, depth: usize) -> Option<(PathBuf, System)> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in &entries {
            let path = entry.path();
            if path.is_file()
                && let Some(system) = System::for_file(&path)
            {
                return Some((path, system));
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

/// Runs `rom` in the firmware's emulator for `system` and returns when it
/// exits. `name` is what the firmware shows in its overlays and history.
pub fn launch(name: &str, system: System, rom: &Path) -> Result<()> {
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
    log::info!(
        "launching {} as {} through {LAUNCH_SCRIPT}",
        rom.display(),
        system.label()
    );
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
        let found = find_rom(&dir).unwrap();
        assert_eq!(found.1, System::GameBoyAdvance);
        assert_eq!(found.0, nested.join("game.gba"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
