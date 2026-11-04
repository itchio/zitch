//! What runs here, whatever the firmware: the ROMs and carts it has
//! emulators for, our LÖVE, and the Linux builds the SDL shim can route
//! to the screen, with the reasons the rest cannot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::butlerd::types::{
    Arch, Candidate, Engine, EngineInfo, Flavor, LaunchStrategy, LaunchTarget, LinuxInfo,
};

use super::*;

/// What the handhelds ship, assumed when the host's cannot be read.
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

/// An emulated system the firmware has an emulator for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct System {
    /// dash's id, e.g. `gba`.
    pub id: String,
    /// The firmware's name for the system.
    pub(crate) name: String,
    /// The firmware's default launcher for it.
    pub(crate) launcher: String,
    /// The core file the launcher loads.
    pub(crate) core: String,
    /// The system's display name.
    pub(crate) label: String,
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

/// Every system the firmware resolves, by dash id. Read once.
pub(super) fn systems() -> &'static HashMap<String, System> {
    #[cfg(test)]
    if let Some(fake) = super::fake::firmware() {
        return fake.systems;
    }
    static SYSTEMS: OnceLock<HashMap<String, System>> = OnceLock::new();
    SYSTEMS.get_or_init(|| firmware().map(|f| f.systems()).unwrap_or_default())
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

/// Something in an install folder the firmware can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// A file the firmware's emulator plays with the system's core;
    /// fantasy console carts count.
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

    pub(super) fn path(&self) -> &Path {
        match self {
            Content::Rom { path, .. } | Content::Love { path } => path,
        }
    }
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
pub(super) fn disc_runs_here(path: &Path) -> bool {
    is_disc_image(path) && DISC_IDS.iter().any(|id| System::for_id(id).is_some())
}

