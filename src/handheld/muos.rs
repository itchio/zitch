//! muOS: detected by its launch script, which also runs ROMs the way
//! its menu does; its RetroArch cores and launch scripts per system,
//! read from its assign metadata on Jacaranda and its manifests on
//! Andromeda; and its panic combo, which kills the process named in a
//! file.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

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
/// Where the Archive Manager looks for a package to install.
const ARCHIVE_DIR: &str = "/mnt/mmc/ARCHIVE";

/// dash ids that `assign.json` spells differently.
pub(super) fn assign_key(id: &str) -> &str {
    match id {
        "a26" => "a2600",
        other => other,
    }
}

pub(super) fn manifests_here() -> bool {
    Path::new(MANIFEST_DIR).join("libretro.json").is_file()
}

/// Every system that resolves here, by dash id.
pub(super) fn resolve_systems() -> HashMap<String, System> {
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
                .filter_map(|(prefix, name)| Some((*prefix, parse_manifest(name, &read(name)?)?)))
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
        name: assign.to_string(),
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
        name: assign.to_string(),
        launcher: launcher.to_string(),
        core: core.to_string(),
        label: label.to_string(),
    })
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

/// Runs `rom` in the firmware's emulator for `system`.
fn launch_rom(
    name: &str,
    system: &System,
    rom: &Path,
    env: &HashMap<String, String>,
) -> Result<()> {
    let System {
        name: assign,
        launcher,
        core,
        ..
    } = system;
    // Without the core or the ini the launch script exits at once with
    // the reason only in the log.
    if manifests_here() {
        let so = Path::new(CORE_DIR).join(core);
        if core.ends_with(".so") && !so.is_file() {
            bail!(
                "This muOS has no {launcher} emulator for {assign}: {} is missing",
                so.display()
            );
        }
    } else {
        let ini = Path::new(ASSIGN_DIR)
            .join(assign)
            .join(format!("{launcher}.ini"));
        if !ini.is_file() {
            bail!(
                "This muOS has no {launcher} emulator for {assign}: it needs {core} in \
                 {CORE_DIR} and {}",
                ini.display()
            );
        }
    }
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
    let files = handoff();
    std::fs::write(&files.rom, rom_go)
        .with_context(|| format!("writing {}", files.rom.display()))?;
    let governor = std::fs::read_to_string(DEFAULT_GOVERNOR).unwrap_or_else(|_| "ondemand".into());
    std::fs::write(&files.governor, governor.trim())
        .with_context(|| format!("writing {}", files.governor.display()))?;
    std::fs::write(&files.filter, "")
        .with_context(|| format!("writing {}", files.filter.display()))?;
    // The app's own config and cache live next to its binary (see
    // mux_launch.sh); the emulator has to find the firmware's instead, or
    // it starts with no button mappings.
    let mut command = Command::new("/bin/sh");
    command
        .arg(LAUNCH_SCRIPT)
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .envs(env);
    let status = spawn_launcher(&mut command)
        .with_context(|| format!("running {LAUNCH_SCRIPT}"))?
        .wait()
        .with_context(|| format!("waiting for {LAUNCH_SCRIPT}"))?;
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

fn set_foreground(process: &str) {
    if let Err(error) = std::fs::write(FOREGROUND_PROCESS, process) {
        log::warn!("setting {FOREGROUND_PROCESS}: {error}");
    }
}

pub(super) struct Muos;

static MUOS: Muos = Muos;

/// muOS, when its launch script is here.
pub(super) fn detect() -> Option<&'static dyn Firmware> {
    Path::new(LAUNCH_SCRIPT)
        .exists()
        .then_some(&MUOS as &dyn Firmware)
}

impl Firmware for Muos {
    fn name(&self) -> &'static str {
        "muos"
    }

    fn details(&self) -> Vec<(&'static str, String)> {
        let mut details = Vec::new();
        if let Some(board) = board() {
            details.push(("board", board));
        }
        details.push(("release", release().name().to_string()));
        details
    }

    fn systems(&self) -> HashMap<String, System> {
        resolve_systems()
    }

    fn launch_rom(
        &self,
        name: &str,
        system: &System,
        rom: &Path,
        env: &HashMap<String, String>,
    ) -> Result<()> {
        launch_rom(name, system, rom, env)
    }

    /// muOS ships no LÖVE of its own: what is there belongs to whichever
    /// bundled app happens to be written in LÖVE, and those come and go
    /// between releases.
    fn love_candidates(&self) -> Vec<PathBuf> {
        APPLICATION_DIRS
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .filter_map(|entry| entry.ok())
            .flat_map(|app| {
                let app = app.path();
                [app.join("love"), app.join(".game/bin/love")]
            })
            .collect()
    }

    /// The frontend's own library path comes with the launch, and its
    /// gl4es crashes.
    fn keeps_library_dir(&self, dir: &Path) -> bool {
        dir != Path::new("/usr/lib/gl4es") && !dir.starts_with("/opt/muos/frontend")
    }

    /// The frontend's cursor preload comes with the launch too, and
    /// libmucursor's SDL_Init override fails a game whose SDL lives in a
    /// bundled shared library.
    fn game_env(&self, env: &mut Vec<(String, OsString)>) {
        env.push(("LD_PRELOAD".to_string(), OsString::new()));
    }

    /// Names the game's process to the panic combo.
    fn game_started(&self, pid: u32) {
        set_foreground(&pid.to_string());
    }

    /// Names zitch again. RetroArch's launcher script names itself and
    /// never puts the app back. The name is the binary's, which the port
    /// build spells `zitch.aarch64`.
    fn game_ended(&self) {
        let name = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.file_name()?.to_str().map(str::to_string))
            .unwrap_or_else(|| "zitch".to_string());
        set_foreground(&name);
    }

    fn update_archive_dir(&self) -> Option<PathBuf> {
        Some(PathBuf::from(ARCHIVE_DIR))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

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
                name: "Nintendo NES - Famicom".into(),
                launcher: "mu-fceumm".into(),
                core: "fceumm_libretro.so".into(),
                label: "Nintendo NES - Famicom".into(),
            })
        );
        // a26 is a2600 in assign.json.
        let a26 = lookup("a26").unwrap();
        assert_eq!(a26.id, "a26");
        assert_eq!(a26.name, "Atari 2600");
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
                name: "Nintendo Game Boy Advance".into(),
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
}
