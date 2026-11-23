//! BananaOS (RG DS, RG DS Plus): detected by its os-release and its
//! port launcher, so a Banana card mounted on a desktop is not one. zitch
//! runs there as a Ports entry, through `run-port`. The menu's systems
//! are sections of `systems.conf`, each with the command line that starts
//! a game (`run = /usr/libexec/banana/run-retroarch gpsp %ROM%`), and a
//! game is started here the way the menu starts it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::*;

const OS_RELEASE: &str = "/etc/os-release";

/// What runs every Ports entry, zitch among them.
const RUN_PORT: &str = "/usr/libexec/banana/run-port";

/// The menu's systems: the image's, with the card's merged over them at
/// the menu's start. The image's alone if the merged one is missing.
const SYSTEMS: [&str; 2] = ["/run/banana/systems.conf", "/etc/banana/systems.conf"];

const BOARD: &str = "/etc/banana/board";
const VERSION: &str = "/etc/banana/version";

/// Sets the CPU governor for a kind of game, as the menu does before each
/// launch; `run-port` games get `performance`.
const PERF: &str = "/usr/bin/banana-perf";
const PORT_PERF: &str = "performance";

/// The backlight `run-port` turned off for the panel a port doesn't draw
/// on, which bananad keeps off after a wake.
const PANEL_OFF: &str = "/run/banana/panel-off";

const BACKLIGHTS: &str = "/sys/class/backlight";

/// Where the menu's games have HOME.
const HOME: &str = "/";

/// What `run-port` and our launch script set for zitch that a game
/// started by the menu doesn't have.
const PORT_ENV: &[&str] = &[
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "SDL_GAMECONTROLLERCONFIG",
    "SDL_KMSDRM_FIRST",
    "SDL_KMSDRM_REQUIRE_DRM_MASTER",
    "PYSDL2_DLL_PATH",
];

/// `systems.conf` sections and their dash ids.
const IDS: &[(&str, &str)] = &[
    ("nes", "nes"),
    ("snes", "snes"),
    ("gb", "gb"),
    ("gbc", "gbc"),
    ("gba", "gba"),
    ("nds", "nds"),
    ("megadrive", "md"),
    ("c64", "c64"),
    ("zxspectrum", "zx"),
    ("n64", "n64"),
    ("psx", "psx"),
    ("pico-8", PICO8),
];

/// A `systems.conf` section with a program to run.
#[derive(Debug, PartialEq, Eq)]
struct Section {
    id: String,
    name: String,
    run: String,
    perf: Option<String>,
}

/// The sections whose `run` is a program; `banana:` ones are the menu's
/// own pages. A later key or section of the same name wins, as in the
/// menu's merge.
fn parse_systems(conf: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;
    let finish = |section: Option<Section>, sections: &mut Vec<Section>| {
        let Some(section) = section else { return };
        if !section.run.starts_with('/') {
            return;
        }
        sections.retain(|s| s.id != section.id);
        sections.push(section);
    };
    for line in conf.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(id) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            finish(current.take(), &mut sections);
            current = Some(Section {
                id: id.trim().to_string(),
                name: id.trim().to_string(),
                run: String::new(),
                perf: None,
            });
            continue;
        }
        let (Some(section), Some((key, value))) = (current.as_mut(), line.split_once('=')) else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "name" => section.name = value,
            "run" => section.run = value,
            "perf" => section.perf = Some(value),
            _ => {}
        }
    }
    finish(current, &mut sections);
    sections
}

fn system(section: &Section) -> Option<System> {
    let (_, id) = IDS.iter().find(|(name, _)| *name == section.id)?;
    let mut words = section.run.split_whitespace();
    let launcher = words.next()?.to_string();
    // run-retroarch's first argument is its core; the others pick their own.
    let core = match Path::new(&launcher).file_name()?.to_str()? {
        "run-retroarch" => words.next().unwrap_or_default().to_string(),
        other => other.to_string(),
    };
    Some(System {
        id: id.to_string(),
        name: section.id.clone(),
        launcher,
        core,
        label: section.name.clone(),
    })
}

fn read_systems() -> Vec<Section> {
    SYSTEMS
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|conf| parse_systems(&conf))
        .unwrap_or_default()
}

fn resolve_systems() -> HashMap<String, System> {
    read_systems()
        .iter()
        .filter_map(system)
        .map(|system| (system.id.clone(), system))
        .collect()
}

