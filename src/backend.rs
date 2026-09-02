//! The butlerd conversation, on its own threads. The interface sends
//! [`Command`]s and polls [`Event`]s; nothing here blocks a frame.
//!
//! Installs go through butler's download queue the way the itch app does
//! it: `Install.Queue` records the download, one long-lived
//! `Downloads.Drive` call works the queue and reports progress, and
//! `Downloads.Discard` cancels. butler then owns staging folders, resume
//! after a restart, and ordering.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::butlerd::types::{
    AcceptLicenseResult, AllowSandboxSetupResult, AnyNotification, AnyServerRequest,
    DownloadReason, DownloadsClearFinishedParams, DownloadsDiscardParams,
    DownloadsDriveCancelParams, DownloadsDriveParams, DownloadsListParams, DownloadsRetryParams,
    FetchCavesParams, FetchGameUploadsParams, FetchProfileOwnedKeysParams, HTMLLaunchResult,
    InstallLocationsAddParams, InstallLocationsListParams, InstallQueueParams, LaunchParams,
    PickManifestActionResult, PrereqsFailedResult, ProfileListParams, ProfileLoginWithAPIKeyParams,
    ProfileUseSavedLoginParams, ShellLaunchResult, URLLaunchResult, UninstallPerformParams,
};
use crate::butlerd::{Client, Daemon, Incoming};
use crate::model::{Cave, Download, DownloadProgress, Game, Profile, Prompt, UserExt};

pub struct Config {
    pub butler: PathBuf,
    pub dbpath: PathBuf,
    /// Used for `Profile.LoginWithAPIKey` when no saved profile exists.
    pub api_key: Option<String>,
    /// Where games go when the database has no install location yet.
    pub install_dir: PathBuf,
    /// Where butler keeps prerequisite installers (DirectX, .NET, ...).
    pub prereqs_dir: PathBuf,
}

pub enum Command {
    Install {
        game: Box<Game>,
    },
    /// Discard a queued, running, or failed download.
    Discard {
        download_id: String,
    },
    Retry {
        download_id: String,
    },
    Uninstall {
        cave_id: String,
    },
    Launch {
        cave_id: String,
    },
    /// The user's pick for a [`Event::Prompt`], or `None` when dismissed.
    Answer {
        prompt: u64,
        choice: Option<usize>,
    },
    Shutdown,
}

pub enum Event {
    /// A one-line description of what the backend is doing.
    Status(String),
    SignedIn(Profile),
    OwnedGames(Vec<Game>),
    /// Every installed game known to this database.
    Caves(Vec<Cave>),
    /// The whole download queue, after anything changed it.
    Downloads(Vec<Download>),
    DownloadProgress {
        download_id: String,
        progress: DownloadProgress,
    },
    DownloadFinished(Download),
    DownloadErrored(Download),
    UninstallFinished {
        cave_id: String,
        result: Result<(), String>,
    },
    /// The game process is up.
    LaunchRunning {
        cave_id: String,
    },
    /// The `Launch` call returned; the game has exited or never started.
    LaunchFinished {
        cave_id: String,
        result: Result<(), String>,
    },
    /// A question to show until [`Event::PromptClosed`] or an answer.
    Prompt(Prompt),
    PromptClosed(u64),
    Error(String),
}

/// Repaints the window when the backend has news. egui only repaints on
/// input, so without this a reply would sit unseen until the mouse moved.
#[derive(Clone, Default)]
pub struct Waker(Arc<Mutex<Option<egui::Context>>>);

impl Waker {
    pub fn attach(&self, ctx: &egui::Context) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx.clone());
    }

    pub fn wake(&self) {
        if let Some(ctx) = self.0.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            ctx.request_repaint();
        }
    }
}

