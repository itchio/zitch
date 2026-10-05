//! Types shared between the backend and the interface. Wire types come from
//! the generated butlerd bindings; these are the app's own.

pub use crate::butlerd::types::{
    Cave, Collection, CollectionGamesFilters, Download, DownloadProgress, DownloadReason, Game,
    GameClassification, GameUpdate, Platforms, Profile, Upload, User,
};

pub trait UserExt {
    /// The display name, or the username when none is set.
    fn name(&self) -> &str;
}

impl UserExt for User {
    fn name(&self) -> &str {
        if self.display_name.is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

pub trait UploadExt {
    fn name(&self) -> &str;
}

impl UploadExt for Upload {
    fn name(&self) -> &str {
        if !self.display_name.is_empty() {
            &self.display_name
        } else if !self.filename.is_empty() {
            &self.filename
        } else {
            "upload"
        }
    }
}

/// "1.2 GB" style sizes.
pub fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub trait CaveExt {
    fn game_id(&self) -> Option<i64>;
}

impl CaveExt for Cave {
    fn game_id(&self) -> Option<i64> {
        self.game.as_ref().map(|game| game.id)
    }
}

/// A queued or running download for a game, as the interface sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstallState {
    /// butlerd's download id; empty until the queue has answered.
    pub download_id: String,
    /// 0 to 1.
    pub progress: f64,
    pub bps: f64,
    pub eta_seconds: f64,
    /// What butler is doing right now: downloading, installing, and so on.
    pub stage: String,
    pub cancelling: bool,
    /// Set when the download stopped with an error; Retry or Dismiss apply.
    pub error: Option<String>,
}

/// A question the backend needs answered before a call can go on, shown as
/// a modal. The backend maps the chosen index back to the typed reply.
/// Why a launch did not run, with the tail of what the game printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFailure {
    pub message: String,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub choices: Vec<String>,
    pub focus: usize,
    /// The choice drawn as the primary button: the expected or safe answer
    /// to a question. A pick from a list of equals has none.
    pub primary: Option<usize>,
    /// The choices stand in a column stepped with Up and Down, rather
    /// than a row stepped with Left and Right.
    pub stacked: bool,
    /// Detail drawn under the choice at the same index; choices past its
    /// end have none.
    pub details: Vec<UploadDetail>,
    /// Work under way: a status line and how far along it is, 0 to 1.
    pub progress: Option<(String, f32)>,
}

/// Which part of the library the main row shows.
/// What kind of thing a library entry is, one row each on the Library tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Games,
    /// Tools and asset packs.
    Tools,
    /// Soundtracks, books, comics, mods, physical games, and the rest.
    Other,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Games, Kind::Tools, Kind::Other];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Games => "Games",
            Kind::Tools => "Tools & assets",
            Kind::Other => "Other",
        }
    }

    pub fn matches(self, game: &Game) -> bool {
        match self {
            Kind::Games => game.classification == GameClassification::Game,
            Kind::Tools => matches!(
                game.classification,
                GameClassification::Tool | GameClassification::Assets
            ),
            Kind::Other => !matches!(
                game.classification,
                GameClassification::Game | GameClassification::Tool | GameClassification::Assets
            ),
        }
    }
}

/// Whether the game has an upload for this operating system. On muOS the
/// platform tags say nothing (a ROM is tagged for nothing, or for whatever
/// the page's web player runs on), so every game is worth trying and the
/// install decides once it sees the files.
pub fn playable_here(game: &Game) -> bool {
    crate::muos::available() || runs_here(&game.platforms)
}

/// Whether itch.io's scan of the game's uploads found something this
/// device runs, of the types not in `hidden`, for the "Playable here"
/// filter. Not every upload gets scanned, so a miss here is a reason to
/// hide, not to refuse an install.
pub fn known_playable_here(game: &Game, hidden: &[String]) -> bool {
    if crate::muos::available() {
        game.scanned_platforms.as_deref().is_some_and(|scanned| {
            any_runs_here(
                scanned,
                device_platforms(),
                hidden,
                crate::muos::love_platforms(),
            )
        })
    } else {
        runs_here(&game.platforms)
    }
}

