//! spruceOS: detected by the environment its launch script sources,
//! so a spruce card mounted on a desktop is not one. Its emulators are
//! folders under `Emu/` on the card, each with a `config.json` naming
//! its launcher and the cores on offer; the launcher tells the system
//! from its own path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::*;

/// What spruce's `appEnv.sh` sets for third-party apps, which our
/// launch script sources.
const APP_ENV: &str = "/mnt/SDCARD/spruce/scripts/appEnv.sh";

/// The emulator folders.
const EMU_DIR: &str = "/mnt/SDCARD/Emu";

/// Where spruce's own launches have HOME.
const HOME: &str = "/mnt/SDCARD";

/// Where the user puts the official PICO-8; without it carts go to the
/// Fake-08 core.
const BIOS_DIR: &str = "/mnt/SDCARD/BIOS";
const PICO8_FILES: &[&str] = &["pico8_64", "pico8.dat"];

/// dash ids and their `Emu/` folders.
const FOLDERS: &[(&str, &str)] = &[
    ("nes", "FC"),
    ("snes", "SFC"),
    ("gb", "GB"),
    ("gbc", "GBC"),
    ("gba", "GBA"),
    ("nds", "NDS"),
    ("md", "MD"),
    ("32x", "THIRTYTWOX"),
    ("sms", "MS"),
    ("gg", "GG"),
    ("pce", "PCE"),
    ("lynx", "LYNX"),
    ("ngp", "NGP"),
    ("a26", "ATARI"),
    ("c64", "COMMODORE"),
    ("amiga", "AMIGA"),
    ("n64", "N64"),
    ("psx", "PS"),
    ("psp", "PSP"),
    ("saturn", "SATURN"),
    ("segacd", "SEGACD"),
    ("dreamcast", "DC"),
    (TIC80, "TIC"),
];

/// The parts of an emulator folder's `config.json` we read.
#[derive(serde::Deserialize)]
struct EmuConfig {
    label: String,
    /// The launcher, relative to the folder.
    launch: String,
    default_emulator: String,
    #[serde(rename = "menuOptions")]
    menu_options: Option<HashMap<String, Choice>>,
}

/// A menu option; the `Emulator*` ones pick a core.
#[derive(serde::Deserialize, Default)]
struct Choice {
    selected: Option<String>,
    #[serde(default)]
    devices: Vec<String>,
}

/// The board and its device names, as `launch.sh` exports them.
struct Board {
    platform: String,
    device_names: Vec<String>,
}

impl Board {
    fn from_env() -> Self {
        Board {
            platform: std::env::var("SPRUCE_PLATFORM").unwrap_or_default(),
            device_names: std::env::var("SPRUCE_DEVICE_NAMES")
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect(),
        }
    }
}

impl EmuConfig {
    /// The core the launcher will pick, as `standard_launch.sh` resolves
    /// it: the board's own option, then the `Emulator*` option whose
    /// device list names this board, then the 64-bit one, then the
    /// plain one, then the default.
    fn core(&self, board: &Board) -> &str {
        let options = self.menu_options.as_ref();
        let selected = |key: &str| {
            options?
                .get(key)?
                .selected
                .as_deref()
                .filter(|s| !s.is_empty())
        };
        let for_device = || {
            let mut keys: Vec<&String> = options?
                .iter()
                .filter(|(key, choice)| {
                    key.starts_with("Emulator")
                        && choice
                            .devices
                            .iter()
                            .any(|d| board.device_names.contains(d))
                })
                .map(|(key, _)| key)
                .collect();
            keys.sort();
            selected(keys.first()?)
        };
        selected(&format!("Emulator_{}", board.platform))
            .or_else(for_device)
            .or_else(|| selected("Emulator_64"))
            .or_else(|| selected("Emulator"))
            .unwrap_or(&self.default_emulator)
    }
}

fn system(id: &str, folder: &str, json: &str, board: &Board) -> Option<System> {
    let config: EmuConfig = serde_json::from_str(json).ok()?;
    let launcher = Path::new(&config.launch).file_name()?.to_str()?.to_string();
    Some(System {
        id: id.to_string(),
        name: folder.to_string(),
        launcher,
        core: config.core(board).to_string(),
        label: config.label,
    })
}

fn pico8_folder() -> &'static str {
    let bios = Path::new(BIOS_DIR);
    if PICO8_FILES.iter().all(|f| bios.join(f).is_file()) {
        "PICO8"
    } else {
        "FAKE08"
    }
}

fn resolve_systems() -> HashMap<String, System> {
    let board = Board::from_env();
    FOLDERS
        .iter()
        .copied()
        .chain([(PICO8, pico8_folder())])
        .filter_map(|(id, folder)| {
            let json = std::fs::read_to_string(Path::new(EMU_DIR).join(folder).join("config.json"))
                .ok()?;
            Some((id.to_string(), system(id, folder, &json, &board)?))
        })
        .collect()
}