pub struct Backend {
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Backend {
    pub fn spawn(config: Config, waker: Waker) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("zitch-backend".into())
            .spawn(move || {
                let emit = Emitter {
                    events: event_tx,
                    waker,
                };
                if let Err(error) = run(config, &emit, command_rx) {
                    log::error!("{error:#}");
                    emit.send(Event::Error(format!("{error:#}")));
                }
            })
            .expect("spawning backend thread");
        Self {
            commands: command_tx,
            events: event_rx,
            thread: Some(thread),
        }
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct Emitter {
    events: mpsc::Sender<Event>,
    waker: Waker,
}

impl Emitter {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        self.waker.wake();
    }

    fn status(&self, text: impl Into<String>) {
        let text = text.into();
        log::info!("{text}");
        self.send(Event::Status(text));
    }
}

fn run(config: Config, emit: &Emitter, commands: mpsc::Receiver<Command>) -> Result<()> {
    emit.status("Starting butler");
    let daemon = Arc::new(Daemon::spawn(&config.butler, &config.dbpath)?);
    let client = Client::connect(&daemon)?;
    emit.status(format!("Connected to butlerd at {}", daemon.address));

    let profile = sign_in(&client, &config, emit)?;
    let name = profile.user.as_ref().map_or("?", UserExt::name);
    emit.status(format!("Signed in as {name}"));
    emit.send(Event::SignedIn(profile.clone()));

    emit.status("Loading library");
    let games = owned_games(&client, profile.id, false)?;
    emit.send(Event::OwnedGames(games));
    emit.status("Library loaded");
    refresh_caves(&client, emit);
    refresh_downloads(&client, emit);

    let stopping = Arc::new(AtomicBool::new(false));
    let driver = spawn_driver(Arc::clone(&daemon), emit.clone(), Arc::clone(&stopping));
    let prompts = Prompts::default();
    let config = Arc::new(config);

    loop {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Command::Install { game }) => {
                if let Err(error) = queue_install(&client, &config, *game) {
                    log::error!("{error:#}");
                    emit.send(Event::Error(format!("{error:#}")));
                }
                refresh_downloads(&client, emit);
            }
            Ok(Command::Discard { download_id }) => {
                if let Err(error) = client.call(DownloadsDiscardParams { download_id }) {
                    log::warn!("discard: {error:#}");
                }
                refresh_downloads(&client, emit);
            }
            Ok(Command::Retry { download_id }) => {
                if let Err(error) = client.call(DownloadsRetryParams { download_id }) {
                    log::warn!("retry: {error:#}");
                }
                refresh_downloads(&client, emit);
            }
            Ok(Command::Uninstall { cave_id }) => {
                spawn_op(
                    format!("uninstall-{cave_id}"),
                    Arc::clone(&daemon),
                    emit.clone(),
                    move |client, emit| {
                        let result = client
                            .call(UninstallPerformParams {
                                cave_id: cave_id.clone(),
                                hard: None,
                            })
                            .map(|_| ())
                            .map_err(|e| format!("{e:#}"));
                        emit.send(Event::UninstallFinished { cave_id, result });
                        refresh_caves(client, emit);
                        Ok(())
                    },
                );
            }
            Ok(Command::Launch { cave_id }) => {
                let config = Arc::clone(&config);
                let prompts = prompts.clone();
                let profile_id = profile.id;
                spawn_op(
                    format!("launch-{cave_id}"),
                    Arc::clone(&daemon),
                    emit.clone(),
                    move |client, emit| {
                        let result = launch(client, &config, &prompts, profile_id, &cave_id, emit);
                        emit.send(Event::LaunchFinished {
                            cave_id,
                            // The innermost error is butler's own words.
                            result: result.map_err(|e| e.root_cause().to_string()),
                        });
                        // Play time and last-played change with every run.
                        refresh_caves(client, emit);
                        Ok(())
                    },
                );
            }
            Ok(Command::Answer { prompt, choice }) => prompts.answer(prompt, choice),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        for incoming in client.poll() {
            log_incoming(&client, incoming);
        }
    }

    stopping.store(true, Ordering::Relaxed);
    if let Err(error) = client.call(DownloadsDriveCancelParams {}) {
        log::debug!("stopping the download driver: {error:#}");
    }
    let _ = driver.join();
    Ok(())
}

