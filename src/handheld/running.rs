//! Running a game the way the firmware's own menu does: a ROM or cart
//! through the firmware's launcher, a `.love` through our LÖVE; and the
//! environment every game gets.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

use crate::report::{Run, STDERR_TAIL, Strategy};

use super::*;

/// The process group of the content running now, so [`stop`] can end
/// it: the emulator is a grandchild behind the launch script, so a
/// group is the only handle on it. One game runs at a time.
pub(super) static RUNNING: Mutex<Option<u32>> = Mutex::new(None);
/// Set by [`stop`], cleared by [`begin`]. A stop can land while butler
/// is still deciding what to run, before there is a group to signal;
/// the spawn that follows checks it and ends the child at once.
pub(super) static STOPPED: AtomicBool = AtomicBool::new(false);

/// Marks the start of a launch, so a stop asked for during it counts.
pub fn begin() {
    STOPPED.store(false, Ordering::SeqCst);
}

/// Runs the content and returns when it exits. `name` is what the
/// firmware shows in its overlays and history; `args` and `env` are the
/// manifest action's, and apply where the runtime takes them. What can
/// be seen of the run goes in `run`.
pub fn launch(
    name: &str,
    content: &Content,
    args: &[String],
    env: &HashMap<String, String>,
    run: &mut Run,
) -> Result<()> {
    log::info!(
        "launching {} as {}",
        content.path().display(),
        content.label()
    );
    let file = content
        .path()
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let result = match content {
        Content::Rom { path, system } => {
            if !args.is_empty() {
                log::warn!("ignoring manifest arguments {args:?} for a ROM");
            }
            run.strategy = Some(Strategy::Retroarch);
            run.core = Some(system.core.clone());
            run.launch_target = Some(format!("rom:{} {file}", system.id));
            match firmware() {
                Some(firmware) => firmware.launch_rom(name, system, path, env),
                None => bail!("no firmware to run a ROM"),
            }
        }
        Content::Love { path } => {
            run.strategy = Some(Strategy::Love);
            run.launch_target = Some(match love_version() {
                Some(version) => format!("love:{version} {file}"),
                None => format!("love {file}"),
            });
            launch_love(path, args, env, run)
        }
    };
    *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = None;
    result
}

/// Tells the firmware a game has the screen: butler's process, for a
/// Linux build it runs itself.
pub fn game_started(pid: u32) {
    if let Some(firmware) = firmware() {
        firmware.game_started(pid);
    }
}

/// Tells the firmware the screen is zitch's again.
pub fn game_ended() {
    if let Some(firmware) = firmware() {
        firmware.game_ended();
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

pub(super) fn end_group(group: u32) {
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
pub(super) fn spawn_group(command: &mut Command) -> std::io::Result<std::process::Child> {
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

/// Copies a game's stderr to ours until it closes, keeping the last
/// [`STDERR_TAIL`] bytes.
pub(super) fn stderr_tail(mut pipe: impl std::io::Read) -> String {
    use std::io::Write;
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = std::io::stderr().write_all(&buf[..n]);
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > 2 * STDERR_TAIL {
                    kept.drain(..kept.len() - STDERR_TAIL);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&kept);
    crate::report::tail(&text, STDERR_TAIL).to_string()
}

/// Routes a game's own SDL2 into the firmware's; see handheld/sdl-dynapi.c.
/// Deployed next to our binary.
pub(super) const SDL_SHIM: &str = "libzitch-sdl.so";
/// Next to the executable: gl4es as the libGL a Linux build asks for.
pub(super) const GL4ES_DIR: &str = "gl4es";

/// What the games butler launches need in their environment here: the
/// SDL shim, gl4es, whatever the firmware adds, and a locale, which the
/// firmwares leave unset and games read without checking.
pub fn game_env() -> Vec<(String, OsString)> {
    let Some(firmware) = firmware() else {
        return Vec::new();
    };
    let mut env = Vec::new();
    let shim = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(SDL_SHIM)))
        .filter(|path| path.is_file());
    match shim {
        Some(shim) => env.push(("SDL_DYNAMIC_API".to_string(), shim.into_os_string())),
        None => log::warn!("{SDL_SHIM} is missing; Linux builds will not find the screen"),
    }
    let gl4es = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(GL4ES_DIR)))
        .filter(|dir| dir.join("libGL.so.1").is_file());
    let inherited = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&inherited)
        .filter(|dir| firmware.keeps_library_dir(dir))
        .collect();
    match gl4es {
        Some(dir) => dirs.insert(0, dir),
        None => log::warn!("{GL4ES_DIR}/libGL.so.1 is missing; desktop GL builds will not start"),
    }
    if let Ok(path) = std::env::join_paths(&dirs) {
        env.push(("LD_LIBRARY_PATH".to_string(), path));
    }
    firmware.game_env(&mut env);
    if std::env::var_os("LANG").is_none() {
        env.push(("LANG".to_string(), "en_US.UTF-8".into()));
    }
    env
}