/// The page-wide filters as a collection query, so butler returns only
/// the games that pass instead of every page being fetched to find them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionFilter {
    pub installed: bool,
    pub playable: bool,
    /// Types left out of "Playable here"; empty while it is off.
    pub hidden: Vec<String>,
}

impl CollectionFilter {
    pub fn any(&self) -> bool {
        self.installed || self.playable
    }

    /// butler's filter matching [`known_playable_here`] and the installed
    /// toggle.
    pub fn to_butler(&self) -> CollectionGamesFilters {
        let mut filters = CollectionGamesFilters {
            installed: self.installed,
            ..Default::default()
        };
        if self.playable {
            if crate::muos::available() {
                filters.scanned_platforms = Some(wanted_platforms(
                    device_platforms(),
                    &self.hidden,
                    crate::muos::love_platforms(),
                ));
            } else {
                filters.platform = Some(os_platform().to_string());
            }
        }
        filters
    }
}

/// What this device runs, in the words of a game's scanned platforms.
/// The handheld profile is itch.io's check for an arm64 Linux build the
/// SDL shim can put on screen.
/// Worked out once: the filter asks this of every game in the library.
pub fn device_platforms() -> &'static [String] {
    static PLATFORMS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    PLATFORMS.get_or_init(|| {
        let mut platforms = crate::muos::runtimes();
        platforms.push("linux-arm64-handheld".to_string());
        platforms
    })
}

/// A heading for the types on the Playable types page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeGroup {
    Native,
    Engines,
    Systems,
}

impl TypeGroup {
    pub fn label(self) -> &'static str {
        match self {
            TypeGroup::Native => "Native",
            TypeGroup::Engines => "Engines",
            TypeGroup::Systems => "Systems",
        }
    }
}

/// One type the "Playable here" filter can include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayableType {
    pub id: String,
    pub label: String,
    pub group: TypeGroup,
}

/// The types this device runs, grouped, in page order.
pub fn playable_types() -> &'static [PlayableType] {
    static TYPES: std::sync::OnceLock<Vec<PlayableType>> = std::sync::OnceLock::new();
    TYPES.get_or_init(|| {
        let mut types: Vec<PlayableType> = device_platforms()
            .iter()
            .map(|id| {
                let group = if id.starts_with("rom:") {
                    TypeGroup::Systems
                } else if id.starts_with("linux-") {
                    TypeGroup::Native
                } else {
                    TypeGroup::Engines
                };
                let label = match id.as_str() {
                    "linux-arm64-handheld" => "Handheld builds".to_string(),
                    _ => scanned_platform_word(id).unwrap_or_else(|| id.clone()),
                };
                PlayableType {
                    id: id.clone(),
                    label,
                    group,
                }
            })
            .collect();
        types.sort_by_key(|t| t.group as u8);
        types
    })
}

/// The type on a row of the Playable types page, which has Everything
/// above the types.
pub fn type_at_row(row: usize) -> Option<&'static PlayableType> {
    playable_types().get(row.checked_sub(1)?)
}

/// The device's types that "Playable here" includes. Hiding all of them
/// counts as hiding none, so the filter never comes up empty.
pub fn shown_types<'a>(device: &'a [String], hidden: &[String]) -> Vec<&'a String> {
    let shown: Vec<&String> = device.iter().filter(|p| !hidden.contains(p)).collect();
    if shown.is_empty() {
        device.iter().collect()
    } else {
        shown
    }
}

/// The scanned platforms the shown types match. A LÖVE game only counts
/// with a version this device runs, `love` alone says nothing about which.
fn wanted_platforms(device: &[String], hidden: &[String], love: &[String]) -> Vec<String> {
    shown_types(device, hidden)
        .into_iter()
        .flat_map(|id| match id.as_str() {
            "love" => love.to_vec(),
            _ => vec![id.clone()],
        })
        .collect()
}