/// Keeps one `Downloads.Drive` call up for the life of the process, on its
/// own connection so its notifications are unambiguous. butler works the
/// queue inside that call and idles when it is empty.
fn spawn_driver(
    daemon: Arc<Daemon>,
    emit: Emitter,
    stopping: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("downloads-driver".into())
        .spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let client = match Client::connect(&daemon) {
                    Ok(client) => client,
                    Err(error) => {
                        log::warn!("download driver: {error:#}");
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                };
                let result = client.call_streaming(DownloadsDriveParams {}, |incoming| {
                    drive_incoming(&client, &emit, incoming);
                });
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                match result {
                    Ok(_) => log::info!("download driver returned; restarting"),
                    Err(error) => log::warn!("download driver: {error:#}; restarting"),
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("spawning download driver")
}

fn drive_incoming(client: &Client, emit: &Emitter, incoming: Incoming) {
    let (method, params) = match incoming {
        Incoming::Notification { method, params } => (method, params),
        Incoming::Request { id, method, params } => {
            match AnyServerRequest::decode(&method, params) {
                Ok(request) => log::warn!("download driver asked {request:?}; not supported yet"),
                Err(error) => log::warn!("bad {method} request: {error}"),
            }
            let _ = client.reply_error(&id, -32601, "not supported by this client");
            return;
        }
    };
    match AnyNotification::decode(&method, params) {
        Ok(AnyNotification::DownloadsDriveProgress(n)) => {
            if let (Some(download), Some(progress)) = (n.download, n.progress) {
                emit.send(Event::DownloadProgress {
                    download_id: download.id,
                    progress,
                });
            }
        }
        Ok(AnyNotification::DownloadsDriveStarted(_))
        | Ok(AnyNotification::DownloadsDriveDiscarded(_)) => {
            refresh_downloads(client, emit);
        }
        Ok(AnyNotification::DownloadsDriveErrored(n)) => {
            if let Some(download) = n.download {
                emit.send(Event::DownloadErrored(download));
            }
            refresh_downloads(client, emit);
        }
        Ok(AnyNotification::DownloadsDriveFinished(n)) => {
            refresh_caves(client, emit);
            if let Some(download) = n.download {
                emit.send(Event::DownloadFinished(download));
            }
            // Finished entries only clutter the queue; the cave is the
            // record of the install now.
            if let Err(error) = client.call(DownloadsClearFinishedParams {}) {
                log::warn!("clearing finished downloads: {error:#}");
            }
            refresh_downloads(client, emit);
        }
        Ok(AnyNotification::Log(log)) => log::debug!("butler: {}", log.message),
        Ok(other) => log::debug!("{other:?}"),
        Err(error) => log::warn!("bad {method} notification: {error}"),
    }
}

fn log_incoming(client: &Client, incoming: Incoming) {
    match incoming {
        Incoming::Notification { method, params } => {
            match AnyNotification::decode(&method, params) {
                Ok(AnyNotification::Log(log)) => log::debug!("butler: {}", log.message),
                Ok(notification) => log::debug!("{notification:?}"),
                Err(error) => log::warn!("bad {method} notification: {error}"),
            }
        }
        Incoming::Request { id, method, params } => {
            match AnyServerRequest::decode(&method, params) {
                Ok(request) => log::warn!("unhandled server request {request:?}"),
                Err(error) => log::warn!("bad {method} request: {error}"),
            }
            let _ = client.reply_error(&id, -32601, "not supported by this client");
        }
    }
}

/// Questions in flight between an op thread and the interface. The op
/// thread blocks on its answer; the interface answers through a command.
#[derive(Clone, Default)]
struct Prompts {
    next_id: Arc<AtomicU64>,
    waiting: Arc<Mutex<HashMap<u64, mpsc::Sender<Option<usize>>>>>,
}

impl Prompts {
    /// Shows a prompt and waits for the pick. `None` means dismissed, or the
    /// interface went away.
    fn ask(&self, emit: &Emitter, title: &str, body: &str, choices: &[&str]) -> Option<usize> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        self.waiting
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, tx);
        emit.send(Event::Prompt(Prompt {
            id,
            title: title.to_string(),
            body: body.to_string(),
            choices: choices.iter().map(|c| c.to_string()).collect(),
            focus: 0,
        }));
        let choice = rx.recv().ok().flatten();
        self.waiting
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        emit.send(Event::PromptClosed(id));
        choice
    }

    fn answer(&self, id: u64, choice: Option<usize>) {
        let waiting = self.waiting.lock().unwrap_or_else(|p| p.into_inner());
        match waiting.get(&id) {
            Some(tx) => {
                let _ = tx.send(choice);
            }
            None => log::debug!("answer to unknown prompt {id}"),
        }
    }
}

