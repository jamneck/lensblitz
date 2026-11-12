//! LensBlitz — an extremely fast, read-only Markdown viewer for macOS.

mod app;
mod doc;
mod highlight;
mod html;
mod images;
mod layout;
mod macos;
mod preload;
mod theme;
mod view;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use winit::event_loop::EventLoop;

/// File extensions treated as Markdown (for the Open panel and for links between documents).
pub const MARKDOWN_EXTENSIONS: &[&str] =
    &["md", "markdown", "mdown", "mkd", "mkdn", "mdwn", "mdx", "mdtxt", "text", "txt"];

/// Print a startup timing line when `LENSBLITZ_TRACE` is set.
pub fn trace(start: Instant, what: &str) {
    if std::env::var_os("LENSBLITZ_TRACE").is_some() {
        eprintln!("lensblitz: {:>7.1} ms  {what}", start.elapsed().as_secs_f64() * 1e3);
    }
}

/// Log a recoverable error to stderr.
#[macro_export]
macro_rules! log_err {
    ($($arg:tt)*) => {
        eprintln!("lensblitz: error: {}", format_args!($($arg)*))
    };
}

const HELP: &str = "\
LensBlitz — an extremely fast, read-only Markdown viewer

USAGE:
    lensblitz [FILE]...

Each FILE opens in its own window. With no FILE, an empty window opens;
drop a Markdown file on it or press ⌘O.

OPTIONS:
    -h, --help       Show this help
    -V, --version    Show the version

KEYS:
    ⌘O open     ⌘W close     ⌘R reload     ⌘F find     ⌘G / ⇧⌘G next / previous
    ⌘+ ⌘- ⌘0 zoom     ⌘[ ⌘] back / forward     ⌘T contents     ⌘C copy
    Space / ⇧Space page     ↑ ↓ j k scroll     ⌘↑ ⌘↓ top / bottom
";

fn main() -> ExitCode {
    let start = Instant::now();

    let mut files = Vec::new();
    let mut literal = false;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--") if !literal => literal = true,
            Some("-h" | "--help") if !literal => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            Some("-V" | "--version") if !literal => {
                println!("lensblitz {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            // Finder used to pass a process serial number to launched apps.
            Some(s) if s.starts_with("-psn_") && !literal => {}
            Some(s) if s.starts_with('-') && s.len() > 1 && !literal => {
                eprintln!("lensblitz: unknown option '{s}'\n\n{HELP}");
                return ExitCode::from(2);
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }

    // Never die silently: say what happened and where before aborting.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("lensblitz: fatal: {info}");
        eprintln!("lensblitz: this is a bug — please report it with the message above.");
    }));

    // Read and parse the documents, and enumerate system fonts, while the window is created.
    for file in &files {
        preload::start(file);
    }
    let fonts = app::SystemFonts::start();

    trace(start, "main: args parsed, font thread started");
    let event_loop = match EventLoop::<eframe::UserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            log_err!("could not start the event loop: {err}");
            return ExitCode::FAILURE;
        }
    };
    // Must happen between creating the event loop and running it (see `macos`).
    macos::install_open_handler();
    trace(start, "event loop ready");

    let title = app::title_for(files.first().map(PathBuf::as_path));
    let mut viewport = app::viewport(&title);
    // Development aid: `LENSBLITZ_SIZE=1280x900` fixes the window size (for screenshots).
    if let Some((w, h)) = std::env::var("LENSBLITZ_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
    }) {
        viewport = viewport.with_inner_size([w, h]);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: true,
        persist_window: false,
        // We install our own, non-blocking system font provider.
        system_font_fallback: false,
        ..Default::default()
    };
    let mut eframe_app = eframe::create_native(
        "LensBlitz",
        options,
        Box::new(move |cc: &eframe::CreationContext<'_>| -> Result<Box<dyn eframe::App>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Box::new(app::App::new(cc, files, fonts, start)))
        }),
        &event_loop,
    );
    match event_loop.run_app(&mut eframe_app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log_err!("event loop failed: {err}");
            ExitCode::FAILURE
        }
    }
}
