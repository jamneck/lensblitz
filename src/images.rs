//! Image cache. Images are decoded (and fetched, for remote ones) on a small pool of
//! worker threads, downscaled to what the screen can show, and uploaded as textures.
//! Nothing here runs before the first frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::log_err;

/// Widest texture we keep, in pixels: the reading column at 2x with headroom for zoom.
const MAX_TEX_W: u32 = 2400;
const MAX_TEX_H: u32 = 8192;
const MAX_FILE_BYTES: u64 = 64 << 20;
const WORKERS: usize = 3;

enum State {
    Idle,
    Loading,
    Ready(TextureHandle, bool),
    Failed,
}

struct Entry {
    /// Natural size in points (pixels for bitmaps, user units for SVG).
    natural: Option<[f32; 2]>,
    /// Whether we already tried to read the size from the file header.
    probed: bool,
    mtime: Option<SystemTime>,
    state: State,
}

pub struct Images {
    entries: Mutex<HashMap<String, Entry>>,
    jobs: Mutex<Option<Sender<String>>>,
    ctx: egui::Context,
}

pub enum Status {
    /// The texture, and whether it is fully opaque (a photo or screenshot).
    Ready(TextureHandle, bool),
    Loading,
    Failed,
}

fn is_remote(key: &str) -> bool {
    key.starts_with("http://") || key.starts_with("https://")
}

fn is_svg(key: &str, bytes: &[u8]) -> bool {
    let path = key.split(['?', '#']).next().unwrap_or(key);
    if path.to_ascii_lowercase().ends_with(".svg") {
        return true;
    }
    let head = &bytes[..bytes.len().min(512)];
    let head = String::from_utf8_lossy(head);
    let head = head.trim_start();
    head.starts_with("<svg")
        || (head.starts_with("<?xml") && head.contains("<svg"))
        || head.starts_with("<!DOCTYPE svg")
}

impl Images {
    pub fn new(ctx: egui::Context) -> Self {
        Self { entries: Mutex::default(), jobs: Mutex::new(None), ctx }
    }

    /// The cache key for an image source: an absolute path or a URL.
    pub fn key(base_dir: Option<&Path>, src: &str) -> Option<String> {
        let src = src.trim();
        if src.is_empty() || src.starts_with("data:") {
            return None;
        }
        if is_remote(src) {
            return Some(src.to_owned());
        }
        if src.starts_with("//") {
            return Some(format!("https:{src}"));
        }
        let raw = src.strip_prefix("file://").unwrap_or(src);
        let raw = raw.split(['?', '#']).next().unwrap_or(raw);
        let decoded = percent_decode(raw);
        let path = Path::new(&decoded);
        let path: PathBuf = if path.is_absolute() {
            path.to_owned()
        } else if let Some(rest) = decoded.strip_prefix("~/") {
            PathBuf::from(std::env::var_os("HOME")?).join(rest)
        } else {
            base_dir?.join(path)
        };
        Some(path.to_string_lossy().into_owned())
    }

    /// Natural size, if known. For local bitmaps this reads just the file header the first
    /// time, so text can be laid out around the image before it has been decoded.
    pub fn natural_size(&self, key: &str) -> Option<[f32; 2]> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mtime = if is_remote(key) { None } else { std::fs::metadata(key).and_then(|m| m.modified()).ok() };
        let entry =
            entries.entry(key.to_owned()).or_insert(Entry { natural: None, probed: false, mtime, state: State::Idle });
        if entry.mtime != mtime {
            // The file changed on disk: forget what we knew.
            *entry = Entry { natural: None, probed: false, mtime, state: State::Idle };
        }
        if entry.natural.is_none() && !entry.probed && !is_remote(key) {
            entry.probed = true;
            entry.natural = image::ImageReader::open(key)
                .ok()
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.into_dimensions().ok())
                .map(|(w, h)| [w as f32, h as f32]);
        }
        entry.natural
    }

    /// The texture for `key`, starting a background load the first time it is asked for.
    pub fn get(self: &Arc<Self>, key: &str) -> Status {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(key.to_owned()).or_insert(Entry {
            natural: None,
            probed: false,
            mtime: None,
            state: State::Idle,
        });
        match &entry.state {
            State::Ready(tex, opaque) => Status::Ready(tex.clone(), *opaque),
            State::Loading => Status::Loading,
            State::Failed => Status::Failed,
            State::Idle => {
                entry.state = State::Loading;
                drop(entries);
                self.enqueue(key.to_owned());
                Status::Loading
            }
        }
    }

    fn enqueue(self: &Arc<Self>, key: String) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let tx = jobs.get_or_insert_with(|| {
            let (tx, rx) = channel::<String>();
            let rx = Arc::new(Mutex::new(rx));
            for n in 0..WORKERS {
                let rx = rx.clone();
                let images = Arc::downgrade(self);
                let spawned = std::thread::Builder::new().name(format!("lensblitz-image-{n}")).spawn(move || {
                    worker(&rx, &images);
                });
                if let Err(err) = spawned {
                    log_err!("could not start image worker: {err}");
                }
            }
            tx
        });
        if tx.send(key).is_err() {
            log_err!("image workers are gone; images will not load");
        }
    }

    fn finish(&self, key: &str, result: Result<(ColorImage, [f32; 2]), String>) {
        let state = match result {
            Ok((image, natural)) => {
                let opaque = image.pixels.iter().all(|p| p.a() == 255);
                let options = TextureOptions {
                    magnification: egui::TextureFilter::Linear,
                    minification: egui::TextureFilter::Linear,
                    wrap_mode: egui::TextureWrapMode::ClampToEdge,
                    mipmap_mode: Some(egui::TextureFilter::Linear),
                };
                let tex = self.ctx.load_texture(key, image, options);
                Ok((tex, natural, opaque))
            }
            Err(err) => Err(err),
        };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.get_mut(key) {
            match state {
                Ok((tex, natural, opaque)) => {
                    entry.natural = Some(natural);
                    entry.state = State::Ready(tex, opaque);
                }
                Err(err) => {
                    log_err!("image {key}: {err}");
                    entry.state = State::Failed;
                }
            }
        }
        drop(entries);
        // Wake every window: any of them may be showing this image.
        self.ctx.request_repaint();
        for id in self.ctx.input(|i| i.raw.viewports.keys().copied().collect::<Vec<_>>()) {
            self.ctx.request_repaint_of(id);
        }
    }
}