/// The launcher's path through the folder, as the menu runs it: the
/// script reads the system from the `Emu/<NAME>/` in its own path.
fn launcher_path(system: &System) -> Option<PathBuf> {
    let folder = Path::new(EMU_DIR).join(&system.name);
    let json = std::fs::read_to_string(folder.join("config.json")).ok()?;
    let config: EmuConfig = serde_json::from_str(&json).ok()?;
    Some(folder.join(config.launch))
}

fn launch_rom(system: &System, rom: &Path, env: &HashMap<String, String>) -> Result<()> {
    let script = launcher_path(system)
        .filter(|script| script.is_file())
        .with_context(|| format!("spruce has no launcher for {}", system.label))?;
    // The emulators keep their config under spruce's HOME, not ours,
    // and take the pad as the menu gives it, not through our map.
    let mut command = Command::new("/bin/sh");
    command
        .arg(&script)
        .arg(rom)
        .env("HOME", HOME)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("SDL_GAMECONTROLLERCONFIG")
        .envs(env);
    let status = spawn_launcher(&mut command)
        .with_context(|| format!("running {}", script.display()))?
        .wait()
        .with_context(|| format!("waiting for {}", script.display()))?;
    // The script's status is its last housekeeping line's, so it only
    // says whether the script ran.
    match status.code() {
        Some(_) => {
            log::debug!("{} exited with {status}", script.display());
            Ok(())
        }
        None => bail!("{} was killed by a signal", script.display()),
    }
}

pub(super) struct Spruce;

static SPRUCE: Spruce = Spruce;

pub(super) fn detect() -> Option<&'static dyn Firmware> {
    let sourced = std::env::var_os("CFW").is_some_and(|cfw| cfw == "SPRUCE");
    (sourced && Path::new(APP_ENV).is_file()).then_some(&SPRUCE as &dyn Firmware)
}

impl Firmware for Spruce {
    fn name(&self) -> &'static str {
        "spruce"
    }

    /// `appEnv.sh` names the model, e.g. `BrickPro`.
    fn details(&self) -> Vec<(&'static str, String)> {
        std::env::var("SPRUCE_PLATFORM")
            .ok()
            .map(|board| ("board", board))
            .into_iter()
            .collect()
    }

    fn systems(&self) -> HashMap<String, System> {
        resolve_systems()
    }

    fn launch_rom(
        &self,
        _name: &str,
        system: &System,
        rom: &Path,
        env: &HashMap<String, String>,
    ) -> Result<()> {
        launch_rom(system, rom, env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brick_pro() -> Board {
        Board {
            platform: "BrickPro".into(),
            device_names: vec!["TRIMUI_BRICK_PRO".into(), "TRIMUI_A133P".into()],
        }
    }

    const GB: &str = r#"{
      "label": "GB",
      "launch": "../../spruce/scripts/emu/standard_launch.sh",
      "menuOptions": {
        "Governor": {"options": ["Smart"], "selected": "Smart"},
        "Emulator": {"options": ["gambatte", "mgba"], "selected": "mgba"}
      },
      "default_emulator": "gambatte"
    }"#;

    const NDS: &str = r#"{
      "label": "NDS",
      "launch": "../../spruce/scripts/emu/standard_launch.sh",
      "menuOptions": {
        "Emulator_Flip": {"devices": ["MIYOO_FLIP"], "selected": "DraStic-trngaje"},
        "Emulator_SmartPro": {"devices": ["TRIMUI_SMART_PRO", "TRIMUI_BRICK_PRO"], "selected": "DSperate"}
      },
      "default_emulator": "DraStic-original"
    }"#;

    const PSP: &str = r#"{
      "label": "PSP",
      "launch": "../../spruce/scripts/emu/standard_launch.sh",
      "menuOptions": null,
      "default_emulator": "PPSSPP-SA"
    }"#;

    #[test]
    fn chosen_core_wins_over_default() {
        let system = system("gb", "GB", GB, &brick_pro()).unwrap();
        assert_eq!(system.core, "mgba");
        assert_eq!(system.label, "GB");
        assert_eq!(system.name, "GB");
        assert_eq!(system.launcher, "standard_launch.sh");
    }

    #[test]
    fn option_naming_the_device_wins() {
        let system = system("nds", "NDS", NDS, &brick_pro()).unwrap();
        assert_eq!(system.core, "DSperate");
    }

    #[test]
    fn board_option_wins_over_device_list() {
        let board = Board {
            platform: "Flip".into(),
            device_names: vec!["MIYOO_FLIP".into()],
        };
        let system = system("nds", "NDS", NDS, &board).unwrap();
        assert_eq!(system.core, "DraStic-trngaje");
    }

    #[test]
    fn default_core_without_options() {
        let system = system("psp", "PSP", PSP, &brick_pro()).unwrap();
        assert_eq!(system.core, "PPSSPP-SA");
    }

    #[test]
    fn unreadable_config_is_no_system() {
        assert!(system("gb", "GB", "{", &brick_pro()).is_none());
    }
}
