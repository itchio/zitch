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
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::butlerd::types::{
    AcceptLicenseResult, AllowSandboxSetupResult, AnyNotification, AnyServerRequest,
    CheckUpdateParams, DownloadReason, DownloadsClearFinishedParams, DownloadsDiscardParams,
    DownloadsDriveCancelParams, DownloadsDriveParams, DownloadsListParams, DownloadsRetryParams,
    FetchCavesParams, FetchCollectionGamesParams, FetchGameUploadsParams,
    FetchProfileCollectionsParams, FetchProfileOwnedKeysParams, HTMLLaunchResult,
    InstallLocationsAddParams, InstallLocationsListParams, InstallQueueParams, LaunchParams,
    PickManifestActionResult, PrereqsFailedResult, ProfileListParams, ProfileLoginWithAPIKeyParams,
    ProfileUseSavedLoginParams, ShellLaunchResult, URLLaunchResult, UninstallPerformParams,
};
use crate::butlerd::{Client, Daemon, Incoming, is_offline};
use crate::model::{
    Cave, CollectionGames, Download, DownloadProgress, Game, GameUpdate, Profile, Prompt,
    UploadExt, UserExt,
};

pub struct Config {
    pub butler: PathBuf,
    pub dbpath: PathBuf,
    /// Used for `Profile.LoginWithAPIKey` when no saved profile exists.
    pub api_key: Option<String>,
    /// A saved profile to use instead of the most recent one.
    pub profile_id: Option<i64>,
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
    /// Asks first; the title names the game in the question.
    Uninstall {
        cave_id: String,
        title: String,
    },
    Launch {
        cave_id: String,
    },
    /// Queue an update butler reported; the first choice is the one taken.
    Update {
        update: Box<GameUpdate>,
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
    /// The profile's collections with their games, in butler's order.
    Collections(Vec<CollectionGames>),
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
    /// Updates butler found for installed games, one per cave.
    Updates(Vec<GameUpdate>),
    /// A question to show until [`Event::PromptClosed`] or an answer.
    Prompt(Prompt),
    PromptClosed(u64),
    /// Whether butler can reach itch.io. Starts unknown and is reported once
    /// the first network call settles, then whenever it changes.
    Online(bool),
    Error(String),
}

/// The current daemon, replaced when it has to be restarted. Everything
/// that opens a connection goes through here so it finds the live one.
type Link = Arc<Mutex<Arc<Daemon>>>;

fn current(link: &Link) -> Arc<Daemon> {
    Arc::clone(&link.lock().unwrap_or_else(|p| p.into_inner()))
}

fn connect(link: &Link) -> Result<Client> {
    Client::connect(&current(link))
}

/// How often to look for the network again while offline.
const PROBE_EVERY: Duration = Duration::from_secs(60);
/// How long to wait before trying to start butler again after it died.
const RESPAWN_DELAY: Duration = Duration::from_secs(5);

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
    let link: Link = Arc::new(Mutex::new(Arc::new(Daemon::spawn(
        &config.butler,
        &config.dbpath,
    )?)));
    let mut client = connect(&link)?;
    emit.status(format!(
        "Connected to butlerd at {}",
        current(&link).address
    ));

    let profile = sign_in(&client, &config, emit)?;
    let name = profile.user.as_ref().map_or("?", UserExt::name);
    emit.status(format!("Signed in as {name}"));
    emit.send(Event::SignedIn(profile.clone()));

    emit.status("Loading library");
    // butler answers from its cache, so this works offline too.
    let (games, stale) = owned_games(&client, profile.id, false)?;
    emit.send(Event::OwnedGames(games));
    emit.status("Library loaded");
    let (collections, collections_stale) = collections(&client, profile.id, false)?;
    emit.send(Event::Collections(collections));
    let stale = stale || collections_stale;
    refresh_caves(&client, emit);
    refresh_downloads(&client, emit);

    let stopping = Arc::new(AtomicBool::new(false));
    let driver = spawn_driver(Arc::clone(&link), emit.clone(), Arc::clone(&stopping));
    let prompts = Prompts::default();
    let config = Arc::new(config);
    let sync = Sync {
        profile_id: profile.id,
        online: Arc::new(AtomicBool::new(true)),
        stale: Arc::new(AtomicBool::new(stale)),
    };
    sync.spawn(&link, emit);
    let mut next_probe = Instant::now() + PROBE_EVERY;

