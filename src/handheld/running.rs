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
pub(super) static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

#[derive(Clone, Copy)]
pub(super) struct Running {
    group: u32,
    /// The group leader is a firmware launch script with housekeeping
    /// after the emulator, so a stop spares it.
    launcher: bool,
}
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
    let running = *RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    match running {
        Some(Running {
            group,
            launcher: true,
        }) => end_group_but_leader(group),
        Some(Running { group, .. }) => end_group(group),
        None => {}
    }
}

fn kill_term(targets: impl IntoIterator<Item = String>) {
    match Command::new("kill").arg("-TERM").args(targets).status() {
        Ok(status) if status.success() => {}
        Ok(status) => log::warn!("kill exited with {status}"),
        Err(error) => log::warn!("running kill: {error}"),
    }
}

pub(super) fn end_group(group: u32) {
    log::info!("stopping process group {group}");
    kill_term([format!("-{group}")]);
}

/// Ends the group's processes but its leader, which goes on to its own
/// end once the others are gone. The members come from /proc.
fn end_group_but_leader(group: u32) {
    log::info!("stopping process group {group} but its leader");
    let members: Vec<String> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != group)
        .filter(|pid| process_group(*pid) == Some(group))
        .map(|pid| pid.to_string())
        .collect();
    if !members.is_empty() {
        kill_term(members);
    }
}

/// A process's group, from the field after the parenthesised name in
/// its stat line.
fn process_group(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(2)?.parse().ok()
}

/// Puts the child in a process group of its own and remembers it for
/// [`stop`]. Nothing here runs anywhere but Linux; the gate keeps the
/// other builds compiling.
pub(super) fn spawn_group(command: &mut Command) -> std::io::Result<std::process::Child> {
    spawn(command, false)
}

/// [`spawn_group`] for a firmware launch script, which a stop spares so
/// it can tidy up after the emulator it ran.
pub(super) fn spawn_launcher(command: &mut Command) -> std::io::Result<std::process::Child> {
    spawn(command, true)
}

fn spawn(command: &mut Command, launcher: bool) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn()?;
    {
        let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        *running = Some(Running {
            group: child.id(),
            launcher,
        });
        // A stop that came first: the script has not started anything
        // yet, so it goes too.
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A launcher stand-in: a shell that runs a long child and then a
    /// line of its own. Sparing it means the line still runs.
    #[test]
    fn stop_spares_the_launcher() {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & wait $!; echo tidy"])
            .stdout(std::process::Stdio::piped())
            .process_group(0);
        let child = command.spawn().unwrap();
        let group = child.id();
        // Give the shell time to fork the sleep.
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(5)
            && !std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some_and(|pid| pid != group && process_group(pid) == Some(group))
            })
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        end_group_but_leader(group);
        let output = child.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "tidy");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn own_process_group_reads() {
        assert!(process_group(std::process::id()).is_some());
    }
}