pub(super) fn is_disc_image(path: &Path) -> bool {
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
        Flavor::Pico8Cart => {
            // A web export's carts are packed into its JavaScript.
            let format = engine
                .and_then(|e| e.details.as_ref()?.get("format")?.as_str())
                .unwrap_or("");
            if format == "js" {
                return Err("PICO-8 web exports are not supported yet".to_string());
            }
            PICO8
        }
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

/// Something `Launch.GetTargets` listed that this device can run: the
/// name `Launch` matches a target by (the action's path, relative to the
/// install folder) and what to call it in a pick.
pub struct LaunchChoice {
    pub target: String,
    pub label: String,
}

/// The AppImages among the targets, which run unpacked ([`unpack`]).
pub fn appimages(targets: &[LaunchTarget]) -> Vec<PathBuf> {
    targets
        .iter()
        .filter_map(|t| t.strategy.as_ref())
        .filter(|s| {
            s.strategy == LaunchStrategy::Native && is_appimage(Path::new(&s.full_target_path))
        })
        .map(|s| PathBuf::from(&s.full_target_path))
        .collect()
}

/// Sorts butler's launch targets into what runs here, in the order to
/// offer them, and why the rest do not. An AppImage is offered as the
/// `AppRun` of its unpacked tree, under the image's name, and what else
/// the tree holds is neither offered nor counted against it unless it
/// cannot run here, which rules the image out. A native build is the developer's
/// own runtime for the game, which beats ours: a PICO-8 export carries
/// the real player, where the firmware's fake-08 gets some carts wrong. A
/// fused LÖVE exe is listed once as a native build and once as the
/// payload inside; the path tells them apart.
pub fn launch_choices(targets: &[LaunchTarget]) -> (Vec<LaunchChoice>, Vec<String>) {
    let file_name = |path: &str| {
        Path::new(path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(path)
            .to_string()
    };
    let mut natives: Vec<(Option<PathBuf>, LaunchChoice)> = Vec::new();
    let mut payloads: Vec<(String, LaunchChoice)> = Vec::new();
    let mut unrunnable = Vec::new();
    let mut blocked_images: Vec<PathBuf> = Vec::new();
    for target in targets {
        let Some(strategy) = target.strategy.as_ref() else {
            continue;
        };
        let Some(candidate) = strategy.candidate.as_ref() else {
            continue;
        };
        let path = strategy.full_target_path.clone();
        let action_path = target
            .action
            .as_ref()
            .map_or(candidate.path.clone(), |a| a.path.clone());
        match strategy.strategy {
            LaunchStrategy::Runtime => match content_for(candidate, PathBuf::from(&path)) {
                Ok(content) => {
                    if !payloads.iter().any(|(p, _)| *p == path) {
                        let choice = LaunchChoice {
                            label: format!("{} ({})", file_name(&path), content.label()),
                            target: action_path,
                        };
                        payloads.push((path, choice));
                    }
                }
                Err(reason) => unrunnable.push(reason),
            },
            LaunchStrategy::Native => {
                if is_appimage(Path::new(&path)) {
                    continue;
                }
                let unpacked = unpacked_from(Path::new(&path));
                let blocked = candidate.linux_info.as_ref().and_then(native_blocker);
                match (unpacked, blocked) {
                    (Some((image, _)), Some(reason)) => {
                        unrunnable
                            .push(format!("{} {reason}", file_name(&image.to_string_lossy())));
                        blocked_images.push(image);
                    }
                    (Some((_, false)), None) => {}
                    (Some((image, true)), None) => {
                        let label =
                            format!("{} (Linux build)", file_name(&image.to_string_lossy()));
                        natives.push((
                            Some(image),
                            LaunchChoice {
                                label,
                                target: action_path,
                            },
                        ));
                    }
                    (None, Some(reason)) => {
                        unrunnable.push(format!("{} {reason}", file_name(&path)));
                    }
                    (None, None) => natives.push((
                        None,
                        LaunchChoice {
                            label: format!("{} (Linux build)", file_name(&path)),
                            target: action_path,
                        },
                    )),
                }
            }
            _ => {}
        }
    }
    let mut choices: Vec<LaunchChoice> = natives
        .into_iter()
        .filter(|(image, _)| !image.as_ref().is_some_and(|i| blocked_images.contains(i)))
        .map(|(_, choice)| choice)
        .collect();
    choices.extend(payloads.into_iter().map(|(_, choice)| choice));
    (choices, unrunnable)
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

pub(super) fn blocker(info: &LinuxInfo, glibc: (u32, u32)) -> Option<String> {
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

/// A target from itch.io's scan of an upload. dash names the info fields
/// differently there than in a Candidate, so it gets its own type.
#[derive(Debug, Default, Deserialize)]
pub struct ScannedTarget {
    #[serde(default)]
    pub flavor: Flavor,
    #[serde(default)]
    pub arch: Option<Arch>,
    #[serde(default)]
    pub engine: Option<EngineInfo>,
    #[serde(default)]
    pub linux_info: Option<LinuxInfo>,
}

/// Whether a scanned target would run once installed, by the checks
/// launching it makes, except that a LÖVE game must say its version.
pub fn scanned_target_runs_here(target: &ScannedTarget) -> bool {
    scanned_target_fits(target, glibc_version())
}

pub(super) fn scanned_target_fits(target: &ScannedTarget, glibc: (u32, u32)) -> bool {
    match target.flavor {
        Flavor::NativeLinux => {
            let info = target.linux_info.as_ref();
            // At launch butler has already dropped builds for other
            // architectures, so native_blocker lets a missing arch through.
            let arch = info.and_then(|i| i.arch).or(target.arch);
            arch == Some(Arch::Arm64)
                && info.is_none_or(|i| {
                    i.os.as_deref().unwrap_or("").is_empty() && blocker(i, glibc).is_none()
                })
        }
        // A LÖVE game that does not say its version only runs if picked.
        Flavor::Love if !target.engine.as_ref().is_some_and(love_version_known) => false,
        flavor => {
            let candidate = Candidate {
                flavor,
                engine: target.engine.clone(),
                ..Default::default()
            };
            content_for(&candidate, PathBuf::new()).is_ok()
        }
    }
}

pub(super) fn love_version_known(engine: &EngineInfo) -> bool {
    parse_version(engine.version.as_deref().unwrap_or("")).is_some()
}

pub(super) fn parse_version(version: &str) -> Option<(u32, u32)> {
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
pub(super) fn host_glibc_version() -> Option<(u32, u32)> {
    // SAFETY: returns a pointer to a static NUL-terminated string.
    let version = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
    parse_version(version.to_str().ok()?)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(super) fn host_glibc_version() -> Option<(u32, u32)> {
    None
}