    loop {
        // Anything can take butler down: the kernel's memory killer on a
        // small device, a crash, a firmware reaping background processes.
        if !current(&link).alive() {
            emit.status("butler exited; restarting");
            match Daemon::spawn(&config.butler, &config.dbpath) {
                Ok(daemon) => {
                    *link.lock().unwrap_or_else(|p| p.into_inner()) = Arc::new(daemon);
                    client = connect(&link)?;
                    if let Err(error) = client.call(ProfileUseSavedLoginParams {
                        profile_id: profile.id,
                    }) {
                        log::warn!("signing in again: {error:#}");
                    }
                    emit.status("butler restarted");
                    refresh_caves(&client, emit);
                    refresh_downloads(&client, emit);
                    // Whatever the old daemon was checking died with it.
                    sync.spawn(&link, emit);
                }
                Err(error) => {
                    emit.send(Event::Error(format!("restarting butler: {error:#}")));
                    std::thread::sleep(RESPAWN_DELAY);
                    continue;
                }
            }
        }
        if !sync.online.load(Ordering::Relaxed) && Instant::now() >= next_probe {
            sync.spawn(&link, emit);
            next_probe = Instant::now() + PROBE_EVERY;
        }
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
            Ok(Command::Uninstall { cave_id, title }) => {
                let prompts = prompts.clone();
                spawn_op(
                    format!("uninstall-{cave_id}"),
                    Arc::clone(&link),
                    emit.clone(),
                    move |client, emit| {
                        // Cancel comes first so a reflex press keeps the game.
                        let confirmed = prompts.ask(
                            emit,
                            &format!("Uninstall {title}?"),
                            "Removes the installed files. Anything the game saved elsewhere stays.",
                            &["Cancel", "Uninstall"],
                        ) == Some(1);
                        if !confirmed {
                            return Ok(());
                        }
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
                    Arc::clone(&link),
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
            Ok(Command::Update { update }) => {
                if update.direct {
                    if let Err(error) = queue_update(&client, &update, 0) {
                        log::error!("{error:#}");
                        emit.send(Event::Error(format!("{error:#}")));
                    }
                    refresh_downloads(&client, emit);
                } else {
                    // Indirect updates are butler's guesses, so the user
                    // picks. Asking blocks on the answer, which arrives
                    // through this loop.
                    let prompts = prompts.clone();
                    spawn_op(
                        format!("update-{}", update.cave_id),
                        Arc::clone(&link),
                        emit.clone(),
                        move |client, emit| {
                            if let Some(choice) = pick_update(&prompts, emit, &update) {
                                queue_update(client, &update, choice)?;
                                refresh_downloads(client, emit);
                            }
                            Ok(())
                        },
                    );
                }
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
    link: Link,
    emit: Emitter,
    stopping: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("downloads-driver".into())
        .spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let client = match connect(&link) {
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
/// The work that needs itch.io: a fresh owned list when butler's cache is
/// stale, and the update check. Runs at startup and again whenever the
/// network comes back, and is what decides whether we are online.
#[derive(Clone)]
struct Sync {
    profile_id: i64,
    online: Arc<AtomicBool>,
    /// Whether butler flagged the cached owned list stale, so a fresh
    /// fetch is still owed.
    stale: Arc<AtomicBool>,
}

impl Sync {
    fn spawn(&self, link: &Link, emit: &Emitter) {
        let sync = self.clone();
        spawn_op(
            "sync".into(),
            Arc::clone(link),
            emit.clone(),
            move |client, emit| match sync.run(client, emit) {
                Ok(()) => Ok(()),
                Err(error) if is_offline(&error) => {
                    sync.set_online(emit, false);
                    Ok(())
                }
                Err(error) => Err(error),
            },
        );
    }

    fn run(&self, client: &Client, emit: &Emitter) -> Result<()> {
        // A one-item fresh fetch is the cheapest call that must reach the
        // API, so it doubles as the network probe.
        client.call(FetchProfileOwnedKeysParams {
            profile_id: self.profile_id,
            limit: Some(1),
            fresh: Some(true),
            ..Default::default()
        })?;
        self.set_online(emit, true);
        if self.stale.load(Ordering::Relaxed) {
            // The cached list is shown already; the itch app also refetches
            // when butler flags it stale, so new purchases appear.
            let (games, _) = owned_games(client, self.profile_id, true)?;
            emit.send(Event::OwnedGames(games));
            let (collections, _) = collections(client, self.profile_id, true)?;
            emit.send(Event::Collections(collections));
            self.stale.store(false, Ordering::Relaxed);
        }
        check_updates(client, emit)
    }

    fn set_online(&self, emit: &Emitter, online: bool) {
        if self.online.swap(online, Ordering::Relaxed) != online {
            log::info!("{}", if online { "online" } else { "offline" });
        }
        emit.send(Event::Online(online));
    }
}

fn check_updates(client: &Client, emit: &Emitter) -> Result<()> {
    let result = client.call(CheckUpdateParams::default())?;
    for warning in &result.warnings {
        log::warn!("update check: {warning}");
    }
    log::info!("{} updates available", result.updates.len());
    // Same-channel updates apply themselves, as in the itch app; the
    // rest wait for the user to pick.
    for update in result.updates.iter().filter(|u| u.direct) {
        if let Err(error) = queue_update(client, update, 0) {
            log::warn!("queueing update: {error:#}");
        }
    }
    emit.send(Event::Updates(result.updates));
    refresh_downloads(client, emit);
    Ok(())
}

fn spawn_op<F>(name: String, link: Link, emit: Emitter, op: F)
where
    F: FnOnce(&Client, &Emitter) -> Result<()> + Send + 'static,
{
    let outer_emit = emit.clone();
    let outer_name = name.clone();
    let result = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let client = match connect(&link) {
                Ok(client) => client,
                Err(error) => {
                    emit.send(Event::Error(format!("{error:#}")));
                    return;
                }
            };
            if let Err(error) = op(&client, &emit) {
                // A daemon that died mid-call is restarted by the main loop
                // and reported there; the call's own failure is noise.
                if !current(&link).alive() {
                    log::warn!("{name}: {error:#} (butler exited)");
                } else {
                    log::error!("{name}: {error:#}");
                    emit.send(Event::Error(format!("{error:#}")));
                }
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

/// Asks which of an indirect update's uploads to install. `None` when the
/// user backs out.
fn pick_update(prompts: &Prompts, emit: &Emitter, update: &GameUpdate) -> Option<usize> {
    let title = update.game.as_ref().map_or("game", |g| g.title.as_str());
    let names: Vec<String> = update
        .choices
        .iter()
        .map(|c| {
            c.upload
                .as_ref()
                .map_or("upload", UploadExt::name)
                .to_string()
        })
        .collect();
    let mut choices: Vec<&str> = names.iter().map(String::as_str).collect();
    choices.push("Cancel");
    let body = format!(
        "Newer uploads for {title} appeared after it was installed. They may be a new \
         version or something else, like extra content. Installing one replaces the \
         current install."
    );
    let picked = prompts.ask(emit, "Update?", &body, &choices)?;
    (picked < names.len()).then_some(picked)
}

fn queue_update(client: &Client, update: &GameUpdate, choice: usize) -> Result<()> {
    let choice = update
        .choices
        .get(choice)
        .ok_or_else(|| anyhow!("update for cave {} has no choice {choice}", update.cave_id))?;
    let queued = client.call(InstallQueueParams {
        cave_id: Some(update.cave_id.clone()),
        game: update.game.clone(),
        upload: choice.upload.clone(),
        build: choice.build.clone(),
        reason: Some(DownloadReason::Update),
        queue_download: Some(true),
        fast_queue: Some(true),
        ..Default::default()
    })?;
    log::info!(
        "queued update for {} as download {}",
        update.game.as_ref().map_or("?", |g| g.title.as_str()),
        queued.id
    );
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
    let chosen = match config.profile_id {
        Some(id) => Some(saved.iter().find(|p| p.id == id).ok_or_else(|| {
            let choices: Vec<String> = saved
                .iter()
                .map(|p| {
                    format!(
                        "{} ({})",
                        p.id,
                        p.user.as_ref().map_or("?", |u| u.username.as_str())
                    )
                })
                .collect();
            anyhow!(
                "no saved profile with id {id}; saved profiles: {}",
                if choices.is_empty() {
                    "none".to_string()
                } else {
                    choices.join(", ")
                }
            )
        })?),
        None => saved.first(),
    };
    if let Some(entry) = chosen {
        emit.status(format!(
            "Using saved login for {}",
            entry.user.as_ref().map_or("?", |u| u.username.as_str())
        ));
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

/// The games the profile owns and whether butler's cache of them is stale.
fn owned_games(client: &Client, profile_id: i64, fresh: bool) -> Result<(Vec<Game>, bool)> {
    let mut games = Vec::new();
    let mut stale = false;
    let mut cursor = None;
    loop {
        let page = client.call(FetchProfileOwnedKeysParams {
            profile_id,
            limit: Some(100),
            cursor: cursor.take(),
            fresh: Some(fresh),
            ..Default::default()
        })?;
        stale |= page.stale == Some(true);
        games.extend(page.items.into_iter().filter_map(|key| key.game));
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok((games, stale && !fresh))
}

/// Long collections are cut here; the rows are for browsing, not
/// exhausting, and each page is a round trip to the API when fresh.
const COLLECTION_GAMES_MAX: usize = 300;

fn collections(
    client: &Client,
    profile_id: i64,
    fresh: bool,
) -> Result<(Vec<CollectionGames>, bool)> {
    let mut collections = Vec::new();
    let mut stale = false;
    let mut cursor = None;
    loop {
        let page = client.call(FetchProfileCollectionsParams {
            profile_id,
            limit: Some(100),
            cursor: cursor.take(),
            fresh: Some(fresh),
            ..Default::default()
        })?;
        stale |= page.stale == Some(true);
        collections.extend(page.items);
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    let mut shelves = Vec::with_capacity(collections.len());
    for collection in collections {
        let mut games = Vec::new();
        let mut cursor = None;
        loop {
            let page = client.call(FetchCollectionGamesParams {
                profile_id,
                collection_id: collection.id,
                limit: Some(100),
                cursor: cursor.take(),
                fresh: Some(fresh),
                ..Default::default()
            })?;
            stale |= page.stale == Some(true);
            games.extend(page.items.into_iter().filter_map(|item| item.game));
            match page.next_cursor {
                Some(next) if !next.is_empty() && games.len() < COLLECTION_GAMES_MAX => {
                    cursor = Some(next)
                }
                _ => break,
            }
        }
        shelves.push(CollectionGames { collection, games });
    }
    Ok((shelves, stale && !fresh))
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
