//! The butlerd conversation, on its own threads. The interface sends
//! [`Command`]s and polls [`Event`]s; nothing here blocks a frame.
//!
//! Installs go through butler's download queue the way the itch app does
//! it: `Install.Queue` records the download, one long-lived
//! `Downloads.Drive` call works the queue and reports progress, and
//! `Downloads.Discard` cancels. butler then owns staging folders, resume
//! after a restart, and ordering.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::butlerd::types::{
    AcceptLicenseResult, AllowSandboxSetupResult, AnyNotification, AnyServerRequest,
    CheckUpdateParams, CollectionGamesFilters, DownloadReason, DownloadsClearFinishedParams,
    DownloadsDiscardParams, DownloadsDriveCancelParams, DownloadsDriveParams, DownloadsListParams,
    DownloadsRetryParams, FetchCaveParams, FetchCavesParams, FetchCollectionGamesParams,
    FetchGameUploadsParams, FetchProfileCollectionsParams, FetchProfileOwnedKeysParams,
    HTMLLaunchResult, InstallLocationsAddParams, InstallLocationsListParams, InstallQueueParams,
    LaunchGetTargetsParams, LaunchParams, LaunchStrategy, LogLevel, PickManifestActionResult,
    PrereqsFailedResult, ProfileListParams, ProfileLoginWithAPIKeyParams,
    ProfileUseSavedLoginParams, RuntimeLaunchResult, ShellLaunchResult, URLLaunchResult,
    UninstallPerformParams, Upload, UploadType,
};
use crate::butlerd::{Cancel, Client, Daemon, Incoming, is_offline};
use crate::model::{
    Cave, CollectionGames, Download, DownloadProgress, Game, GameUpdate, LaunchFailure, Profile,
    Prompt, UploadExt, UserExt, human_size, upload_platform_names, upload_runs_here,
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
    /// Variables for the games butler launches, on top of our own. On
    /// muOS this is what routes their SDL to the screen (`muos::game_env`).
    pub game_env: Vec<(String, OsString)>,
}

pub enum Command {
    Install {
        game: Box<Game>,
    },
    /// The next page of a collection's games.
    CollectionPage {
        collection_id: i64,
        cursor: String,
    },
    /// Every installed game in each collection, from butler's own filter,
    /// so the answer covers pages not fetched yet.
    CollectionsInstalled {
        collection_ids: Vec<i64>,
    },
    /// Discard a queued, running, or failed download. With `confirm`, the
    /// game's title, the user is asked first and [`Event::Discarding`] says
    /// they agreed.
    Discard {
        download_id: String,
        confirm: Option<String>,
    },
    Retry {
        download_id: String,
    },
    /// Drop finished downloads, done or failed, from butler's queue.
    ClearFinished,
    /// Asks first; the title names the game in the question.
    Uninstall {
        cave_id: String,
        title: String,
    },
    Launch {
        cave_id: String,
    },
    /// Kill a running game by ending its launch call.
    QuitGame {
        cave_id: String,
    },
    /// Queue an update butler reported; the first choice is the one taken.
    Update {
        update: Box<GameUpdate>,
    },
    /// Check for updates now, on the user's request, and say what came of it.
    CheckUpdates,
    /// Refetch the owned list and collections whether or not butler thinks
    /// its cache is stale, and check for updates on the way.
    RefreshLibrary,
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
    CollectionsFailed(String),
    /// A background refresh failed for a reason other than being offline.
    /// What is on screen came from the cache and stays; the loop retries.
    SyncFailed(String),
    CollectionPage {
        collection_id: i64,
        games: Vec<Game>,
        next_cursor: Option<String>,
    },
    CollectionPageFailed {
        collection_id: i64,
        error: String,
    },
    CollectionsInstalled(Vec<(i64, Vec<Game>)>),
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
    /// Queueing the install never produced a download.
    InstallFailed {
        game_id: i64,
        error: String,
    },
    /// The user backed out of the upload picker.
    InstallDeclined {
        game_id: i64,
    },
    /// The user confirmed a cancel and the discard is under way.
    Discarding {
        download_id: String,
    },
    /// The download stays in the queue as it was.
    DiscardFailed {
        download_id: String,
        error: String,
    },
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
        result: Result<(), LaunchFailure>,
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
/// How often to ask itch.io for updates while online. The itch app's
/// intended cadence.
const UPDATE_EVERY: Duration = Duration::from_secs(30 * 60);
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
        &config.game_env,
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
    refresh_caves(&client, emit);
    refresh_downloads(&client, emit);