/// Runs a game and stays in the call until it exits, answering whatever
/// butler asks along the way.
fn launch(
    client: &Client,
    config: &Config,
    prompts: &Prompts,
    profile_id: i64,
    cave_id: &str,
    emit: &Emitter,
) -> Result<()> {
    std::fs::create_dir_all(&config.prereqs_dir)
        .with_context(|| format!("creating {}", config.prereqs_dir.display()))?;
    let params = LaunchParams {
        cave_id: cave_id.to_string(),
        prereqs_dir: Some(config.prereqs_dir.to_string_lossy().into_owned()),
        profile_id: Some(profile_id),
        ..Default::default()
    };
    client.call_streaming(params, |incoming| match incoming {
        Incoming::Notification { method, params } => {
            match AnyNotification::decode(&method, params) {
                Ok(AnyNotification::LaunchRunning(_)) => emit.send(Event::LaunchRunning {
                    cave_id: cave_id.to_string(),
                }),
                Ok(AnyNotification::LaunchExited(_)) => log::info!("game exited"),
                Ok(AnyNotification::PrereqsStarted(n)) => {
                    emit.status(format!("Installing {} prerequisites", n.tasks.len()))
                }
                Ok(AnyNotification::PrereqsTaskState(n)) => emit.status(format!(
                    "{}: {:?} {:.0}%",
                    n.name,
                    n.status,
                    n.progress * 100.0
                )),
                Ok(AnyNotification::Log(log)) => log::debug!("butler: {}", log.message),
                Ok(other) => log::debug!("{other:?}"),
                Err(error) => log::warn!("bad {method} notification: {error}"),
            }
        }
        Incoming::Request { id, method, params } => {
            let request = match AnyServerRequest::decode(&method, params) {
                Ok(request) => request,
                Err(error) => {
                    log::warn!("bad {method} request: {error}");
                    let _ = client.reply_error(&id, -32602, &error.to_string());
                    return;
                }
            };
            let outcome = answer_launch_request(client, prompts, emit, &id, request);
            if let Err(error) = outcome {
                log::warn!("answering {method}: {error:#}");
                let _ = client.reply_error(&id, -32603, &format!("{error:#}"));
            }
        }
    })?;
    Ok(())
}

fn answer_launch_request(
    client: &Client,
    prompts: &Prompts,
    emit: &Emitter,
    id: &serde_json::Value,
    request: AnyServerRequest,
) -> Result<()> {
    match request {
        AnyServerRequest::PickManifestAction(p) => {
            let names: Vec<&str> = p.actions.iter().map(|a| a.name.as_str()).collect();
            let picked = if names.len() == 1 {
                Some(0)
            } else {
                prompts.ask(emit, "What do you want to launch?", "", &names)
            };
            match picked {
                Some(index) => client.reply(
                    id,
                    PickManifestActionResult {
                        index: index as i64,
                    },
                ),
                None => client.reply_error(id, 499, "launch cancelled"),
            }
        }
        AnyServerRequest::AcceptLicense(p) => {
            let accept =
                prompts.ask(emit, "License agreement", &p.text, &["Accept", "Decline"]) == Some(0);
            client.reply(id, AcceptLicenseResult { accept })
        }
        AnyServerRequest::ShellLaunch(p) => {
            log::info!("opening {}", p.item_path);
            open::that_detached(&p.item_path)?;
            client.reply(id, ShellLaunchResult {})
        }
        AnyServerRequest::URLLaunch(p) => {
            log::info!("opening {}", p.url);
            open::that_detached(&p.url)?;
            client.reply(id, URLLaunchResult {})
        }
        AnyServerRequest::HTMLLaunch(_) => {
            // TODO: serve the folder and open a window for it.
            emit.send(Event::Error("HTML games are not supported yet".into()));
            let _: Option<HTMLLaunchResult> = None;
            client.reply_error(id, 501, "HTML games are not supported yet")
        }
        AnyServerRequest::AllowSandboxSetup(_) => {
            client.reply(id, AllowSandboxSetupResult { allow: false })
        }
        AnyServerRequest::PrereqsFailed(p) => {
            let go_on = prompts.ask(
                emit,
                "Prerequisites failed to install",
                &p.error,
                &["Launch anyway", "Cancel"],
            ) == Some(0);
            client.reply(id, PrereqsFailedResult { r#continue: go_on })
        }
        other => {
            log::warn!("unhandled server request {other:?}");
            client.reply_error(id, -32601, "not supported by this client")
        }
    }
}

/// Runs a butlerd call on its own connection and thread, so the main loop
/// keeps turning while it works.
fn spawn_op<F>(name: String, daemon: Arc<Daemon>, emit: Emitter, op: F)
where
    F: FnOnce(&Client, &Emitter) -> Result<()> + Send + 'static,
{
    let outer_emit = emit.clone();
    let outer_name = name.clone();
    let result = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let client = match Client::connect(&daemon) {
                Ok(client) => client,
                Err(error) => {
                    emit.send(Event::Error(format!("{error:#}")));
                    return;
                }
            };
            if let Err(error) = op(&client, &emit) {
                log::error!("{name}: {error:#}");
                emit.send(Event::Error(format!("{error:#}")));
            }
        });
    if let Err(error) = result {
        outer_emit.send(Event::Error(format!("spawning {outer_name}: {error}")));
    }
}