/// The section's command line with the game put in, word by word as the
/// menu's shell would split it; no shell, so the path needs no quoting.
fn command_line(run: &str, rom: &Path, section: &str) -> Vec<String> {
    let rom = rom.to_string_lossy();
    run.split_whitespace()
        .map(|word| word.replace("%ROM%", &rom).replace("%SYSTEM%", section))
        .collect()
}

fn set_perf(perf: &str) {
    if let Err(err) = Command::new(PERF).arg(perf).status() {
        log::warn!("{PERF} {perf}: {err}");
    }
}

/// The backlight of the panel zitch draws its interface on, once it has
/// lit it.
static OWN_PANEL: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// The panel `run-port` darkened, lit while a game has the screens (a DS
/// game draws on both) and dark again after.
struct OtherPanel(Option<PathBuf>);

impl OtherPanel {
    fn light() -> Self {
        OtherPanel(light_other_panel())
    }
}

/// Lights the panel `run-port` darkened and returns its backlight. With
/// the file gone, bananad leaves the panel lit after a wake.
fn light_other_panel() -> Option<PathBuf> {
    let backlight = std::fs::read_to_string(PANEL_OFF)
        .ok()
        .map(|path| PathBuf::from(path.trim()))
        .filter(|path| !path.as_os_str().is_empty())?;
    let _ = std::fs::remove_file(PANEL_OFF);
    if let Err(err) = std::fs::write(backlight.join("bl_power"), "0") {
        log::warn!("lighting {}: {err}", backlight.display());
    }
    Some(backlight)
}

impl Drop for OtherPanel {
    fn drop(&mut self) {
        let Some(backlight) = &self.0 else { return };
        if let Err(err) = std::fs::write(backlight.join("bl_power"), "4") {
            log::warn!("darkening {}: {err}", backlight.display());
        }
        let _ = std::fs::write(PANEL_OFF, format!("{}\n", backlight.display()));
    }
}

fn launch_rom(system: &System, rom: &Path, env: &HashMap<String, String>) -> Result<()> {
    let section = read_systems()
        .into_iter()
        .find(|s| s.id == system.name)
        .with_context(|| format!("BananaOS has no launcher for {}", system.label))?;
    let words = command_line(&section.run, rom, &section.id);
    let (program, args) = words
        .split_first()
        .with_context(|| format!("{} has an empty run line", section.id))?;

    let mut command = Command::new(program);
    command
        .args(args)
        .env("HOME", HOME)
        .env("BANANA_ROM", rom)
        .env("BANANA_SYSTEM", &section.id)
        .envs(env);
    for name in PORT_ENV {
        command.env_remove(name);
    }

    set_perf(section.perf.as_deref().unwrap_or("balanced"));
    let panel = OtherPanel::light();
    let status = spawn_launcher(&mut command)
        .with_context(|| format!("running {program}"))
        .and_then(|mut child| {
            child
                .wait()
                .with_context(|| format!("waiting for {program}"))
        });
    drop(panel);
    set_perf(PORT_PERF);

    // An emulator's status is its own quit's, so it only says whether the
    // launcher ran.
    match status?.code() {
        Some(code) => {
            log::debug!("{program} exited with {code}");
            Ok(())
        }
        None => bail!("{program} was killed by a signal"),
    }
}

fn os_release_name(os_release: &str) -> Option<&str> {
    os_release.lines().find_map(|line| {
        let value = line.strip_prefix("NAME=")?;
        Some(value.trim_matches('"'))
    })
}

fn read_trimmed(path: &str) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

pub(super) struct BananaOs;

static BANANAOS: BananaOs = BananaOs;

pub(super) fn detect() -> Option<&'static dyn Firmware> {
    let os_release = std::fs::read_to_string(OS_RELEASE).ok()?;
    let banana = os_release_name(&os_release) == Some("BananaOS") && Path::new(RUN_PORT).is_file();
    banana.then_some(&BANANAOS as &dyn Firmware)
}