fn any_runs_here(
    scanned: &[String],
    device: &[String],
    hidden: &[String],
    love: &[String],
) -> bool {
    let wanted = wanted_platforms(device, hidden, love);
    scanned.iter().any(|p| wanted.contains(p))
}

/// Whether an upload is built for this device: on muOS something the
/// firmware or the SDL shim can run, going by itch.io's scan of its files
/// when it has one and its name otherwise; elsewhere an upload tagged for
/// the OS. With `scans_decide` ([`scans_decide`]), an unscanned upload
/// does not count.
pub fn upload_runs_here(upload: &Upload, scans_decide: bool) -> bool {
    if crate::muos::available() {
        if let Some(runs) = scan_runs_here(upload) {
            return runs;
        }
        if scans_decide {
            return false;
        }
        let path = std::path::Path::new(&upload.filename);
        // An archive can hold anything; what it holds is only known once
        // butler has unpacked it. One tagged for another OS is not worth
        // the download, one with no tags (how ROM and .love zips arrive)
        // is.
        let foreign = (upload.platforms.windows.is_some() || upload.platforms.osx.is_some())
            && upload.platforms.linux.is_none();
        crate::muos::runs_here(path) || (is_archive(path) && !foreign)
    } else {
        runs_here(&upload.platforms)
    }
}

/// Whether itch.io's scans alone decide which of a game's uploads run
/// here, leaving out the unscanned ones: on muOS, once any is scanned.
pub fn scans_decide<'a>(uploads: impl IntoIterator<Item = &'a Upload>) -> bool {
    crate::muos::available() && uploads.into_iter().any(|u| u.launch_targets.is_some())
}

/// What itch.io's scan of the upload's files says about this device, or
/// `None` when it was not scanned.
fn scan_runs_here(upload: &Upload) -> Option<bool> {
    let targets = upload.launch_targets.as_ref()?.as_array()?;
    Some(targets.iter().any(|t| {
        serde::Deserialize::deserialize(t).is_ok_and(|t| crate::muos::scanned_target_runs_here(&t))
    }))
}

/// What an upload picker shows under an upload's name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UploadDetail {
    /// What the upload holds in words, each with whether this device runs
    /// it: from itch.io's scan when there is one, its tags otherwise.
    pub platforms: Vec<(String, bool)>,
    /// Size, kind, and how sure the platforms are.
    pub notes: Vec<String>,
}

pub fn upload_detail(upload: &Upload, scans_decide: bool) -> UploadDetail {
    use serde::Deserialize;
    let muos = crate::muos::available();
    let mut detail = UploadDetail::default();
    let scanned = upload.launch_targets.as_ref().and_then(|t| t.as_array());
    match scanned {
        Some(targets) => {
            for target in targets {
                let Ok(target) = crate::muos::ScannedTarget::deserialize(target) else {
                    continue;
                };
                let Some((word, here)) = scanned_target_word(&target) else {
                    continue;
                };
                match detail.platforms.iter_mut().find(|(w, _)| *w == word) {
                    Some((_, runs)) => *runs |= here,
                    None => detail.platforms.push((word, here)),
                }
            }
            if targets.is_empty() {
                detail.notes.push("scan found nothing to run".to_string());
            }
        }
        None => {
            let by_name = muos && crate::muos::runs_here(std::path::Path::new(&upload.filename));
            detail.platforms = platform_words(&upload.platforms)
                .into_iter()
                .map(|word| {
                    let here = !muos && runs_here(&upload.platforms) && word == os_word();
                    (word.to_string(), here)
                })
                .collect();
            if muos {
                detail.notes.push(if by_name {
                    "not scanned, file name looks playable".to_string()
                } else if scans_decide || !upload_runs_here(upload, false) {
                    "not scanned".to_string()
                } else {
                    "not scanned, archive may hold anything".to_string()
                });
            }
        }
    }
    if upload.size > 0 {
        detail.notes.insert(0, human_size(upload.size));
    }
    if upload.demo {
        detail.notes.push("demo".to_string());
    }
    if let Some(kind) = upload_kind(upload) {
        detail.notes.push(kind.to_string());
    }
    detail
}

