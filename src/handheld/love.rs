//! The LÖVE runtime: ours next to the binary or one the firmware
//! carries, which games it runs, and running one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

use crate::report::Run;

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

/// Ours, deployed next to the binary by handheld-love, else one the
/// firmware carries.
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
        let theirs = firmware().map(|f| f.love_candidates()).unwrap_or_default();
        ours.into_iter().chain(theirs).find_map(love_at)
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

/// Runs a `.love` (or a folder) in LÖVE. Its own SDL window takes the
/// screen, like an emulator's. The firmware gets its pid, as there is
/// no launcher script to name it. Its stderr still reaches ours, and its
/// tail and exit status go in `run`.
pub(super) fn launch_love(
    path: &Path,
    args: &[String],
    env: &HashMap<String, String>,
    run: &mut Run,
) -> Result<()> {
    let love = love().context("no LÖVE on this device")?;
    let dir = love.binary.parent().unwrap_or(Path::new("/"));
    let game_env = game_env();
    let mut command = Command::new(&love.binary);
    command
        .arg(path)
        .args(args)
        .current_dir(dir)
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .envs(game_env.iter().map(|(name, value)| (name, value)))
        // Asks for ES outright, so the shim never has to interpret
        // the desktop probes LÖVE makes first otherwise.
        .env("LOVE_GRAPHICS_USE_OPENGLES", "1")
        .envs(env)
        .env("LD_LIBRARY_PATH", love_lib_path(&love.libs, &game_env, env))
        .stderr(std::process::Stdio::piped());
    let mut child =
        spawn_group(&mut command).with_context(|| format!("running {}", love.binary.display()))?;
    game_started(child.id());
    // Drained as it comes, or the game blocks once the pipe fills.
    let drain = child
        .stderr
        .take()
        .map(|pipe| std::thread::spawn(move || stderr_tail(pipe)));
    let status = child
        .wait()
        .with_context(|| format!("waiting for {}", love.binary.display()))?;
    if let Some(tail) = drain.and_then(|d| d.join().ok()) {
        run.stderr_tail = tail;
    }
    run.exit_code = status.code();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        run.signal = status.signal();
    }
    if !status.success() {
        bail!("love exited with {status}");
    }
    Ok(())
}

/// LÖVE's own libraries first, then whatever the game environments add.
fn love_lib_path(
    libs: &Path,
    game_env: &[(String, std::ffi::OsString)],
    env: &HashMap<String, String>,
) -> std::ffi::OsString {
    let mut dirs = vec![libs.to_path_buf()];
    for (name, value) in game_env {
        if name == "LD_LIBRARY_PATH" {
            dirs.extend(std::env::split_paths(value));
        }
    }
    if let Some(rest) = env.get("LD_LIBRARY_PATH") {
        dirs.extend(std::env::split_paths(rest));
    }
    std::env::join_paths(&dirs).unwrap_or_else(|_| libs.into())
}
