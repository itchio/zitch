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

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};

use crate::butlerd::types::{Arch, Candidate, Engine, Flavor, LinuxInfo};

const LAUNCH_SCRIPT: &str = "/opt/muos/script/mux/launch.sh";
/// Where Andromeda keeps the launch handoff files; Jacaranda uses /tmp.
const RUN_DIR: &str = "/run/muos";
/// The hardware model, e.g. `rg35xx-h` or `tui-brick-pro`.
const BOARD_NAME: &str = "/opt/muos/device/config/board/name";
/// The governor the firmware goes back to after content.
const DEFAULT_GOVERNOR: &str = "/opt/muos/device/config/cpu/default";
/// The firmware's systems on Jacaranda: `assign.json` maps ids to a
/// folder per system, each with a `global.ini` naming its default
/// launcher and an ini per launcher naming the core. The launch script
/// reads the launcher named in rom_go from here.
const ASSIGN_DIR: &str = "/opt/muos/share/info/assign";
/// The same on Andromeda: `assign.json` maps ids to a system name, and
/// `libretro.json` and `external.json` hold each system's cores under
/// that name, with `default` naming one. The user copy wins, as it does
/// in the launch script. The launcher the script wants is the core's key
/// with a prefix for its runtime: `mu-` sends a libretro core to Pickles,
/// `ext-` an external one to its own launcher, and no prefix means
/// RetroArch, which the menu never picks.
const MANIFEST_DIR: &str = "/opt/muos/share/info/manifest";
const USER_MANIFEST_DIR: &str = "/run/muos/storage/info/manifest";
const CORE_DIR: &str = "/opt/muos/share/core";
/// What the firmware's R2+Select+B panic combo kills (`proc_die.sh`).
/// The app launcher set it to zitch; while a game has the screen it must
/// name the game, or the combo kills zitch under it and orphans the game.
/// A name is looked up with `pgrep` (`-x` on Jacaranda, `-f` on
/// Andromeda), so a process started by path only matches by pid.
const FOREGROUND_PROCESS: &str = "/opt/muos/config/system/foreground_process";
/// Where the firmware keeps its apps. An app installed to the SD card
/// lands under the second.
const APPLICATION_DIRS: [&str; 2] = [
    "/opt/muos/share/application",
    "/run/muos/storage/application",
];
/// What Jacaranda ships, assumed when the host's cannot be read.
const FALLBACK_GLIBC_VERSION: (u32, u32) = (2, 38);

/// Why a game cannot run on the LÖVE that is here. The 0.x series is a
/// different API, so 11.x will not load those at all.
fn love_blocker(wanted: &str, have: &str) -> Option<String> {
    wanted
        .starts_with("0.")
        .then(|| format!("made for LÖVE {wanted}; this device has LÖVE {have}"))
}

struct Love {
    binary: PathBuf,
    libs: PathBuf,
    version: String,
}

/// Routes a game's own SDL2 into the firmware's; see handheld/sdl-dynapi.c.
/// Deployed next to our binary.
const SDL_SHIM: &str = "libzitch-sdl.so";

/// Something in an install folder the firmware can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// A file RetroArch plays with the system's core; fantasy console
    /// carts count.
    Rom {
        path: PathBuf,
        system: &'static System,
    },
    /// A `.love` file, or a folder with `main.lua` at its root.
    Love { path: PathBuf },
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

    pub fn label(&self) -> &str {
        match self {
            Content::Rom { system, .. } => &system.label,
            Content::Love { .. } => "LÖVE",
        }
    }

    fn path(&self) -> &Path {
        match self {
            Content::Rom { path, .. } | Content::Love { path } => path,
        }
    }
}

/// Ours, deployed next to the binary by handheld-love, else one the
/// firmware carries. muOS ships no LÖVE of its own: what is there
/// belongs to whichever bundled app happens to be written in LÖVE, and
/// those come and go between releases.
fn love() -> Option<&'static Love> {
    static LOVE: OnceLock<Option<Love>> = OnceLock::new();
    LOVE.get_or_init(|| {
        let ours = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("love")));
        let apps = APPLICATION_DIRS
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .filter_map(|entry| entry.ok());
        ours.into_iter()
            .chain(apps.flat_map(|app| {
                let app = app.path();
                [app.join("love"), app.join(".game/bin/love")]
            }))
            .find_map(love_at)
    })
    .as_ref()
}

/// The runtime around a `love` binary, if it is one: `liblove` sits in a
/// sibling directory, named for the architecture on newer builds.
fn love_at(binary: PathBuf) -> Option<Love> {
    if !binary.is_file() {
        return None;
    }
    let parent = binary.parent()?;
    ["libs", "libs.aarch64"]
        .iter()
        .map(|name| parent.join(name))
        .find_map(|libs| {
            let version = liblove_version(&libs)?;
            Some(Love {
                binary: binary.clone(),
                libs,
                version,
            })
        })
}