/// An upload's type in words, unless it is a plain download.
pub fn upload_kind(upload: &Upload) -> Option<&'static str> {
    use crate::butlerd::types::UploadType;
    match upload.r#type {
        UploadType::Default | UploadType::Other | UploadType::Unknown => None,
        UploadType::Flash => Some("flash"),
        UploadType::Unity => Some("unity web player"),
        UploadType::Java => Some("java"),
        UploadType::HTML => Some("html"),
        UploadType::Soundtrack => Some("soundtrack"),
        UploadType::Book => Some("book"),
        UploadType::Video => Some("video"),
        UploadType::Documentation => Some("documentation"),
        UploadType::Mod => Some("mod"),
        UploadType::AudioAssets => Some("audio assets"),
        UploadType::GraphicalAssets => Some("graphical assets"),
        UploadType::Sourcecode => Some("source code"),
    }
}

/// This OS as [`platform_words`] names it.
fn os_word() -> &'static str {
    match os_platform() {
        "linux" => "Linux",
        "osx" => "macOS",
        _ => "Windows",
    }
}

/// A scanned launch target in words, with whether it runs here. Native
/// builds name their architecture, since that decides it on a handheld.
fn scanned_target_word(target: &crate::muos::ScannedTarget) -> Option<(String, bool)> {
    use crate::butlerd::types::{Arch, Flavor};
    let arch = target
        .linux_info
        .as_ref()
        .and_then(|i| i.arch)
        .or(target.arch);
    let (os, platform_os) = match target.flavor {
        Flavor::NativeLinux => ("Linux", "linux"),
        Flavor::NativeWindows | Flavor::ScriptWindows | Flavor::MSI => ("Windows", "windows"),
        Flavor::NativeMacos | Flavor::AppMacos => ("macOS", "osx"),
        _ => ("", ""),
    };
    let here = |platform: &str| {
        if crate::muos::available() {
            crate::muos::scanned_target_runs_here(target)
        } else {
            desktop_runs(platform)
        }
    };
    if !os.is_empty() {
        let (arch_word, arch_id) = match arch {
            Some(Arch::Amd64) => (" x64", "amd64"),
            Some(Arch::_386) => (" x86", "386"),
            Some(Arch::Arm64) => (" ARM64", "arm64"),
            Some(Arch::Arm) => (" ARM", "arm"),
            Some(Arch::Riscv64) => (" RISC-V", "riscv64"),
            Some(Arch::Universal) => (" universal", "universal"),
            _ => ("", ""),
        };
        let runs = here(&format!("{platform_os}-{arch_id}"));
        return Some((format!("{os}{arch_word}"), runs));
    }
    let word = match target.flavor {
        Flavor::ROM => {
            let system = target
                .engine
                .as_ref()
                .and_then(|e| e.details.as_ref())
                .and_then(|d| d.get("system"))
                .and_then(|s| s.as_str())?;
            rom_system_name(system)
        }
        Flavor::Love => match target.engine.as_ref().and_then(|e| e.version.as_deref()) {
            Some(version) if !version.is_empty() => format!("LÖVE {version}"),
            _ => "LÖVE, version unknown".to_string(),
        },
        flavor => {
            let id = serde_json::to_value(flavor).ok()?;
            scanned_platform_word(id.as_str()?)?
        }
    };
    let runs = crate::muos::available() && crate::muos::scanned_target_runs_here(target);
    Some((word, runs))
}

/// The archive types butler unpacks on install.
fn is_archive(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz"
        )
    })
}

fn runs_here(p: &Platforms) -> bool {
    match os_platform() {
        "linux" => p.linux.is_some(),
        "osx" => p.osx.is_some(),
        _ => p.windows.is_some(),
    }
}

/// This OS in itch.io's platform words.
fn os_platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "windows"
    }
}

/// The platforms a game has downloads for, as words, for saying why it
/// cannot be installed here.
pub fn platform_names(game: &Game) -> Vec<&'static str> {
    platform_words(&game.platforms)
}

pub fn upload_platform_names(upload: &Upload) -> Vec<&'static str> {
    platform_words(&upload.platforms)
}

