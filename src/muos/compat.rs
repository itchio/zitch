//! What runs here: the LÖVE we ship, the ROMs and carts the firmware
//! has cores for, and why a Linux build cannot reach the screen.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use serde::Deserialize;

use crate::butlerd::types::{
    Arch, Candidate, Engine, EngineInfo, Flavor, LaunchStrategy, LaunchTarget, LinuxInfo,
};

use super::*;

/// Why a game cannot run on the LÖVE that is here. The 0.x series is a
/// different API, so 11.x will not load those at all. A game made for a
/// newer LÖVE, or one that does not say which it wants, is given a try.
pub(super) fn love_blocker(wanted: &str, have: &str) -> Option<String> {
    let (major, _) = parse_version(wanted)?;
    let runs = parse_version(have).is_some_and(|(m, _)| m <= major);
    (!runs).then(|| format!("made for LÖVE {wanted}; this device has LÖVE {have}"))
}

/// The LÖVE games this device runs, in the words of a game's scanned
/// platforms: `love:11.0` up to its own version, and the next major one.
pub fn love_platforms() -> &'static [String] {
    static PLATFORMS: OnceLock<Vec<String>> = OnceLock::new();
    PLATFORMS.get_or_init(|| {
        love()
            .map(|love| love_platforms_for(&love.version))
            .unwrap_or_default()
    })
}

pub(super) fn love_platforms_for(have: &str) -> Vec<String> {
    let Some((major, minor)) = parse_version(have) else {
        return Vec::new();
    };
    (0..=minor)
        .map(|n| format!("love:{major}.{n}"))
        .chain([format!("love:{}.0", major + 1)])
        .collect()
}

pub(super) struct Love {
    pub(super) binary: PathBuf,
    pub(super) libs: PathBuf,
    pub(super) version: String,
}

/// Routes a game's own SDL2 into the firmware's; see handheld/sdl-dynapi.c.
/// Deployed next to our binary.
pub(super) const SDL_SHIM: &str = "libzitch-sdl.so";

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

    pub(super) fn path(&self) -> &Path {
        match self {
            Content::Rom { path, .. } | Content::Love { path } => path,
        }
    }
}

/// Ours, deployed next to the binary by handheld-love, else one the
/// firmware carries. muOS ships no LÖVE of its own: what is there
/// belongs to whichever bundled app happens to be written in LÖVE, and
/// those come and go between releases.
pub(super) fn love() -> Option<&'static Love> {
    #[cfg(test)]
    if let Some(fake) = super::fake::firmware() {
        return fake.love;
    }
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

/// The LÖVE version games run on here, if there is one.
pub fn love_version() -> Option<&'static str> {
    love().map(|love| love.version.as_str())
}

/// The runtime around a `love` binary, if it is one: `liblove` sits in a
/// sibling directory, named for the architecture on newer builds.
pub(super) fn love_at(binary: PathBuf) -> Option<Love> {
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
pub(super) fn liblove_version(libs: &Path) -> Option<String> {
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

/// Sorts butler's launch targets into what runs here, in the order to
/// offer them, and why the rest do not. A native build is the developer's
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
    let mut natives = Vec::new();
    let mut payloads: Vec<(String, LaunchChoice)> = Vec::new();
    let mut unrunnable = Vec::new();
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
                let blocked = candidate.linux_info.as_ref().and_then(native_blocker);
                match blocked {
                    Some(reason) => unrunnable.push(format!("{} {reason}", file_name(&path))),
                    None => natives.push(LaunchChoice {
                        label: format!("{} (Linux build)", file_name(&path)),
                        target: action_path,
                    }),
                }
            }
            _ => {}
        }
    }
    natives.extend(payloads.into_iter().map(|(_, choice)| choice));
    (natives, unrunnable)
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
