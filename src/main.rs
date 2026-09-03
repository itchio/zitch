mod app;
mod backend;
mod butlerd;
mod gamepad;
mod glyphs;
mod images;
mod model;
mod ui;

use std::path::{Path, PathBuf};

use clap::Parser;

/// A controller-friendly big picture itch.io client.
#[derive(Debug, Parser)]
#[command(name = "zitch", version, about)]
struct Cli {
    /// Path to the butler binary. Defaults to the one the itch app installed
    /// under the config directory (see --app-name), or `butler` on PATH when
    /// BROTH_USE_LOCAL includes `butler`, as with the itch app.
    #[arg(long, env = "ZITCH_BUTLER")]
    butler: Option<PathBuf>,

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

    /// Window size in points, e.g. `640x480` to preview a handheld screen.
    /// On a HiDPI display the window covers more pixels than this.
    #[arg(long, value_name = "WxH", value_parser = parse_size, default_value = "1280x720")]
    window: (f32, f32),

    /// Extra interface magnification on top of the layout, which already
    /// scales with the screen. Ignored with --emulate.
    #[arg(long, default_value_t = 1.0)]
    zoom: f32,

    /// Lay the interface out for a display of this many pixels, e.g.
    /// `640x480`, and scale it to fit the window with black bars. Resizing
    /// the window changes the magnification, not the layout.
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    emulate: Option<(f32, f32)>,

    /// Use the handheld cover policy (small thumbnails, tight memory and
    /// disk budgets, no animated covers) regardless of screen size, or
    /// `--low-spec=false` to never use it.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    low_spec: Option<bool>,

    /// Log JSON-RPC traffic.
    #[arg(short, long)]
    verbose: bool,
}

fn parse_size(text: &str) -> Result<(f32, f32), String> {
    let (w, h) = text
        .split_once('x')
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got {text:?}"))?;
    let parse = |s: &str| s.trim().parse::<f32>().map_err(|e| format!("{s:?}: {e}"));
    Ok((parse(w)?, parse(h)?))
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
    let covers = images::CoverLoader::new(
        base_dirs.cache_dir().join("zitch").join("covers"),
        images::Policy::for_screen(cli.emulate.unwrap_or(cli.window).1, cli.low_spec),
    );
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
    let butler = match cli
        .butler
        .map(Ok)
        .unwrap_or_else(|| find_butler(&config_dir))
    {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    log::info!("using butler at {}", butler.display());
    let config = backend::Config {
        butler,
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
            .with_inner_size([cli.window.0, cli.window.1]),
        ..Default::default()
    };
    eframe::run_native(
        "zitch",
        options,
        Box::new(move |cc| {
            waker.attach(&cc.egui_ctx);
            Ok(Box::new(app::App::new(
                backend,
                covers,
                &cc.egui_ctx,
                cli.zoom,
                cli.emulate,
                cli.low_spec,
                shot,
            )))
        }),
    )
}

/// Finds butler the way the itch app's broth does: the version named by
/// `broth/butler/.chosen-version` in the config directory, unless
/// `BROTH_USE_LOCAL` lists `butler`, in which case the one on PATH.
fn find_butler(config_dir: &Path) -> Result<PathBuf, String> {
    let use_local = std::env::var("BROTH_USE_LOCAL")
        .map(|list| list.split(',').any(|name| name.trim() == "butler"))
        .unwrap_or(false);
    if use_local {
        log::info!("BROTH_USE_LOCAL includes butler; using butler on PATH");
        return Ok(PathBuf::from("butler"));
    }
    let broth = config_dir.join("broth").join("butler");
    let marker = broth.join(".chosen-version");
    let version = std::fs::read_to_string(&marker).map_err(|error| {
        format!(
            "no butler installed under {}: reading {}: {error}\n\
             Point --app-name at an itch app install (itch or kitch), pass \
             --butler, or set BROTH_USE_LOCAL=butler to use the one on PATH.",
            broth.display(),
            marker.display()
        )
    })?;
    let exe = if cfg!(windows) {
        "butler.exe"
    } else {
        "butler"
    };
    let path = broth.join("versions").join(version.trim()).join(exe);
    if !path.is_file() {
        return Err(format!(
            "{} names version {} but {} does not exist",
            marker.display(),
            version.trim(),
            path.display()
        ));
    }
    Ok(path)
}