fn platform_words(p: &Platforms) -> Vec<&'static str> {
    let mut names = Vec::new();
    if p.windows.is_some() {
        names.push("Windows");
    }
    if p.osx.is_some() {
        names.push("macOS");
    }
    if p.linux.is_some() {
        names.push("Linux");
    }
    names
}

/// The game's scanned platforms as words, each with whether this device
/// runs it, for the detail page. `None` when the game was not scanned.
pub fn scanned_platform_words(game: &Game) -> Option<Vec<(String, bool)>> {
    let scanned = game.scanned_platforms.as_deref()?;
    Some(platform_words_here(scanned, |p| {
        if crate::muos::available() {
            any_runs_here(
                &[p.to_string()],
                device_platforms(),
                &[],
                crate::muos::love_platforms(),
            )
        } else {
            desktop_runs(p)
        }
    }))
}

fn platform_words_here(scanned: &[String], runs: impl Fn(&str) -> bool) -> Vec<(String, bool)> {
    let mut words: Vec<(String, bool)> = Vec::new();
    for platform in scanned {
        let Some(word) = scanned_platform_word(platform) else {
            continue;
        };
        let here = runs(platform);
        match words.iter_mut().find(|(w, _)| *w == word) {
            Some((_, runs)) => *runs |= here,
            None => words.push((word, here)),
        }
    }
    words
}

/// A scanned platform on a desktop: a build for this OS and architecture.
fn desktop_runs(platform: &str) -> bool {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        other => other,
    };
    let os = os_platform();
    platform == format!("{os}-{arch}") || (arch == "amd64" && platform == format!("{os}-386"))
}

/// A scanned platform in words, or `None` for one that is not a way to
/// play the game.
fn scanned_platform_word(platform: &str) -> Option<String> {
    if let Some(system) = platform.strip_prefix("rom:") {
        return Some(rom_system_name(system));
    }
    // a runtime named with its version, e.g. love:11.5
    if let Some((flavor, version)) = platform.split_once(':') {
        return scanned_platform_word(flavor).map(|word| format!("{word} {version}"));
    }
    let word = match platform {
        "linux-arm" | "linux-arm64" | "linux-arm64-handheld" => "Linux ARM",
        p if p.starts_with("windows-") => "Windows",
        p if p.starts_with("osx-") => "macOS",
        p if p.starts_with("linux-") => "Linux",
        "html" => "Web",
        "love" => "LÖVE",
        "godot-pck" => "Godot",
        "gamemaker-data" => "GameMaker",
        "renpy" => "Ren'Py",
        p if p.starts_with("rpgmaker-") => "RPG Maker",
        "pico8-cart" => "PICO-8",
        "tic80-cart" => "TIC-80",
        "swf" => "Flash",
        "jar" => "Java",
        "dos" => "DOS",
        "ags" => "AGS",
        "doom-wad" => "Doom WAD",
        "playdate" => "Playdate",
        "solarus-quest" => "Solarus",
        _ => return None,
    };
    Some(word.to_string())
}

fn rom_system_name(system: &str) -> String {
    let name = match system {
        "nes" => "NES",
        "snes" => "SNES",
        "gb" => "Game Boy",
        "gbc" => "Game Boy Color",
        "gba" => "Game Boy Advance",
        "nds" => "Nintendo DS",
        "3ds" => "Nintendo 3DS",
        "n64" => "Nintendo 64",
        "md" => "Mega Drive",
        "32x" => "32X",
        "sms" => "Master System",
        "gg" => "Game Gear",
        "segacd" => "Sega CD",
        "saturn" => "Saturn",
        "dreamcast" => "Dreamcast",
        "pce" => "PC Engine",
        "lynx" => "Lynx",
        "ngp" => "Neo Geo Pocket",
        "a26" => "Atari 2600",
        "c64" => "C64",
        "amiga" => "Amiga",
        "psx" => "PlayStation",
        "ps2" => "PlayStation 2",
        "psp" => "PSP",
        "pocket" => "Analogue Pocket",
        other => return format!("{} ROM", other.to_uppercase()),
    };
    name.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    Library,
    /// One game, with one of its buttons focused, or one of its
    /// screenshots while `shot` is set. The button stays for coming back
    /// up from the screenshots.
    Game {
        id: i64,
        button: usize,
        shot: Option<usize>,
    },
    /// The types "Playable here" includes, with a row focused: 0 is
    /// Everything, then the types in [`playable_types`] order.
    PlayableTypes {
        row: usize,
    },
}