fn worker(rx: &Mutex<Receiver<String>>, images: &std::sync::Weak<Images>) {
    loop {
        let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
        let Ok(key) = job else {
            return;
        };
        let Some(images) = images.upgrade() else {
            return;
        };
        let result = load(&key);
        images.finish(&key, result);
    }
}

fn load(key: &str) -> Result<(ColorImage, [f32; 2]), String> {
    let bytes = if is_remote(key) { fetch(key)? } else { read_local(key)? };
    if is_svg(key, &bytes) { decode_svg(&bytes) } else { decode_bitmap(&bytes) }
}

fn read_local(path: &str) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("file is too large ({} MB)", meta.len() >> 20));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(concat!("LensBlitz/", env!("CARGO_PKG_VERSION")))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into()
    });
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    response.body_mut().with_config().limit(MAX_FILE_BYTES).read_to_vec().map_err(|e| e.to_string())
}

fn decode_bitmap(bytes: &[u8]) -> Result<(ColorImage, [f32; 2]), String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("image has no pixels".to_owned());
    }
    let scale = (MAX_TEX_W as f32 / w as f32).min(MAX_TEX_H as f32 / h as f32).min(1.0);
    let img = if scale < 1.0 {
        let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
        img.resize_exact(nw, nh, image::imageops::FilterType::CatmullRom)
    } else {
        img
    };
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok((ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), [w as f32, h as f32]))
}

fn svg_options() -> &'static resvg::usvg::Options<'static> {
    static OPTIONS: OnceLock<resvg::usvg::Options<'static>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut opt = resvg::usvg::Options::default();
        let db = opt.fontdb_mut();
        // Badges and diagrams ask for these by name; scanning every system font is slow.
        for file in [
            "/System/Library/Fonts/Supplemental/Verdana.ttf",
            "/System/Library/Fonts/Supplemental/Verdana Bold.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/System/Library/Fonts/Supplemental/Georgia.ttf",
            "/System/Library/Fonts/Supplemental/Courier New.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/Menlo.ttc",
        ] {
            let _ = db.load_font_file(file);
        }
        db.load_font_data(crate::theme::INTER.to_vec());
        db.load_font_data(crate::theme::MONO.to_vec());
        db.set_sans_serif_family("Helvetica");
        db.set_serif_family("Georgia");
        db.set_monospace_family("Menlo");
        opt.font_family = "Helvetica".to_owned();
        opt
    })
}

fn decode_svg(bytes: &[u8]) -> Result<(ColorImage, [f32; 2]), String> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, svg_options()).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    // Render at 2x for Retina, more for tiny icons, less for huge drawings.
    let scale = (MAX_TEX_W as f32 / w).min(MAX_TEX_H as f32 / h).min(if w < 400.0 { 3.0 } else { 2.0 });
    let (pw, ph) = (((w * scale).ceil() as u32).max(1), ((h * scale).ceil() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or("SVG is too large to render")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let image = ColorImage::from_rgba_premultiplied([pw as usize, ph as usize], pixmap.data());
    Ok((image, [w, h]))
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_resolve_relative_and_remote() {
        let base = Path::new("/docs/guide");
        assert_eq!(Images::key(Some(base), "img/a%20b.png").as_deref(), Some("/docs/guide/img/a b.png"));
        assert_eq!(Images::key(Some(base), "/abs/x.png?raw=1").as_deref(), Some("/abs/x.png"));
        assert_eq!(Images::key(Some(base), "https://x.dev/a.svg").as_deref(), Some("https://x.dev/a.svg"));
        assert_eq!(Images::key(None, "rel.png"), None);
        assert_eq!(Images::key(Some(base), "data:image/png;base64,AAAA"), None);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b%2Fc%"), "a b/c%");
        assert_eq!(percent_decode("100%25"), "100%");
    }

    #[test]
    fn sniffs_svg() {
        assert!(is_svg("https://img.shields.io/badge/a-b-blue", b"<svg xmlns=\"http://www.w3.org/2000/svg\">"));
        assert!(is_svg("logo.SVG", b""));
        assert!(!is_svg("photo.png", b"\x89PNG\r\n"));
    }
}