impl Firmware for BananaOs {
    fn name(&self) -> &'static str {
        "bananaos"
    }

    /// The board, e.g. `rgds-plus`, and the release, e.g. `0.5.3.6`.
    fn details(&self) -> Vec<(&'static str, String)> {
        let mut details = Vec::new();
        if let Some(board) = read_trimmed(BOARD) {
            details.push(("board", board));
        }
        if let Some(version) = read_trimmed(VERSION) {
            details.push(("release", version));
        }
        details
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
    /// The bottom panel. `run-port` lights it again when zitch exits.
    fn second_screen(&self) -> bool {
        let backlight = light_other_panel();
        *OWN_PANEL.lock().unwrap_or_else(|p| p.into_inner()) = backlight;
        true
    }

    /// A game of ours draws on the top panel alone, so ours goes dark
    /// under it. bananad keeps the named panel dark across a wake.
    fn game_started(&self, _pid: u32) {
        let own = OWN_PANEL.lock().unwrap_or_else(|p| p.into_inner());
        let Some(backlight) = own.as_ref() else {
            return;
        };
        if let Err(err) = std::fs::write(backlight.join("bl_power"), "4") {
            log::warn!("darkening {}: {err}", backlight.display());
        }
        let _ = std::fs::write(PANEL_OFF, format!("{}\n", backlight.display()));
    }

    fn game_ended(&self) {
        if OWN_PANEL
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            light_other_panel();
        }
    }

    /// zitch and its games both draw through SDL's KMSDRM driver.
    fn hands_over_display(&self) -> bool {
        true
    }

    /// The menu's idle dimming stops while a port runs, so zitch switches
    /// both backlights itself.
    fn screens_lit(&self, lit: bool) {
        let Ok(backlights) = std::fs::read_dir(BACKLIGHTS) else {
            return;
        };
        let power = if lit { "0" } else { "4" };
        for backlight in backlights.flatten() {
            let path = backlight.path().join("bl_power");
            if let Err(err) = std::fs::write(&path, power) {
                log::warn!("{}: {err}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = "\
# the systems
[gba]
name = Game Boy Advance
ext = gba zip 7z
run = /usr/libexec/banana/run-retroarch gpsp %ROM%
perf = balanced

[megadrive]
name = Mega Drive
folder = megadrive
run = /usr/libexec/banana/run-retroarch picodrive %ROM%

[nds]
name = Nintendo DS
run = /usr/libexec/banana/run-nds %ROM%
perf = superdrastic

[files]
name = Files
run = banana:files

[gba]
run = /usr/libexec/banana/run-retroarch mgba %ROM%
";

    #[test]
    fn sections_with_a_program_are_read() {
        let sections = parse_systems(CONF);
        let ids: Vec<&str> = sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["megadrive", "nds", "gba"]);
        let nds = &sections[1];
        assert_eq!(nds.name, "Nintendo DS");
        assert_eq!(nds.perf.as_deref(), Some("superdrastic"));
    }

    #[test]
    fn a_later_section_replaces_an_earlier_one() {
        let sections = parse_systems(CONF);
        let gba = sections.iter().find(|s| s.id == "gba").unwrap();
        assert_eq!(gba.run, "/usr/libexec/banana/run-retroarch mgba %ROM%");
    }

    #[test]
    fn systems_take_dash_ids_and_cores() {
        let sections = parse_systems(CONF);
        let systems: HashMap<String, System> = sections
            .iter()
            .filter_map(system)
            .map(|s| (s.id.clone(), s))
            .collect();
        let md = &systems["md"];
        assert_eq!(md.name, "megadrive");
        assert_eq!(md.core, "picodrive");
        assert_eq!(md.label, "Mega Drive");
        assert_eq!(md.launcher, "/usr/libexec/banana/run-retroarch");
        let nds = &systems["nds"];
        assert_eq!(nds.core, "run-nds");
        assert!(!systems.contains_key("files"));
    }

    #[test]
    fn unknown_sections_are_no_system() {
        let section = Section {
            id: "gameandwatch".into(),
            name: "Game & Watch".into(),
            run: "/usr/libexec/banana/run-gw %ROM%".into(),
            perf: None,
        };
        assert!(system(&section).is_none());
    }

    #[test]
    fn the_game_goes_in_unquoted() {
        assert_eq!(
            command_line(
                "/usr/libexec/banana/run-retroarch gpsp %ROM%",
                Path::new("/data/home/zitch/games/My Game (v1)/game.gba"),
                "gba",
            ),
            [
                "/usr/libexec/banana/run-retroarch",
                "gpsp",
                "/data/home/zitch/games/My Game (v1)/game.gba",
            ]
        );
        assert_eq!(
            command_line("/x/run %SYSTEM% %ROM%", Path::new("/g.nes"), "nes"),
            ["/x/run", "nes", "/g.nes"]
        );
    }

    #[test]
    fn os_release_names_banana() {
        let release = "NAME=BananaOS\nVERSION=0.5.3.6\nID=bananaos\n";
        assert_eq!(os_release_name(release), Some("BananaOS"));
        assert_eq!(os_release_name("NAME=\"Arch Linux\"\n"), Some("Arch Linux"));
        assert_eq!(os_release_name("ID=x\n"), None);
    }
}
