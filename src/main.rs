mod app;
mod backend;
mod butlerd;
mod gamepad;
mod glyphs;
mod images;
mod model;
mod ui;

use std::path::PathBuf;

use clap::Parser;

/// A controller-friendly big picture itch.io client.
#[derive(Debug, Parser)]
#[command(name = "zitch", version, about)]
struct Cli {
    /// Path to the butler binary. Defaults to `butler` on PATH.
    #[arg(long, env = "ZITCH_BUTLER", default_value = "butler")]
    butler: PathBuf,

    /// Which config directory to use: `~/.config/<name>`, laid out like the
    /// itch app's. `itch` or `kitch` reuses that app's butler database and
    /// saved login. Do not run both against one database at the same time.
    #[arg(long, env = "ZITCH_APP_NAME", default_value = "zitch")]
    app_name: String,

    /// Butler database path. Overrides the one derived from --app-name.
    #[arg(long)]
    dbpath: Option<PathBuf>,

    /// Which saved login to use, by butlerd profile id. Defaults to the
    /// most recently used one; an unknown id lists the choices.
    #[arg(long, env = "ZITCH_PROFILE_ID")]
    profile_id: Option<i64>,

    /// File containing an itch.io API key, used to sign in when the database
    /// has no saved profile. Never logged.
    #[arg(long, env = "ZITCH_API_KEY_FILE")]
    api_key_file: Option<PathBuf>,

    /// Write a PNG of the window to this path once the library has loaded
    /// (or after a few seconds) and exit. For development.
    #[arg(long, value_name = "PATH")]
    screenshot: Option<PathBuf>,

    /// Input to play before the screenshot, e.g. `down,down,right,enter`.
    #[arg(long, value_name = "STEPS", requires = "screenshot")]
    screenshot_script: Option<String>,

    /// Log JSON-RPC traffic.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(if cli.verbose {
            "zitch=trace"
        } else {
            "zitch=info"
        }),
    )
    .init();

    let base_dirs = directories::BaseDirs::new().expect("a home directory");
    let config_dir = base_dirs.config_dir().join(&cli.app_name);
    // Covers are the same whichever app's database is in use.
    let covers = images::CoverLoader::new(base_dirs.cache_dir().join("zitch").join("covers"));
    let dbpath = cli
        .dbpath
        .unwrap_or_else(|| config_dir.join("db").join("butler.db"));
    log::info!("using {}", dbpath.display());
    let api_key = std::env::var("ZITCH_API_KEY").ok().or_else(|| {
        let path = cli.api_key_file.as_ref()?;
        match std::fs::read_to_string(path) {
            Ok(key) => Some(key.trim().to_string()),
            Err(error) => {
                log::error!("reading {}: {error}", path.display());
                None
            }
        }
    });
    let config = backend::Config {
        butler: cli.butler,
        dbpath,
        api_key,
        profile_id: cli.profile_id,
        // Same layout as the itch app, so a shared config dir shares games.
        install_dir: config_dir.join("apps"),
        prereqs_dir: config_dir.join("prereqs"),
    };

    let script = match cli.screenshot_script.as_deref().map(app::parse_script) {
        Some(Ok(script)) => script,
        Some(Err(error)) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
        None => Vec::new(),
    };
    let shot = cli
        .screenshot
        .map(|path| app::Shot::new(path, std::time::Duration::from_secs(8), script));
    let waker = backend::Waker::default();
    let backend = backend::Backend::spawn(config, waker.clone());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("zitch")
            .with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "zitch",
        options,
        Box::new(move |cc| {
            waker.attach(&cc.egui_ctx);
            Ok(Box::new(app::App::new(backend, covers, &cc.egui_ctx, shot)))
        }),
    )
}
