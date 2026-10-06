//! Running a game the way the firmware's own menu does: a ROM or cart
//! through its launch script and RetroArch, a `.love` through our LÖVE.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

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
            launch_rom(name, system, path, env)
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

pub(super) fn set_foreground(process: &str) {
    if let Err(error) = std::fs::write(FOREGROUND_PROCESS, process) {
        log::warn!("setting {FOREGROUND_PROCESS}: {error}");
    }
}

/// Runs a `.love` (or a folder) in LÖVE. Its own SDL window takes the
/// screen, like RetroArch's. The panic combo gets its pid, as there is
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
        .envs(env)
        .stderr(std::process::Stdio::piped());
    let mut child =
        spawn_group(&mut command).with_context(|| format!("running {}", love.binary.display()))?;
    set_foreground(&child.id().to_string());
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

/// Runs `rom` in the firmware's emulator for `system`.
pub(super) fn launch_rom(
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
