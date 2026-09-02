mod app;
mod backend;
mod butlerd;
mod model;

use std::path::PathBuf;

use clap::Parser;

/// A controller-friendly big picture itch.io client.
#[derive(Debug, Parser)]
#[command(name = "zitch", version, about)]
struct Cli {
    /// Path to the butler binary. Defaults to `butler` on PATH.
    #[arg(long, env = "ZITCH_BUTLER", default_value = "butler")]
    butler: PathBuf,

    /// Where butler keeps its database. Separate from the itch app's.
    #[arg(long)]
    dbpath: Option<PathBuf>,

    /// File containing an itch.io API key, used to sign in when the database
    /// has no saved profile. Never logged.
    #[arg(long, env = "ZITCH_API_KEY_FILE")]
    api_key_file: Option<PathBuf>,

    /// Write a PNG of the window to this path once the library has loaded
    /// (or after a few seconds) and exit. For development.
    #[arg(long, value_name = "PATH")]
    screenshot: Option<PathBuf>,

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

    let dirs = directories::ProjectDirs::from("", "", "zitch").expect("a home directory");
    let dbpath = cli
        .dbpath
        .unwrap_or_else(|| dirs.data_dir().join("butler.db"));
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
    };

    let shot = cli
        .screenshot
        .map(|path| app::Shot::new(path, std::time::Duration::from_secs(8)));
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
            Ok(Box::new(app::App::new(backend, &cc.egui_ctx, shot)))
        }),
    )
}