fn refresh_caves(client: &Client, emit: &Emitter) {
    match all_caves(client) {
        Ok(caves) => {
            log::info!("{} installed games", caves.len());
            emit.send(Event::Caves(caves));
        }
        Err(error) => log::warn!("refreshing caves: {error:#}"),
    }
}

fn refresh_downloads(client: &Client, emit: &Emitter) {
    match client.call(DownloadsListParams {}) {
        Ok(list) => emit.send(Event::Downloads(list.downloads)),
        Err(error) => log::warn!("listing downloads: {error:#}"),
    }
}

/// Puts a game on the download queue; the driver takes it from there.
fn queue_install(client: &Client, config: &Config, game: Game) -> Result<()> {
    let location = install_location(client, config)?;
    let uploads = client
        .call(FetchGameUploadsParams {
            game_id: game.id,
            compatible: true,
            fresh: Some(true),
        })?
        .uploads;
    // TODO: let the user pick when there is more than one.
    let upload = uploads
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("{} has no download for this computer", game.title))?;
    let queued = client.call(InstallQueueParams {
        game: Some(game.clone()),
        upload: Some(upload),
        install_location_id: Some(location),
        reason: Some(DownloadReason::Install),
        queue_download: Some(true),
        ..Default::default()
    })?;
    log::info!("queued {} as download {}", game.title, queued.id);
    Ok(())
}

/// The first install location, created from the config when there is none.
fn install_location(client: &Client, config: &Config) -> Result<String> {
    let locations = client
        .call(InstallLocationsListParams {})?
        .install_locations;
    if let Some(first) = locations.into_iter().next() {
        return Ok(first.id);
    }
    std::fs::create_dir_all(&config.install_dir)
        .with_context(|| format!("creating {}", config.install_dir.display()))?;
    let added = client.call(InstallLocationsAddParams {
        id: None,
        path: config.install_dir.to_string_lossy().into_owned(),
    })?;
    log::info!("added install location {}", config.install_dir.display());
    added
        .install_location
        .map(|location| location.id)
        .ok_or_else(|| anyhow!("Install.Locations.Add returned no location"))
}

fn sign_in(client: &Client, config: &Config, emit: &Emitter) -> Result<Profile> {
    let mut saved = client.call(ProfileListParams {})?.profiles;
    saved.sort_by(|a, b| b.last_connected.cmp(&a.last_connected));
    if let Some(entry) = saved.first() {
        emit.status("Using saved login");
        let result = client.call(ProfileUseSavedLoginParams {
            profile_id: entry.id,
        })?;
        return result
            .profile
            .ok_or_else(|| anyhow!("saved login returned no profile"));
    }
    let Some(api_key) = &config.api_key else {
        bail!(
            "no saved profile in {}; pass --api-key-file or set ZITCH_API_KEY to sign in once",
            config.dbpath.display()
        );
    };
    emit.status("Signing in with API key");
    let result = client
        .call(ProfileLoginWithAPIKeyParams {
            api_key: api_key.clone(),
        })
        .context("API key login")?;
    result
        .profile
        .ok_or_else(|| anyhow!("login returned no profile"))
}

fn owned_games(client: &Client, profile_id: i64, fresh: bool) -> Result<Vec<Game>> {
    let mut games = Vec::new();
    let mut cursor = None;
    loop {
        let page = client.call(FetchProfileOwnedKeysParams {
            profile_id,
            limit: Some(100),
            cursor: cursor.take(),
            fresh: Some(fresh),
            ..Default::default()
        })?;
        // The cache is empty on first run and answers stale with no items;
        // a fresh fetch fills it.
        if page.stale == Some(true) && !fresh && games.is_empty() && page.items.is_empty() {
            return owned_games(client, profile_id, true);
        }
        games.extend(page.items.into_iter().filter_map(|key| key.game));
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(games)
}

fn all_caves(client: &Client) -> Result<Vec<Cave>> {
    let mut caves = Vec::new();
    let mut cursor = None;
    loop {
        let page = client.call(FetchCavesParams {
            limit: Some(100),
            cursor: cursor.take(),
            ..Default::default()
        })?;
        caves.extend(page.items);
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(caves)
}