    let stopping = Arc::new(AtomicBool::new(false));
    let driver = spawn_driver(Arc::clone(&link), emit.clone(), Arc::clone(&stopping));
    let prompts = Prompts::default();
    let launches = Launches::default();
    let config = Arc::new(config);
    let sync = Sync {
        profile_id: profile.id,
        online: Arc::new(AtomicBool::new(true)),
        stale: Arc::new(AtomicBool::new(stale)),
        collections_loaded: Arc::new(AtomicBool::new(false)),
        collections_stale: Arc::new(AtomicBool::new(false)),
        retry: Arc::new(AtomicBool::new(false)),
    };
    sync.spawn(&link, emit);
    let mut next_probe = Instant::now() + PROBE_EVERY;
    let mut next_update_check = Instant::now() + UPDATE_EVERY;

    loop {
        // Anything can take butler down: the kernel's memory killer on a
        // small device, a crash, a firmware reaping background processes.
        if !current(&link).alive() {
            emit.status("butler exited; restarting");
            match Daemon::spawn(&config.butler, &config.dbpath, &config.game_env) {
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
                    if let Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) =
                        commands.recv_timeout(RESPAWN_DELAY)
                    {
                        break;
                    }
                    continue;
                }
            }
        }
        let due = !sync.online.load(Ordering::Relaxed) || sync.retry.load(Ordering::Relaxed);
        if due && Instant::now() >= next_probe {
            sync.spawn(&link, emit);
            next_probe = Instant::now() + PROBE_EVERY;
        }
        if Instant::now() >= next_update_check {
            next_update_check = Instant::now() + UPDATE_EVERY;
            // The sync pass checks on its own once the network is back.
            if sync.online.load(Ordering::Relaxed) {
                spawn_op(
                    "update-check".into(),
                    Arc::clone(&link),
                    emit.clone(),
                    |error| Event::SyncFailed(format!("{error:#}")),
                    |client, emit| check_updates(client, emit).map(|_| ()),
                );
            }
        }
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Command::Install { game }) => {
                // Picking an upload blocks on the answer, which arrives
                // through this loop, so the install runs on its own thread.
                let config = Arc::clone(&config);
                let prompts = prompts.clone();
                let game_id = game.id;
                spawn_op(
                    format!("install-{game_id}"),
                    Arc::clone(&link),
                    emit.clone(),
                    move |error| Event::InstallFailed {
                        game_id,
                        error: format!("{error:#}"),
                    },
                    move |client, emit| {
                        if !queue_install(client, &config, &prompts, emit, *game)? {
                            emit.send(Event::InstallDeclined { game_id });
                        }
                        refresh_downloads(client, emit);
                        Ok(())
                    },
                );
            }
            Ok(Command::CollectionPage {
                collection_id,
                cursor,
            }) => {
                let profile_id = profile.id;
                spawn_op(
                    format!("collection-{collection_id}"),
                    Arc::clone(&link),
                    emit.clone(),
                    move |error| Event::CollectionPageFailed {
                        collection_id,
                        error: format!("{error:#}"),
                    },
                    move |client, emit| {
                        let page =
                            collection_page(client, profile_id, collection_id, Some(cursor), None)?;
                        emit.send(Event::CollectionPage {
                            collection_id,
                            games: page.0,
                            next_cursor: page.1,
                        });
                        Ok(())
                    },
                );
            }
            Ok(Command::CollectionsInstalled { collection_ids }) => {
                let profile_id = profile.id;
                spawn_op(
                    "collections-installed".into(),
                    Arc::clone(&link),
                    emit.clone(),
                    |error| Event::Error(format!("{error:#}")),
                    move |client, emit| {
                        let filter = CollectionGamesFilters {
                            installed: true,
                            ..Default::default()
                        };
                        let mut lists = Vec::with_capacity(collection_ids.len());
                        for id in collection_ids {
                            let mut games = Vec::new();
                            let mut cursor = None;
                            loop {
                                let (page, next) = collection_page(
                                    client,
                                    profile_id,
                                    id,
                                    cursor.take(),
                                    Some(filter.clone()),
                                )?;
                                games.extend(page);
                                match next {
                                    Some(next) => cursor = Some(next),
                                    None => break,
                                }
                            }
                            lists.push((id, games));
                        }
                        emit.send(Event::CollectionsInstalled(lists));
                        Ok(())
                    },
                );
            }
            Ok(Command::Discard {
                download_id,
                confirm: None,
            }) => discard(&client, emit, download_id),
            Ok(Command::Discard {
                download_id,
                confirm: Some(title),
            }) => {
                let prompts = prompts.clone();
                spawn_op(
                    format!("discard-{download_id}"),
                    Arc::clone(&link),
                    emit.clone(),
                    {
                        let download_id = download_id.clone();
                        move |error| Event::DiscardFailed {
                            download_id: download_id.clone(),
                            error: format!("{error:#}"),
                        }
                    },
                    move |client, emit| {
                        // Keep comes first so a reflex press keeps the download.
                        let confirmed = prompts.ask(
                            emit,
                            &format!("Cancel downloading {title}?"),
                            "What has downloaded so far is thrown away.",
                            &["Keep downloading", "Cancel download"],
                        ) == Some(1);
                        if confirmed {
                            emit.send(Event::Discarding {
                                download_id: download_id.clone(),
                            });
                            discard(client, emit, download_id);
                        }
                        Ok(())
                    },
                );
            }
            Ok(Command::Retry { download_id }) => {
                if let Err(error) = client.call(DownloadsRetryParams { download_id }) {
                    log::warn!("retry: {error:#}");
                }
                refresh_downloads(&client, emit);
            }
            Ok(Command::ClearFinished) => {
                if let Err(error) = client.call(DownloadsClearFinishedParams {}) {
                    log::warn!("clearing finished downloads: {error:#}");
                }
                refresh_downloads(&client, emit);
            }
            Ok(Command::Uninstall { cave_id, title }) => {
                let prompts = prompts.clone();
                spawn_op(
                    format!("uninstall-{cave_id}"),
                    Arc::clone(&link),
                    emit.clone(),
                    {
                        let cave_id = cave_id.clone();
                        move |error| Event::UninstallFinished {
                            cave_id: cave_id.clone(),
                            result: Err(format!("{error:#}")),
                        }
                    },
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
                        client.call(UninstallPerformParams {
                            cave_id: cave_id.clone(),
                            hard: None,
                        })?;
                        emit.send(Event::UninstallFinished {
                            cave_id,
                            result: Ok(()),
                        });
                        refresh_caves(client, emit);
                        Ok(())
                    },
                );
            }
            Ok(Command::Launch { cave_id }) => {
                if launches.any() {
                    emit.send(Event::LaunchFinished {
                        cave_id,
                        result: Err(LaunchFailure {
                            message: "another game is still running".into(),
                            log: Vec::new(),
                        }),
                    });
                    continue;
                }
                crate::muos::begin();
                let config = Arc::clone(&config);
                let prompts = prompts.clone();
                let launches = launches.clone();
                let profile_id = profile.id;
                spawn_op(
                    format!("launch-{cave_id}"),
                    Arc::clone(&link),
                    emit.clone(),
                    {
                        let cave_id = cave_id.clone();
                        move |error| Event::LaunchFinished {
                            cave_id: cave_id.clone(),
                            result: Err(LaunchFailure {
                                // The innermost error is butler's own words.
                                message: error.root_cause().to_string(),
                                log: Vec::new(),
                            }),
                        }
                    },
                    move |client, emit| {
                        let (target, name) = match plan_launch(client, &prompts, &cave_id, emit) {
                            Ok(Plan::Launch { target, name }) => (target, name),
                            Ok(Plan::Cancelled) => {
                                emit.send(Event::LaunchFinished {
                                    cave_id,
                                    result: Ok(()),
                                });
                                return Ok(());
                            }
                            Err(failure) => {
                                emit.send(Event::LaunchFinished {
                                    cave_id,
                                    result: Err(failure),
                                });
                                return Ok(());
                            }
                        };
                        let quit = launches.track(&cave_id, client)?;
                        let result = launch(
                            client,
                            &config,
                            &prompts,
                            profile_id,
                            &cave_id,
                            target.as_deref(),
                            &name,
                            emit,
                        );
                        launches.forget(&cave_id);
                        // Play time and last-played change with every run.
                        refresh_caves(client, emit);
                        // Ending the connection is how the user quits; the
                        // call's failure is then the expected outcome.
                        let result = if quit.load(Ordering::Relaxed) {
                            Ok(())
                        } else {
                            result
                        };
                        emit.send(Event::LaunchFinished { cave_id, result });
                        Ok(())
                    },
                );
            }
            Ok(Command::QuitGame { cave_id }) => {
                launches.quit(&cave_id);
                // A payload the firmware runs is our own child, not butler's.
                crate::muos::stop();
            }
            Ok(Command::RefreshLibrary) => {
                sync.stale.store(true, Ordering::Relaxed);
                sync.collections_stale.store(true, Ordering::Relaxed);
                next_update_check = Instant::now() + UPDATE_EVERY;
                emit.status("Refreshing library");
                sync.spawn(&link, emit);
            }
            Ok(Command::CheckUpdates) => {
                next_update_check = Instant::now() + UPDATE_EVERY;
                spawn_op(
                    "update-check".into(),
                    Arc::clone(&link),
                    emit.clone(),
                    |error| Event::Error(format!("Couldn't check for updates: {error:#}")),
                    move |client, emit| check_updates(client, emit).map(|_| ()),
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
                        |error| Event::Error(format!("{error:#}")),
                        move |client, emit| {
                            if let Some(choice) = pick_update(client, &prompts, emit, &update) {
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
            // Finished entries stay listed, as in the itch app, until the
            // user clears them.
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
    /// Shows a question and waits for the answer: a row of choices with
    /// the first as the primary one. `None` means dismissed, or the
    /// interface went away.
    fn ask(&self, emit: &Emitter, title: &str, body: &str, choices: &[&str]) -> Option<usize> {
        self.show(emit, title, body, choices, Some(0), false)
    }

    /// Shows a pick between equals: a column, none drawn as primary.
    fn pick(&self, emit: &Emitter, title: &str, body: &str, choices: &[&str]) -> Option<usize> {
        self.show(emit, title, body, choices, None, true)
    }

    fn show(
        &self,
        emit: &Emitter,
        title: &str,
        body: &str,
        choices: &[&str],
        primary: Option<usize>,
        stacked: bool,
    ) -> Option<usize> {
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
            primary,
            stacked,
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

/// A launch's connection and the flag saying its end was asked for.
type Launching = (Cancel, Arc<AtomicBool>);

/// Launch calls in flight, so a running game can be quit from the loop.
#[derive(Clone, Default)]
struct Launches {
    active: Arc<Mutex<HashMap<String, Launching>>>,
}

impl Launches {
    /// Registers the connection carrying `cave_id`'s launch. Returns the
    /// flag that `quit` sets, so the thread knows the drop was asked for.
    fn track(&self, cave_id: &str, client: &Client) -> Result<Arc<AtomicBool>> {
        let quit = Arc::new(AtomicBool::new(false));
        let cancel = client
            .cancel_handle()
            .context("cloning launch connection")?;
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(cave_id.to_string(), (cancel, Arc::clone(&quit)));
        Ok(quit)
    }

    fn any(&self) -> bool {
        !self
            .active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }

    fn forget(&self, cave_id: &str) {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(cave_id);
    }

    fn quit(&self, cave_id: &str) {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        match active.get(cave_id) {
            Some((cancel, quit)) => {
                log::info!("quitting {cave_id}");
                quit.store(true, Ordering::Relaxed);
                cancel.cancel();
            }
            None => log::warn!("quit: no launch in flight for {cave_id}"),
        }
    }
}

/// How many of the game's last lines to keep for a failed launch.
const LAUNCH_LOG_TAIL: usize = 40;

/// What Play does once butler has said what the install holds.
enum Plan {
    /// Let butler launch it. On muOS a payload the firmware runs is named
    /// by path, and comes back to us as a `RuntimeLaunch` request.
    Launch {
        target: Option<String>,
        name: String,
    },
    /// The user dismissed the pick between several payloads.
    Cancelled,
}

/// On muOS, which of the install's contents to launch: a ROM or a
/// `.love` the firmware runs, picked by the user when there are several,
/// or else a Linux build, which butler launches through the SDL shim. A
/// Linux build that cannot reach the screen fails here, with the reason.
/// Elsewhere butler chooses on its own.
fn plan_launch(
    client: &Client,
    prompts: &Prompts,
    cave_id: &str,
    emit: &Emitter,
) -> Result<Plan, LaunchFailure> {
    if !crate::muos::available() {
        return Ok(Plan::Launch {
            target: None,
            name: String::new(),
        });
    }
    let failure = |message: String| LaunchFailure {
        message,
        log: Vec::new(),
    };
    let cave = client
        .call(FetchCaveParams {
            cave_id: cave_id.to_string(),
            profile_id: None,
        })
        .map_err(|error| failure(error.root_cause().to_string()))?
        .cave
        .ok_or_else(|| failure("the game is no longer installed".into()))?;
    let name = cave
        .game
        .as_ref()
        .map_or("itch.io", |g| g.title.as_str())
        .to_string();
    let targets = client
        .call(LaunchGetTargetsParams {
            cave_id: cave_id.to_string(),
            runtimes: Some(crate::muos::runtimes()),
            deep_probe: Some(true),
        })
        .map_err(|error| failure(error.root_cause().to_string()))?
        .targets;

    // A payload we can run: its path, the name `Launch` matches a target
    // by (the action's path, relative to the install folder), and what it
    // is. A fused LÖVE exe is listed once as a native build and once as
    // the payload inside; the path tells them apart.
    struct Choice {
        path: String,
        target: String,
        content: crate::muos::Content,
    }
    let mut choices: Vec<Choice> = Vec::new();
    let mut unrunnable = Vec::new();
    for target in &targets {
        let Some(strategy) = target.strategy.as_ref() else {
            continue;
        };
        if strategy.strategy != LaunchStrategy::Runtime {
            continue;
        }
        let Some(candidate) = strategy.candidate.as_ref() else {
            continue;
        };
        let path = strategy.full_target_path.clone();
        match crate::muos::content_for(candidate, PathBuf::from(&path)) {
            Ok(content) => {
                if !choices.iter().any(|c| c.path == path) {
                    choices.push(Choice {
                        target: target
                            .action
                            .as_ref()
                            .map_or(candidate.path.clone(), |a| a.path.clone()),
                        path,
                        content,
                    });
                }
            }
            Err(reason) => unrunnable.push(reason),
        }
    }
    if choices.is_empty() {
        let natives: Vec<_> = targets
            .iter()
            .filter(|t| {
                t.strategy
                    .as_ref()
                    .is_some_and(|s| s.strategy == LaunchStrategy::Native)
            })
            .collect();
        if natives.is_empty() {
            return match unrunnable.into_iter().next() {
                Some(reason) => Err(failure(reason)),
                None => Ok(Plan::Launch { target: None, name }),
            };
        }
        // butler picks among the natives; only when none can reach the
        // screen is there nothing for it to do.
        let blocked: Vec<String> = natives
            .iter()
            .filter_map(|t| t.strategy.as_ref()?.candidate.as_ref()?.linux_info.as_ref())
            .filter_map(crate::muos::native_blocker)
            .collect();
        if blocked.len() == natives.len() {
            return Err(failure(blocked.into_iter().next().unwrap_or_default()));
        }
        return Ok(Plan::Launch { target: None, name });
    }

    let index = if choices.len() == 1 {
        0
    } else {
        let names: Vec<String> = choices
            .iter()
            .map(|c| {
                let file = Path::new(&c.path)
                    .file_name()
                    .map_or(c.path.as_str(), |f| f.to_str().unwrap_or(&c.path));
                format!("{file} ({})", c.content.label())
            })
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        match prompts.pick(emit, "What do you want to launch?", "", &names) {
            Some(index) => index,
            None => return Ok(Plan::Cancelled),
        }
    };
    Ok(Plan::Launch {
        target: Some(choices.swap_remove(index).target),
        name,
    })
}

/// Runs a game and stays in the call until it exits, answering whatever
/// butler asks along the way. A failure carries the tail of what butler
/// logged at error level, which is where the game's stderr ends up.
/// `target` names a launch target by path, as `Launch.GetTargets` gave
/// it; `name` is the game's title, for what the firmware shows while a
/// payload of ours runs.
#[allow(clippy::too_many_arguments)]
fn launch(
    client: &Client,
    config: &Config,
    prompts: &Prompts,
    profile_id: i64,
    cave_id: &str,
    target: Option<&str>,
    name: &str,
    emit: &Emitter,
) -> Result<(), LaunchFailure> {
    let mut errors: std::collections::VecDeque<String> = Default::default();
    let launching = LaunchCall {
        client,
        config,
        prompts,
        profile_id,
        cave_id,
        target,
        name,
        emit,
    };
    let result = launch_inner(&launching, |line| {
        if errors.len() == LAUNCH_LOG_TAIL {
            errors.pop_front();
        }
        errors.push_back(line);
    });
    result.map_err(|error| LaunchFailure {
        // Our own reply to a RuntimeLaunch comes back wrapped as a
        // remote error; the words are ours already.
        message: error
            .root_cause()
            .to_string()
            .trim_start_matches("json-rpc2: error 500: ")
            .to_string(),
        log: errors
            .into_iter()
            .filter(|l| !l.starts_with("Relaying launch failure") && !l.starts_with("Had error"))
            .collect(),
    })
}

/// One launch's particulars, shared by the call and its request handlers.
struct LaunchCall<'a> {
    client: &'a Client,
    config: &'a Config,
    prompts: &'a Prompts,
    profile_id: i64,
    cave_id: &'a str,
    target: Option<&'a str>,
    name: &'a str,
    emit: &'a Emitter,
}

fn launch_inner(launching: &LaunchCall<'_>, mut on_error_line: impl FnMut(String)) -> Result<()> {
    let LaunchCall {
        client,
        config,
        prompts,
        profile_id,
        cave_id,
        target,
        name,
        emit,
    } = *launching;
    std::fs::create_dir_all(&config.prereqs_dir)
        .with_context(|| format!("creating {}", config.prereqs_dir.display()))?;
    let muos = crate::muos::available();
    let params = LaunchParams {
        cave_id: cave_id.to_string(),
        prereqs_dir: Some(config.prereqs_dir.to_string_lossy().into_owned()),
        profile_id: Some(profile_id),
        target: target.map(str::to_string),
        // The firmware's payloads come back to us to run; nothing else
        // but a Linux build has a way onto its screen.
        runtimes: muos.then(crate::muos::runtimes),
        allowed_strategies: muos.then(|| vec![LaunchStrategy::Native, LaunchStrategy::Runtime]),
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
                Ok(AnyNotification::Log(log)) => {
                    log::debug!("butler: {}", log.message);
                    if log.level == LogLevel::Error {
                        on_error_line(log.message);
                    }
                }
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
            let outcome = answer_launch_request(client, prompts, name, emit, &id, request);
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
    name: &str,
    emit: &Emitter,
    id: &serde_json::Value,
    request: AnyServerRequest,
) -> Result<()> {
    match request {
        AnyServerRequest::RuntimeLaunch(p) => {
            let candidate = p
                .candidate
                .as_ref()
                .context("runtime launch names no payload")?;
            let content = crate::muos::content_for(candidate, PathBuf::from(&p.full_target_path))
                .map_err(anyhow::Error::msg)?;
            // butler's LaunchRunning came just before this, so the
            // interface is hiding; the emulator's first frame is later
            // than that.
            let args = p.args.as_deref().unwrap_or(&[]);
            let env = p.env.clone().unwrap_or_default();
            match crate::muos::launch(name, &content, args, &env) {
                Ok(()) => client.reply(id, RuntimeLaunchResult {}),
                Err(error) => client.reply_error(id, 500, &format!("{error:#}")),
            }
        }
        AnyServerRequest::PickManifestAction(p) => {
            let names: Vec<&str> = p.actions.iter().map(|a| a.name.as_str()).collect();
            let picked = if names.len() == 1 {
                Some(0)
            } else {
                prompts.pick(emit, "What do you want to launch?", "", &names)
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
/// The background work: collections, a fresh owned list when butler's
/// cache is stale, and the update check. Runs at startup and again
/// whenever the network comes back, and is what decides whether we are
/// online. Everything here runs on one thread, one step after another:
/// butlerd handles requests concurrently and two large writes at once
/// can fail on a stale sqlite snapshot, which is what happened when the
/// owned list and the collections refetched side by side.
#[derive(Clone)]
struct Sync {
    profile_id: i64,
    online: Arc<AtomicBool>,
    /// Whether butler flagged the cached owned list stale, so a fresh
    /// fetch is still owed.
    stale: Arc<AtomicBool>,
    /// The cached collections have been sent once.
    collections_loaded: Arc<AtomicBool>,
    /// Butler flagged the cached collections stale.
    collections_stale: Arc<AtomicBool>,
    /// The last run failed for a reason other than being offline, so the
    /// main loop should run it again at the next probe.
    retry: Arc<AtomicBool>,
}

impl Sync {
    fn spawn(&self, link: &Link, emit: &Emitter) {
        let sync = self.clone();
        self.retry.store(false, Ordering::Relaxed);
        spawn_op(
            "sync".into(),
            Arc::clone(link),
            emit.clone(),
            |error| Event::SyncFailed(format!("{error:#}")),
            move |client, emit| match sync.run(client, emit) {
                Ok(()) => Ok(()),
                Err(error) if is_offline(&error) => {
                    sync.set_online(emit, false);
                    Ok(())
                }
                Err(error) => {
                    sync.retry.store(true, Ordering::Relaxed);
                    Err(error)
                }
            },
        );
    }

    fn run(&self, client: &Client, emit: &Emitter) -> Result<()> {
        if !self.collections_loaded.load(Ordering::Relaxed) {
            // Local reads only; the rows show before the network is probed.
            match collections(client, self.profile_id, false) {
                Ok((cached, stale)) => {
                    emit.send(Event::Collections(cached));
                    self.collections_loaded.store(true, Ordering::Relaxed);
                    self.collections_stale.store(stale, Ordering::Relaxed);
                }
                Err(error) => emit.send(Event::CollectionsFailed(format!("{error:#}"))),
            }
        }
        // A one-item fresh fetch is the cheapest call that must reach the
        // API, so it doubles as the network probe.
        client.call(FetchProfileOwnedKeysParams {
            profile_id: self.profile_id,
            limit: Some(1),
            fresh: Some(true),
            ..Default::default()
        })?;
        self.set_online(emit, true);
        // Before the slow refetches below, so updates show within seconds
        // of the library.
        check_updates(client, emit)?;
        if self.stale.load(Ordering::Relaxed) {
            // The cached list is shown already; the itch app also refetches
            // when butler flags it stale, so new purchases appear.
            let (games, _) = owned_games(client, self.profile_id, true)?;
            emit.send(Event::OwnedGames(games));
            self.stale.store(false, Ordering::Relaxed);
        }
        if self.collections_stale.load(Ordering::Relaxed) {
            let (fresh, _) = collections(client, self.profile_id, true)?;
            emit.send(Event::Collections(fresh));
            self.collections_stale.store(false, Ordering::Relaxed);
        }
        Ok(())
    }

    fn set_online(&self, emit: &Emitter, online: bool) {
        if self.online.swap(online, Ordering::Relaxed) != online {
            log::info!("{}", if online { "online" } else { "offline" });
        }
        emit.send(Event::Online(online));
    }
}

/// Asks itch.io what is newer than the installs and hands the list to
/// the interface. Nothing is queued here: every update waits for the
/// user, direct or not. Indirect updates are butler's guesses that some
/// other upload replaced the installed one, so those are narrowed to
/// uploads that run here and dropped when none does.
fn check_updates(client: &Client, emit: &Emitter) -> Result<Vec<GameUpdate>> {
    let result = client.call(CheckUpdateParams::default())?;
    for warning in &result.warnings {
        log::warn!("update check: {warning}");
    }
    let mut updates = result.updates;
    for update in updates.iter_mut().filter(|u| !u.direct) {
        update
            .choices
            .retain(|c| c.upload.as_ref().is_some_and(upload_runs_here));
    }
    updates.retain(|u| !u.choices.is_empty());
    log::info!("{} updates available", updates.len());
    emit.send(Event::Updates(updates.clone()));
    Ok(updates)
}

fn spawn_op<F, E>(name: String, link: Link, emit: Emitter, fail: E, op: F)
where
    F: FnOnce(&Client, &Emitter) -> Result<()> + Send + 'static,
    E: Fn(anyhow::Error) -> Event + Send + std::marker::Sync + 'static,
{
    let fail = Arc::new(fail);
    let outer_emit = emit.clone();
    let outer_fail = Arc::clone(&fail);
    let outer_name = name.clone();
    let result = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let client = match connect(&link) {
                Ok(client) => client,
                Err(error) => {
                    emit.send(fail(error));
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
                    emit.send(fail(error));
                }
            }
        });
    if let Err(error) = result {
        outer_emit.send(outer_fail(anyhow::anyhow!(
            "spawning {outer_name}: {error}"
        )));
    }
}

fn discard(client: &Client, emit: &Emitter, download_id: String) {
    if let Err(error) = client.call(DownloadsDiscardParams {
        download_id: download_id.clone(),
    }) {
        log::warn!("discard: {error:#}");
        emit.send(Event::DiscardFailed {
            download_id,
            error: format!("{error:#}"),
        });
    }
    refresh_downloads(client, emit);
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
/// Queues an install, asking which upload when the game has more than one
/// for this device. `Ok(false)` when the user backed out.
fn queue_install(
    client: &Client,
    config: &Config,
    prompts: &Prompts,
    emit: &Emitter,
    game: Game,
) -> Result<bool> {
    let location = install_location(client, config)?;
    // butler's compatibility filter goes by platform tags, which a ROM
    // does not carry; on muOS every upload is fetched and judged by name.
    let muos = crate::muos::available();
    let mut uploads = client
        .call(FetchGameUploadsParams {
            game_id: game.id,
            compatible: !muos,
            fresh: Some(true),
        })?
        .uploads;
    if muos {
        uploads.retain(upload_runs_here);
    }
    if uploads.is_empty() {
        bail!("{} has no download for this device", game.title);
    }
    let index = if uploads.len() == 1 {
        0
    } else {
        match pick_upload(prompts, emit, &game, &uploads) {
            Some(index) => index,
            None => return Ok(false),
        }
    };
    let upload = uploads.swap_remove(index);
    // butler knows a few ROM extensions and sniffs the rest as "unknown",
    // for which it has no installer. A ROM is a file to copy, so on muOS
    // an upload that is not an archive is installed as one.
    let ignore_installers = muos && crate::muos::runs_here(std::path::Path::new(&upload.filename));
    let queued = client.call(InstallQueueParams {
        game: Some(game.clone()),
        upload: Some(upload),
        install_location_id: Some(location),
        reason: Some(DownloadReason::Install),
        queue_download: Some(true),
        ignore_installers: ignore_installers.then_some(true),
        ..Default::default()
    })?;
    log::info!("queued {} as download {}", game.title, queued.id);
    Ok(true)
}

/// One line per upload for the picker: its name, size, and what marks it
/// out when the name alone does not.
fn upload_label(upload: &Upload) -> String {
    let mut label = upload.name().to_string();
    let mut notes = Vec::new();
    if upload.size > 0 {
        notes.push(human_size(upload.size));
    }
    if upload.demo {
        notes.push("demo".to_string());
    }
    let kind = match upload.r#type {
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
    };
    notes.extend(kind.map(str::to_string));
    if !notes.is_empty() {
        label.push_str(&format!(" ({})", notes.join(", ")));
    }
    label
}

/// Asks which upload to install. `None` when the user backs out.
fn pick_upload(
    prompts: &Prompts,
    emit: &Emitter,
    game: &Game,
    uploads: &[Upload],
) -> Option<usize> {
    let labels: Vec<String> = uploads.iter().map(upload_label).collect();
    let mut choices: Vec<&str> = labels.iter().map(String::as_str).collect();
    choices.push("Cancel");
    let body = format!("{} has more than one download for this device.", game.title);
    let picked = prompts.pick(emit, "Which download?", &body, &choices)?;
    (picked < labels.len()).then_some(picked)
}

/// Asks which of an indirect update's uploads to install. `None` when the
/// user backs out.
/// Asks which of an indirect update's uploads to install, showing what is
/// installed now beside what is offered.
fn pick_update(
    client: &Client,
    prompts: &Prompts,
    emit: &Emitter,
    update: &GameUpdate,
) -> Option<usize> {
    let title = update.game.as_ref().map_or("game", |g| g.title.as_str());
    let installed = client
        .call(FetchCaveParams {
            cave_id: update.cave_id.clone(),
            profile_id: None,
        })
        .ok()
        .and_then(|r| r.cave)
        .and_then(|cave| cave.upload)
        .map(|upload| describe_upload(&upload));
    let names: Vec<String> = update
        .choices
        .iter()
        .map(|c| {
            c.upload
                .as_ref()
                .map_or("upload".to_string(), describe_upload)
        })
        .collect();
    // One offer needs no naming on its button; the body already names it.
    let mut choices: Vec<&str> = if names.len() == 1 {
        vec!["Install"]
    } else {
        names.iter().map(String::as_str).collect()
    };
    choices.push("Cancel");
    let mut body = format!(
        "A newer upload of {title} appeared after it was installed. It may be a new \
         version or something else, like extra content. Installing it replaces the \
         current install.\n"
    );
    if let Some(installed) = installed {
        body.push_str(&format!("\nInstalled: {installed}"));
    }
    body.push_str(&format!("\nOffered: {}", names.join(", ")));
    // One offer is a yes-or-no question; several are a pick between them.
    let picked = if names.len() == 1 {
        prompts.ask(emit, "Update?", &body, &choices)?
    } else {
        prompts.pick(emit, "Update?", &body, &choices)?
    };
    (picked < names.len()).then_some(picked)
}

/// "name (Linux)" for telling uploads apart in a prompt.
fn describe_upload(upload: &Upload) -> String {
    let platforms = upload_platform_names(upload);
    if platforms.is_empty() {
        upload.name().to_string()
    } else {
        format!("{} ({})", upload.name(), platforms.join(", "))
    }
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

const COLLECTION_PAGE: i64 = 100;

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
        // One page each; rows fetch the rest as they are scrolled. With
        // `fresh`, butler pulls the whole collection into its database on
        // this call, so later pages are local.
        let page = client.call(FetchCollectionGamesParams {
            profile_id,
            collection_id: collection.id,
            limit: Some(COLLECTION_PAGE),
            fresh: Some(fresh),
            ..Default::default()
        })?;
        stale |= page.stale == Some(true);
        shelves.push(CollectionGames {
            collection,
            games: page
                .items
                .into_iter()
                .filter_map(|item| item.game)
                .collect(),
            next_cursor: page.next_cursor.filter(|c| !c.is_empty()),
        });
    }
    Ok((shelves, stale && !fresh))
}

/// One page of a collection's games from butler's database, and the cursor
/// for the page after it.
fn collection_page(
    client: &Client,
    profile_id: i64,
    collection_id: i64,
    cursor: Option<String>,
    filters: Option<CollectionGamesFilters>,
) -> Result<(Vec<Game>, Option<String>)> {
    let page = client.call(FetchCollectionGamesParams {
        profile_id,
        collection_id,
        limit: Some(COLLECTION_PAGE),
        cursor,
        filters,
        fresh: Some(false),
        ..Default::default()
    })?;
    Ok((
        page.items
            .into_iter()
            .filter_map(|item| item.game)
            .collect(),
        page.next_cursor.filter(|c| !c.is_empty()),
    ))
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