/// A collection with the games butler has for it.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionGames {
    pub collection: Collection,
    /// The games fetched so far, in collection order.
    pub games: Vec<std::sync::Arc<Game>>,
    /// Where the next page starts, while there is one.
    pub next_cursor: Option<String>,
    /// A fresh copy of the games is on its way.
    pub refreshing: bool,
}

/// The top-level screens, switched with the bumpers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    #[default]
    Library,
    Collections,
    Downloads,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Library, Tab::Collections, Tab::Downloads];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::Collections => "Collections",
            Tab::Downloads => "Downloads",
        }
    }

    pub fn next(self, step: i32) -> Tab {
        let len = Self::ALL.len() as i32;
        let index = Self::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32;
        Self::ALL[((index + step).rem_euclid(len)) as usize]
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn get(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
    /// To the first item in the row.
    Home,
    /// To the last item in the row.
    End,
    /// A screenful of rows up.
    PageUp,
    /// A screenful of rows down.
    PageDown,
    /// To the first row.
    Top,
    /// To the last row.
    Bottom,
}

/// What the interface asked for while drawing. Applied after the frame so
/// views never mutate state they are reading.
#[derive(Debug, Clone)]
pub enum Action {
    MoveFocus(Direction),
    /// Focus the nth owned game, scrolling to it; for scripted screenshots.
    FocusIndex(usize),
    /// Focus a tile without scrolling to it; the pointer is already there.
    FocusTile {
        row: usize,
        col: usize,
    },
    /// The row is near its end and has more games to fetch.
    MoreGames {
        row: usize,
    },
    /// Guide, Start or Escape: bring the window up over a running game
    /// and open or close the menu drawer.
    Menu,
    /// Focus a menu drawer item; the pointer is already there.
    MenuFocus(usize),
    /// Close the window; the hosts shut the backend down on the way out.
    Quit,
    /// Focus a detail-page button; the pointer is already there.
    FocusButton(usize),
    /// Focus a download row's button; the pointer is already there.
    FocusDownload {
        row: usize,
        button: usize,
    },
    /// Focus a button on the tab's toolbar; the pointer is already there.
    FocusToolbar(usize),
    Activate,
    Back,
    Open(Page),
    Play {
        cave_id: String,
    },
    /// Kill the running game.
    QuitGame {
        cave_id: String,
    },
    /// Get the window out of the running game's way.
    BackToGame,
    /// Answer the open prompt with a choice, or dismiss it with `None`.
    Answer {
        prompt: u64,
        choice: Option<usize>,
    },
    /// Focus a prompt button; the pointer is already there.
    PromptFocus(usize),
    /// Focus a row of the compatibility report; the pointer is already
    /// there.
    ReportFocus(usize),
    /// Ask how the game in this cave runs.
    Report {
        cave_id: String,
    },
    /// List what else there is to do with the game in this cave.
    GameOptions {
        cave_id: String,
    },
    /// Hide games with no upload for this device, on every tab.
    SetPlayableOnly(bool),
    /// Include or leave out one type in "Playable here".
    TogglePlayableType(String),
    /// Leave out every type but this one.
    OnlyPlayableType(String),
    /// Include every type again.
    AllPlayableTypes,
    /// Focus a row on the Playable types page; the pointer is already there.
    FocusTypeRow(usize),
    SetTab(Tab),
    /// Narrow the Collections tab to installed games, or show everything.
    SetCollectionsInstalledOnly(bool),
    /// The sign-in page's checkbox: report what device this is.
    SetShareDeviceInfo(bool),
    /// Step through the tabs, wrapping.
    CycleTab(i32),
    /// Y on a pad: in and out of the filters on the library, the QR code
    /// on a game's page.
    Secondary,
    /// Slash on a keyboard: the search box on the library, the QR code
    /// on a game's page.
    Search,
    /// Show the open game's page as a QR code.
    ShowQr,
    /// Close the QR code.
    HideQr,
    /// Open the game page's screenshot at this index full screen.
    ViewScreenshot(usize),
    /// Close the full screen screenshot.
    CloseScreenshot,
    /// Leave the search box, keeping its text; focus goes to the results.
    SearchDone,
    ClearSearch,
    Install {
        game_id: i64,
    },
    /// Discard the game's download, whether running or failed.
    CancelInstall {
        game_id: i64,
    },
    RetryInstall {
        game_id: i64,
    },
    /// Drop finished downloads from the Downloads tab, as butler keeps
    /// them listed until asked.
    ClearFinished,
    /// Queue the update butler found for this cave.
    Update {
        cave_id: String,
    },
    /// Queue every direct update butler found.
    UpdateAll,
    /// Ask butler for updates now.
    CheckUpdates,
    /// Refetch the owned list and collections from itch.io.
    RefreshLibrary,
    /// Forget the signed-in profile and show the sign-in page.
    ChangeUser,
    /// Open the dialog for updating zitch itself, checking for a release.
    SelfUpdate,
    Uninstall {
        cave_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scanned_platforms_match_device() {
        let device = strings(&["love", "rom:gba", "linux-arm64-handheld"]);
        let love = strings(&["love:11.4", "love:11.5"]);
        let runs = |scanned: &[&str]| any_runs_here(&strings(scanned), &device, &[], &love);
        assert!(runs(&["windows-amd64", "rom:gba"]));
        assert!(runs(&["linux-arm64", "linux-arm64-handheld"]));
        assert!(runs(&["love:11.4"]));
        assert!(!runs(&["love"]));
        assert!(!runs(&["love:12.0"]));
        assert!(!runs(&["linux-arm64"]));
        assert!(!runs(&["rom:snes"]));
        assert!(!runs(&[]));
    }

    #[test]
    fn hidden_types_do_not_count() {
        let device = strings(&["love", "rom:gba"]);
        let hidden = strings(&["rom:gba"]);
        let love = strings(&["love:11.5"]);
        assert!(!any_runs_here(
            &strings(&["rom:gba"]),
            &device,
            &hidden,
            &love
        ));
        assert!(any_runs_here(
            &strings(&["rom:gba", "love:11.5"]),
            &device,
            &hidden,
            &love
        ));
        assert_eq!(
            wanted_platforms(&device, &strings(&["love"]), &love),
            ["rom:gba"]
        );
    }

    #[test]
    fn hiding_every_type_hides_none() {
        let device = strings(&["love", "rom:gba"]);
        let hidden = strings(&["love", "rom:gba"]);
        assert_eq!(shown_types(&device, &hidden).len(), 2);
        assert!(any_runs_here(&strings(&["rom:gba"]), &device, &hidden, &[]));
    }

    #[test]
    fn scanned_platforms_in_words() {
        let device = strings(&["pico8-cart", "linux-arm64-handheld"]);
        let words = platform_words_here(
            &strings(&[
                "linux-amd64",
                "linux-arm64",
                "linux-arm64-handheld",
                "osx-amd64",
                "osx-arm64",
                "pico8-cart",
                "script",
                "rom:pocket",
                "love:11.5",
                "godot-pck:4.2",
            ]),
            |p| device.iter().any(|d| d == p),
        );
        let expect = [
            ("Linux", false),
            ("Linux ARM", true),
            ("macOS", false),
            ("PICO-8", true),
            ("Analogue Pocket", false),
            ("LÖVE 11.5", false),
            ("Godot 4.2", false),
        ];
        assert_eq!(
            words,
            expect
                .iter()
                .map(|(w, h)| (w.to_string(), *h))
                .collect::<Vec<_>>()
        );
    }
}