/// The version out of `liblove-<version>.so`, which is how the firmware
/// names it and the only place the runtime says what it is.
fn liblove_version(libs: &Path) -> Option<String> {
    std::fs::read_dir(libs)
        .ok()?
        .filter_map(|e| e.ok())
        .find_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = name.strip_prefix("liblove-")?.strip_suffix(".so")?;
            Some(version.to_string())
        })
}

/// Whether the firmware can run a file with this name. Used before an
/// install, when the name is all there is; once installed, butler's
/// targets decide ([`content_for`]).
pub fn runs_here(path: &Path) -> bool {
    Content::for_file(path).is_some() || disc_runs_here(path)
}

/// A disc image names its system in its header, which butler only reads
/// once the game is installed, so before that any disc system being here
/// has to do.
fn disc_runs_here(path: &Path) -> bool {
    is_disc_image(path) && DISC_IDS.iter().any(|id| System::for_id(id).is_some())
}

fn is_disc_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| DISC_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The payload flavors the firmware has runtimes for, in dash's words,
/// for `Launch.GetTargets`.
pub fn runtimes() -> Vec<String> {
    let mut runtimes = Vec::new();
    if love().is_some() {
        runtimes.push("love".to_string());
    }
    if System::for_id(PICO8).is_some() {
        runtimes.push("pico8-cart".to_string());
    }
    if System::for_id(TIC80).is_some() {
        runtimes.push("tic80-cart".to_string());
    }
    runtimes.extend(
        ROM_IDS
            .iter()
            .filter(|id| System::for_id(id).is_some())
            .map(|id| format!("rom:{id}")),
    );
    runtimes
}

/// What a payload butler hands us is here, or why this one cannot run
/// even though the firmware was said to run its kind.
pub fn content_for(candidate: &Candidate, path: PathBuf) -> Result<Content, String> {
    let engine = candidate.engine.as_ref();
    let id = match candidate.flavor {
        Flavor::Love => {
            let Some(love) = love() else {
                return Err("this device has no LÖVE".to_string());
            };
            let wanted = engine.and_then(|e| e.version.as_deref()).unwrap_or("");
            return match love_blocker(wanted, &love.version) {
                Some(why) => Err(why),
                None => Ok(Content::Love { path }),
            };
        }
        Flavor::ROM => engine
            .filter(|e| e.engine == Engine::ROM)
            .and_then(|e| e.details.as_ref()?.get("system")?.as_str())
            .unwrap_or(""),
        Flavor::Pico8Cart => PICO8,
        Flavor::TIC80Cart => TIC80,
        other => return Err(format!("no runtime for {other:?} on this device")),
    };
    if id.is_empty() {
        // dash leaves the system empty for a disc image it could not read.
        return Err("could not tell which system this is for".to_string());
    }
    match System::for_id(id) {
        Some(system) => Ok(Content::Rom { path, system }),
        None => Err(format!("no emulator for {id} on this device")),
    }
}

/// Why a Linux build cannot run here, from what butler read out of its
/// executable, or `None` when nothing says it cannot. The screen is the
/// usual problem: only the firmware's SDL2 can open it, so a build must
/// link that, or bundle an SDL2 the shim can redirect there. butler
/// keeps 32-bit ARM builds as a fallback for arm64 hosts, which this
/// firmware cannot honour: it has no 32-bit loader or libraries.
pub fn native_blocker(info: &LinuxInfo) -> Option<String> {
    blocker(info, glibc_version())
}

