//! What the firmware is and has: its paths on both releases, the board,
//! and the systems its RetroArch cores and launch scripts cover.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::*;

pub(super) const LAUNCH_SCRIPT: &str = "/opt/muos/script/mux/launch.sh";
/// Where Andromeda keeps the launch handoff files; Jacaranda uses /tmp.
pub(super) const RUN_DIR: &str = "/run/muos";
/// The hardware model, e.g. `rg35xx-h` or `tui-brick-pro`.
pub(super) const BOARD_NAME: &str = "/opt/muos/device/config/board/name";
/// The governor the firmware goes back to after content.
pub(super) const DEFAULT_GOVERNOR: &str = "/opt/muos/device/config/cpu/default";
/// The firmware's systems on Jacaranda: `assign.json` maps ids to a
/// folder per system, each with a `global.ini` naming its default
/// launcher and an ini per launcher naming the core. The launch script
/// reads the launcher named in rom_go from here.
pub(super) const ASSIGN_DIR: &str = "/opt/muos/share/info/assign";
/// The same on Andromeda: `assign.json` maps ids to a system name, and
/// `libretro.json` and `external.json` hold each system's cores under
/// that name, with `default` naming one. The user copy wins, as it does
/// in the launch script. The launcher the script wants is the core's key
/// with a prefix for its runtime: `mu-` sends a libretro core to Pickles,
/// `ext-` an external one to its own launcher, and no prefix means
/// RetroArch, which the menu never picks.
pub(super) const MANIFEST_DIR: &str = "/opt/muos/share/info/manifest";
pub(super) const USER_MANIFEST_DIR: &str = "/run/muos/storage/info/manifest";
pub(super) const CORE_DIR: &str = "/opt/muos/share/core";
/// What the firmware's R2+Select+B panic combo kills (`proc_die.sh`).
/// The app launcher set it to zitch; while a game has the screen it must
/// name the game, or the combo kills zitch under it and orphans the game.
/// A name is looked up with `pgrep` (`-x` on Jacaranda, `-f` on
/// Andromeda), so a process started by path only matches by pid.
pub(super) const FOREGROUND_PROCESS: &str = "/opt/muos/config/system/foreground_process";
/// Where the firmware keeps its apps. An app installed to the SD card
/// lands under the second.
pub(super) const APPLICATION_DIRS: [&str; 2] = [
    "/opt/muos/share/application",
    "/run/muos/storage/application",
];
/// What Jacaranda ships, assumed when the host's cannot be read.
pub(super) const FALLBACK_GLIBC_VERSION: (u32, u32) = (2, 38);

/// dash's ROM system ids: what `Launch.GetTargets` runtimes take and
/// what a ROM payload's `engine.details["system"]` holds.
pub(super) const ROM_IDS: [&str; 24] = [
    "nes",
    "snes",
    "gb",
    "gbc",
    "gba",
    "nds",
    "3ds",
    "md",
    "32x",
    "sms",
    "gg",
    "pce",
    "lynx",
    "ngp",
    "a26",
    "c64",
    "amiga",
    "n64",
    "psx",
    "ps2",
    "psp",
    "saturn",
    "segacd",
    "dreamcast",
];
/// dash's disc formats, and the systems it reads out of their headers.
/// `.bin` is not one: it needs its `.cue`, and alone it matches far too much.
pub(super) const DISC_EXTS: [&str; 3] = ["cue", "iso", "chd"];
pub(super) const DISC_IDS: [&str; 6] = ["psx", "ps2", "psp", "saturn", "segacd", "dreamcast"];
/// Fantasy consoles: dash names their carts as payloads of their own,
/// the firmware assigns them like any system.
pub(super) const PICO8: &str = "pico8";
pub(super) const TIC80: &str = "tic80";

/// dash ids that `assign.json` spells differently.
pub(super) fn assign_key(id: &str) -> &str {
    match id {
        "a26" => "a2600",
        other => other,
    }
}

/// An emulated system this firmware has an emulator for, from its assign
/// metadata. A user who reassigned a system in the menu is not followed,
/// since that assignment is per ROM folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct System {
    /// dash's id, e.g. `gba`.
    pub id: String,
    /// The firmware's name for the system: the folder under
    /// [`ASSIGN_DIR`], or the key in the manifests.
    pub(super) assign: String,
    /// The default launcher: the launcher ini's stem, or the core's key
    /// in the manifest.
    pub(super) launcher: String,
    /// The core file the launcher loads.
    pub(super) core: String,
    /// The system's display name.
    pub(super) label: String,
}

impl System {
    /// The system dash's `id` names, if this firmware has one.
    pub fn for_id(id: &str) -> Option<&'static System> {
        systems().get(id)
    }

    /// The system a file is a ROM for, by extension, if it runs here.
    pub fn for_file(path: &Path) -> Option<&'static System> {
        System::for_id(id_for_file(path)?)
    }
}

pub(super) fn manifests_here() -> bool {
    Path::new(MANIFEST_DIR).join("libretro.json").is_file()
}

/// Every system that resolves here, by dash id. Read once.
pub(super) fn systems() -> &'static HashMap<String, System> {
    #[cfg(test)]
    if let Some(fake) = super::fake::firmware() {
        return fake.systems;
    }
    static SYSTEMS: OnceLock<HashMap<String, System>> = OnceLock::new();
    SYSTEMS.get_or_init(|| {
        let ids = ROM_IDS.iter().chain(&[PICO8, TIC80]);
        if manifests_here() {
            let read = |name: &str| {
                [USER_MANIFEST_DIR, MANIFEST_DIR]
                    .iter()
                    .find_map(|dir| std::fs::read_to_string(Path::new(dir).join(name)).ok())
            };
            let aliases = read("assign.json")
                .map(|json| parse_assign(&json))
                .unwrap_or_default();
            let manifests: Vec<(&str, Manifest)> =
                [("mu-", "libretro.json"), ("ext-", "external.json")]
                    .iter()
                    .filter_map(|(prefix, name)| {
                        Some((*prefix, parse_manifest(name, &read(name)?)?))
                    })
                    .collect();
            let board = board();
            ids.filter_map(|id| {
                let system = resolve_manifest(id, &aliases, &manifests, board.as_deref())?;
                Some((id.to_string(), system))
            })
            .collect()
        } else {
            let dir = Path::new(ASSIGN_DIR);
            let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).ok();
            let aliases = read("assign.json")
                .map(|json| parse_assign(&json))
                .unwrap_or_default();
            ids.filter_map(|id| Some((id.to_string(), resolve(id, &aliases, read)?)))
                .collect()
        }
    })
}

/// Systems by name, each with its cores.
type Manifest = HashMap<String, ManifestSystem>;

#[derive(serde::Deserialize)]
pub(super) struct ManifestSystem {
    default: String,
    cores: HashMap<String, ManifestCore>,
}

#[derive(serde::Deserialize)]
pub(super) struct ManifestCore {
    core: String,
    #[serde(default)]
    require: Require,
}

/// What a core needs to be offered at all.
#[derive(Default, serde::Deserialize)]
pub(super) struct Require {
    /// The boards it runs on; any when empty.
    #[serde(default)]
    device: Vec<String>,
}

pub(super) fn parse_manifest(name: &str, json: &str) -> Option<Manifest> {
    match serde_json::from_str(json) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            log::warn!("{name}: {error}");
            None
        }
    }
}

/// Resolves a dash id through `assign.json` and the first manifest that
/// lists the system, the way the launch script looks a core up. Each
/// manifest comes with its runtime prefix.
pub(super) fn resolve_manifest(
    id: &str,
    aliases: &HashMap<String, String>,
    manifests: &[(&str, Manifest)],
    board: Option<&str>,
) -> Option<System> {
    let assign = aliases.get(assign_key(id))?;
    let (prefix, system) = manifests
        .iter()
        .find_map(|(prefix, m)| Some((prefix, m.get(assign)?)))?;
    let Some(core) = system.cores.get(&system.default) else {
        log::warn!("{assign}: default core {} is not listed", system.default);
        return None;
    };
    let devices = &core.require.device;
    if !devices.is_empty() && !board.is_some_and(|b| devices.iter().any(|d| d == b)) {
        log::info!(
            "{assign}: {} only runs on {}",
            system.default,
            devices.join(", ")
        );
        return None;
    }
    Some(System {
        id: id.to_string(),
        assign: assign.to_string(),
        launcher: format!("{prefix}{}", system.default),
        core: core.core.clone(),
        label: assign.to_string(),
    })
}

/// `assign.json`: aliases to assign folder names.
pub(super) fn parse_assign(json: &str) -> HashMap<String, String> {
    match serde_json::from_str::<HashMap<String, serde_json::Value>>(json) {
        Ok(map) => map
            .into_iter()
            .filter_map(|(k, v)| Some((k, v.as_str()?.to_string())))
            .collect(),
        Err(error) => {
            log::warn!("assign.json: {error}");
            HashMap::new()
        }
    }
}