fn blocker(info: &LinuxInfo, glibc: (u32, u32)) -> Option<String> {
    match info.arch {
        Some(Arch::Arm64) | None => {}
        Some(Arch::Arm) => {
            return Some("is a 32-bit ARM build; this device runs 64-bit only".into());
        }
        Some(_) => return Some("is not an ARM build".into()),
    }
    if let Some(version) = info.glibc_version.as_deref()
        && let Some(needed) = parse_version(version)
        && needed > glibc
    {
        return Some(format!(
            "needs glibc {version}; this device has {}.{}",
            glibc.0, glibc.1
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

/// The host's C library; a build wanting a newer one fails to load.
pub fn glibc_version() -> (u32, u32) {
    static VERSION: OnceLock<(u32, u32)> = OnceLock::new();
    *VERSION.get_or_init(|| host_glibc_version().unwrap_or(FALLBACK_GLIBC_VERSION))
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn host_glibc_version() -> Option<(u32, u32)> {
    // SAFETY: returns a pointer to a static NUL-terminated string.
    let version = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
    parse_version(version.to_str().ok()?)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn host_glibc_version() -> Option<(u32, u32)> {
    None
}

/// dash's ROM system ids: what `Launch.GetTargets` runtimes take and
/// what a ROM payload's `engine.details["system"]` holds.
const ROM_IDS: [&str; 23] = [
    "nes",
    "snes",
    "gb",
    "gbc",
    "gba",
    "nds",
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
const DISC_EXTS: [&str; 3] = ["cue", "iso", "chd"];
const DISC_IDS: [&str; 6] = ["psx", "ps2", "psp", "saturn", "segacd", "dreamcast"];
/// Fantasy consoles: dash names their carts as payloads of their own,
/// the firmware assigns them like any system.
const PICO8: &str = "pico8";
const TIC80: &str = "tic80";

/// dash ids that `assign.json` spells differently.
fn assign_key(id: &str) -> &str {
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
    assign: String,
    /// The default launcher: the launcher ini's stem, or the core's key
    /// in the manifest.
    launcher: String,
    /// The core file the launcher loads.
    core: String,
    /// The system's display name.
    label: String,
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

fn manifests_here() -> bool {
    Path::new(MANIFEST_DIR).join("libretro.json").is_file()
}

/// Every system that resolves here, by dash id. Read once.
fn systems() -> &'static HashMap<String, System> {
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
            ids.filter_map(|id| Some((id.to_string(), resolve_manifest(id, &aliases, &manifests)?)))
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
struct ManifestSystem {
    default: String,
    cores: HashMap<String, ManifestCore>,
}

#[derive(serde::Deserialize)]
struct ManifestCore {
    core: String,
}

fn parse_manifest(name: &str, json: &str) -> Option<Manifest> {
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
fn resolve_manifest(
    id: &str,
    aliases: &HashMap<String, String>,
    manifests: &[(&str, Manifest)],
) -> Option<System> {
    let assign = aliases.get(assign_key(id))?;
    let (prefix, system) = manifests
        .iter()
        .find_map(|(prefix, m)| Some((prefix, m.get(assign)?)))?;
    let Some(core) = system.cores.get(&system.default) else {
        log::warn!("{assign}: default core {} is not listed", system.default);
        return None;
    };
    Some(System {
        id: id.to_string(),
        assign: assign.to_string(),
        launcher: format!("{prefix}{}", system.default),
        core: core.core.clone(),
        label: assign.to_string(),
    })
}

/// `assign.json`: aliases to assign folder names.
fn parse_assign(json: &str) -> HashMap<String, String> {
    match serde_json::from_str::<HashMap<String, serde_json::Value>>(json) {
        Ok(map) => map
            .into_iter()
            .filter_map(|(k, v)| Some((k, v.as_str()?.to_string())))
            .collect(),
        Err(error) => {
            log::warn!("{ASSIGN_DIR}/assign.json: {error}");
            HashMap::new()
        }
    }
}

/// `key=` under `[section]`; bare lines are skipped.
fn ini_value<'a>(ini: &'a str, section: &str, key: &str) -> Option<&'a str> {
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
fn resolve(
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
fn id_for_file(path: &Path) -> Option<&'static str> {
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
struct Handoff {
    rom: PathBuf,
    governor: PathBuf,
    filter: PathBuf,
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
fn handoff() -> &'static Handoff {
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

/// The process group of the content running now, so [`stop`] can end
/// it: the emulator is a grandchild behind the launch script, so a
/// group is the only handle on it. One game runs at a time.
static RUNNING: Mutex<Option<u32>> = Mutex::new(None);
/// Set by [`stop`], cleared by [`begin`]. A stop can land while butler
/// is still deciding what to run, before there is a group to signal;
/// the spawn that follows checks it and ends the child at once.
static STOPPED: AtomicBool = AtomicBool::new(false);

/// Marks the start of a launch, so a stop asked for during it counts.
pub fn begin() {
    STOPPED.store(false, Ordering::SeqCst);
}

/// Runs the content and returns when it exits. `name` is what the
/// firmware shows in its overlays and history; `args` and `env` are the
/// manifest action's, and apply where the runtime takes them.
pub fn launch(
    name: &str,
    content: &Content,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<()> {
    log::info!(
        "launching {} as {}",
        content.path().display(),
        content.label()
    );
    let result = match content {
        Content::Rom { path, system } => {
            if !args.is_empty() {
                log::warn!("ignoring manifest arguments {args:?} for a ROM");
            }
            launch_rom(name, system, path, env)
        }
        Content::Love { path } => launch_love(path, args, env),
    };
    *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = None;
    result
}

/// Names the game's process to the panic combo: butler's, for a Linux
/// build it runs itself.
pub fn foreground(pid: u32) {
    if available() {
        set_foreground(&pid.to_string());
    }
}

/// Names zitch again once a launch is over. RetroArch's launcher script
/// names itself and never puts the app back. The name is the binary's,
/// which the port build spells `zitch.aarch64`.
pub fn foreground_back() {
    if available() {
        let name = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.file_name()?.to_str().map(str::to_string))
            .unwrap_or_else(|| "zitch".to_string());
        set_foreground(&name);
    }
}

/// Ends whatever [`launch`] is running, or is about to.
pub fn stop() {
    STOPPED.store(true, Ordering::SeqCst);
    let group = *RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(group) = group {
        end_group(group);
    }
}

fn end_group(group: u32) {
    log::info!("stopping process group {group}");
    match Command::new("kill")
        .arg("-TERM")
        .arg(format!("-{group}"))
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => log::warn!("kill exited with {status}"),
        Err(error) => log::warn!("running kill: {error}"),
    }
}

/// Puts the child in a process group of its own and remembers it for
/// [`stop`]. Nothing here runs anywhere but Linux; the gate keeps the
/// other builds compiling.
fn spawn_group(command: &mut Command) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn()?;
    {
        let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        *running = Some(child.id());
        if STOPPED.load(Ordering::SeqCst) {
            end_group(child.id());
        }
    }
    Ok(child)
}

fn set_foreground(process: &str) {
    if let Err(error) = std::fs::write(FOREGROUND_PROCESS, process) {
        log::warn!("setting {FOREGROUND_PROCESS}: {error}");
    }
}

/// Runs a `.love` (or a folder) in LÖVE. Its own SDL window takes the
/// screen, like RetroArch's. The panic combo gets its pid, as there is
/// no launcher script to name it.
fn launch_love(path: &Path, args: &[String], env: &HashMap<String, String>) -> Result<()> {
    let love = love().context("no LÖVE on this device")?;
    let dir = love.binary.parent().unwrap_or(Path::new("/"));
    let mut command = Command::new(&love.binary);
    command
        .arg(path)
        .args(args)
        .current_dir(dir)
        .env("LD_LIBRARY_PATH", &love.libs)
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .envs(env);
    let mut child =
        spawn_group(&mut command).with_context(|| format!("running {}", love.binary.display()))?;
    set_foreground(&child.id().to_string());
    let status = child
        .wait()
        .with_context(|| format!("waiting for {}", love.binary.display()))?;
    if !status.success() {
        bail!("love exited with {status}");
    }
    Ok(())
}

/// Runs `rom` in the firmware's emulator for `system`.
fn launch_rom(
    name: &str,
    system: &System,
    rom: &Path,
    env: &HashMap<String, String>,
) -> Result<()> {
    let System {
        assign,
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
    let status = spawn_group(&mut command)
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::butlerd::types::EngineInfo;

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
            resolve_manifest("gba", &aliases, &manifests),
            Some(System {
                id: "gba".into(),
                assign: "Nintendo Game Boy Advance".into(),
                launcher: "mu-mgba".into(),
                core: "mgba_libretro.so".into(),
                label: "Nintendo Game Boy Advance".into(),
            })
        );
        // Only in the second manifest.
        let n64 = resolve_manifest("n64", &aliases, &manifests).unwrap();
        assert_eq!(n64.core, "ext-mupen64plus-gliden64");
        assert_eq!(n64.launcher, "ext-mupen64plus - standalone - glide");
        // Not in assign.json at all.
        assert_eq!(resolve_manifest("nds", &aliases, &manifests), None);
        // In assign.json, in no manifest.
        assert_eq!(resolve_manifest("md", &aliases, &manifests), None);
        // Listed, but its default core is not.
        assert_eq!(resolve_manifest("nes", &aliases, &manifests), None);
        assert!(parse_manifest("x.json", "not json").is_none());
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
        // nothing resolves without the firmware
        assert!(!disc_runs_here(Path::new("game.cue")));
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
        assert_eq!(love_blocker("", "11.5"), None);
        assert!(love_blocker("0.8.0", "11.5").is_some());
        assert!(love_blocker("0.10.2", "11.5").unwrap().contains("11.5"));
    }

    #[test]
    fn payloads_without_a_runtime_are_turned_away() {
        // no LÖVE off-device, and no runtime for a native build either
        let love = payload(
            Flavor::Love,
            Some(EngineInfo {
                engine: Engine::Love,
                version: Some("11.5".into()),
                details: None,
            }),
        );
        assert_eq!(
            content_for(&love, PathBuf::from("/g/game.love")),
            Err("this device has no LÖVE".to_string())
        );
        let native = payload(Flavor::NativeLinux, None);
        assert!(content_for(&native, PathBuf::from("/g/bin")).is_err());
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