/// `key=` under `[section]`; bare lines are skipped.
pub(super) fn ini_value<'a>(ini: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut in_section = false;
    for line in ini.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name.trim() == section;
        } else if in_section
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim());
        }
    }
    None
}

/// Resolves a dash id through `assign.json`, `global.ini` and the default
/// launcher's ini. `read` takes a path relative to the assign folder.
pub(super) fn resolve(
    id: &str,
    aliases: &HashMap<String, String>,
    read: impl Fn(&str) -> Option<String>,
) -> Option<System> {
    let assign = aliases.get(assign_key(id))?;
    let global = read(&format!("{assign}/global.ini"))?;
    let Some(launcher) = ini_value(&global, "global", "default") else {
        log::warn!("{assign}/global.ini names no default launcher");
        return None;
    };
    let Some(ini) = read(&format!("{assign}/{launcher}.ini")) else {
        log::warn!("{assign}/{launcher}.ini is missing");
        return None;
    };
    let Some(core) = ini_value(&ini, launcher, "core") else {
        log::warn!("{assign}/{launcher}.ini names no core");
        return None;
    };
    let label = ini_value(&global, "global", "name").unwrap_or(assign);
    Some(System {
        id: id.to_string(),
        assign: assign.to_string(),
        launcher: launcher.to_string(),
        core: core.to_string(),
        label: label.to_string(),
    })
}

/// The dash id a file's name says it is a ROM for. Only extensions dash
/// takes on the name alone; `.bin`, `.cue`, `.iso` and `.chd` need a
/// header and would match too much here.
pub(super) fn id_for_file(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name.ends_with(".p8.png") {
        return Some(PICO8);
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "nes" => "nes",
        "sfc" | "smc" => "snes",
        "gb" => "gb",
        "gbc" => "gbc",
        "gba" => "gba",
        "md" | "gen" => "md",
        "32x" => "32x",
        "sms" => "sms",
        "gg" => "gg",
        "pce" => "pce",
        "lnx" => "lynx",
        "ngp" | "ngc" => "ngp",
        "a26" => "a26",
        "d64" | "prg" | "t64" => "c64",
        "adf" => "amiga",
        "z64" | "n64" | "v64" => "n64",
        "p8" => PICO8,
        "tic" => TIC80,
        _ => return None,
    })
}

/// Whether this is a muOS device: its launch script is present.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| Path::new(LAUNCH_SCRIPT).exists())
}

/// What the frontend writes before running the launch script: the
/// content to run, the CPU governor while it runs, and a colour filter.
pub(super) struct Handoff {
    pub(super) rom: PathBuf,
    pub(super) governor: PathBuf,
    pub(super) filter: PathBuf,
}

/// The firmware releases whose launch handoff differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    Jacaranda,
    Andromeda,
}

impl Release {
    pub fn name(self) -> &'static str {
        match self {
            Release::Jacaranda => "jacaranda",
            Release::Andromeda => "andromeda",
        }
    }
}

/// Which release this is. /run/muos exists on Jacaranda too, so the
/// directory says nothing; the launch script names its own files, and
/// only Jacaranda's spells out /tmp/rom_go.
pub fn release() -> Release {
    static RELEASE: OnceLock<Release> = OnceLock::new();
    *RELEASE.get_or_init(|| {
        let script = std::fs::read_to_string(LAUNCH_SCRIPT).unwrap_or_default();
        if script.contains("/tmp/rom_go") {
            Release::Jacaranda
        } else {
            Release::Andromeda
        }
    })
}

/// The hardware model the firmware was built for, e.g. `rg35xx-h`.
pub fn board() -> Option<String> {
    let name = std::fs::read_to_string(BOARD_NAME).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Which names this firmware uses for the launch handoff.
pub(super) fn handoff() -> &'static Handoff {
    static HANDOFF: OnceLock<Handoff> = OnceLock::new();
    HANDOFF.get_or_init(|| match release() {
        Release::Jacaranda => Handoff {
            rom: PathBuf::from("/tmp/rom_go"),
            governor: PathBuf::from("/tmp/gov_go"),
            filter: PathBuf::from("/tmp/flt_go"),
        },
        Release::Andromeda => {
            let run = Path::new(RUN_DIR);
            Handoff {
                rom: run.join("content"),
                governor: run.join("governor"),
                filter: run.join("filter"),
            }
        }
    })
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
