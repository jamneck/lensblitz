//! The application: owns the windows and routes "open this file" requests to them.
//!
//! The first document lives in eframe's root window; every further document gets its own
//! native window (an egui *deferred viewport*, which repaints independently).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use eframe::egui::epaint::text::{FallbackRequest, FontProvider};
use eframe::egui::{self, FontFamily, ViewportBuilder, ViewportCommand, ViewportId};
use egui_system_font_provider::SystemFontProvider;

use crate::macos;
use crate::theme::{self, Theme};
use crate::view::{Shared, Win};

pub const WINDOW_SIZE: [f32; 2] = [920.0, 1040.0];
pub const MIN_WINDOW_SIZE: [f32; 2] = [440.0, 320.0];

/// Window settings shared by the root window and the extra document windows.
pub fn viewport(title: &str) -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title(title)
        .with_app_id("com.lensblitz.app")
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size(MIN_WINDOW_SIZE)
        // Content runs under a transparent title bar: no chrome, just the page.
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(true)
        .with_drag_and_drop(true)
        // Use the bundle's icon rather than eframe's default.
        .with_icon(egui::IconData::default())
}

pub fn title_for(path: Option<&Path>) -> String {
    match path.and_then(Path::file_name) {
        Some(name) => format!("LensBlitz — {}", name.to_string_lossy()),
        None => "LensBlitz".to_owned(),
    }
}

/// System fonts (CJK, colour emoji, symbols …) for characters Inter and JetBrains Mono lack.
///
/// Enumerating installed fonts takes a while, so it happens on a background thread that
/// starts before the window exists; the provider is plugged in once it is ready, so no
/// frame ever waits for it.
pub struct SystemFonts {
    ready: Arc<Mutex<Option<Arc<SystemFontProvider>>>>,
}

static WAKE: OnceLock<egui::Context> = OnceLock::new();

impl SystemFonts {
    pub fn start() -> Self {
        let ready: Arc<Mutex<Option<Arc<SystemFontProvider>>>> = Arc::default();
        let slot = ready.clone();
        let spawned = std::thread::Builder::new().name("lensblitz-fonts".to_owned()).spawn(move || {
            let provider = SystemFontProvider::new();
            // The first lookup blocks until enumeration is done; do it here, not on the UI thread.
            let _ = provider.font_for(&FallbackRequest { cluster: "あ", family: &FontFamily::Proportional });
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(provider));
            if let Some(ctx) = WAKE.get() {
                ctx.request_repaint_of(ViewportId::ROOT);
            }
        });
        if let Err(err) = spawned {
            crate::log_err!("could not start the system font thread: {err}");
        }
        Self { ready }
    }

    fn take(&self) -> Option<Arc<SystemFontProvider>> {
        self.ready.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

struct Child {
    id: ViewportId,
    win: Arc<Mutex<Win>>,
    position: Option<egui::Pos2>,
}

pub struct App {
    shared: Arc<Shared>,
    fonts: SystemFonts,
    root: Win,
    /// The root window was closed while other windows were open. It stays alive, hidden,
    /// because eframe's root owns the event loop.
    root_hidden: bool,
    children: Vec<Child>,
    next_id: u64,
    frames: u64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>, fonts: SystemFonts, start: Instant) -> Self {
        crate::trace(start, "window and GL context created");
        let ctx = &cc.egui_ctx;
        ctx.set_fonts(theme::font_definitions());
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        theme::apply_chrome(ctx, Theme::get(false));
        theme::apply_chrome(ctx, Theme::get(true));
        macos::set_context(ctx);
        let _ = WAKE.set(ctx.clone());

        let shared = Arc::new(Shared::new(ctx, start));
        let mut app = Self {
            shared,
            fonts,
            root: Win::new(0, ViewportId::ROOT),
            root_hidden: false,
            children: Vec::new(),
            next_id: 1,
            frames: 0,
        };
        app.install_system_fonts(ctx);
        // Files from the command line, plus any Finder sent while we were launching.
        for path in files.into_iter().chain(macos::take_opened()) {
            app.open(ctx, &path, None);
        }
        crate::trace(start, "document parsed, app ready");
        app
    }

    fn install_system_fonts(&mut self, ctx: &egui::Context) {
        if let Some(provider) = self.fonts.take() {
            crate::trace(self.shared.start, "system fonts installed");
            ctx.add_font_provider(provider);
            // Every galley we hold was shaped without these fonts.
            self.shared.fonts_epoch.fetch_add(1, Ordering::AcqRel);
            for child in &self.children {
                ctx.request_repaint_of(child.id);
            }
            ctx.request_repaint();
        }
    }

    /// Open `path`: focus the window already showing it, reuse an empty window, or make a new one.
    fn open(&mut self, ctx: &egui::Context, path: &Path, anchor: Option<String>) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let trace = |how: &str| crate::trace(self.shared.start, &format!("open {} -> {how}", path.display()));
        if !self.root_hidden && self.root.path.as_deref() == Some(&path) {
            trace("already open, focusing its window");
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
            return;
        }
        for child in &self.children {
            if child.win.lock().unwrap_or_else(|e| e.into_inner()).path.as_deref() == Some(&path) {
                trace("already open, focusing its window");
                ctx.send_viewport_cmd_to(child.id, ViewportCommand::Focus);
                return;
            }
        }
        if self.root_hidden || self.root.is_empty() {
            trace("first window");
            self.root.open(&path, anchor, &self.shared);
            if self.root_hidden {
                self.root_hidden = false;
                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Visible(true));
            }
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
            return;
        }
        trace("new window");
        let n = self.next_id;
        self.next_id += 1;
        let id = ViewportId::from_hash_of(("lensblitz-doc", n));
        let mut win = Win::new(n, id);
        win.open(&path, anchor, &self.shared);
        // Cascade from the root window so new windows do not stack exactly on top.
        let step = 28.0 * (1 + self.children.len() % 8) as f32;
        let position = ctx
            .input(|i| i.raw.viewports.get(&ViewportId::ROOT).and_then(|v| v.outer_rect))
            .map(|r| r.min + egui::vec2(step, step));
        self.children.push(Child { id, win: Arc::new(Mutex::new(win)), position });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.install_system_fonts(&ctx);
        self.frames += 1;
        if self.frames == 2 {
            // Cosmetic, so it waits until the first frame is on screen.
            macos::set_dock_icon_if_unbundled();
        }

        for path in macos::take_opened() {
            self.open(&ctx, &path, None);
        }
        let requests = std::mem::take(&mut *self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()));
        for request in requests {
            self.open(&ctx, &request.path, request.anchor);
        }

        self.children.retain(|c| !c.win.lock().unwrap_or_else(|e| e.into_inner()).closed);

        // Closing the root window with other documents open only hides it.
        if ctx.input(|i| i.viewport().close_requested()) && !self.children.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.root_hidden = true;
            self.root.unload(&self.shared);
        }
        if self.root_hidden && self.children.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }

        for child in &self.children {
            let win = child.win.clone();
            let shared = self.shared.clone();
            let title = win.lock().unwrap_or_else(|e| e.into_inner()).title();
            let mut builder = viewport(&title);
            if let Some(position) = child.position {
                builder = builder.with_position(position);
            }
            ctx.show_viewport_deferred(child.id, builder, move |ui, _class| {
                let mut win = win.lock().unwrap_or_else(|e| e.into_inner());
                if win.closed {
                    return;
                }
                win.ui(ui, &shared);
                if ui.input(|i| i.viewport().close_requested()) {
                    win.closed = true;
                    win.unload(&shared);
                    // The root owns the list of windows; let it drop this one.
                    ui.ctx().request_repaint_of(ViewportId::ROOT);
                }
            });
        }

        if !self.root_hidden {
            self.root.ui(ui, &self.shared);
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        Theme::get(self.shared.dev.theme.unwrap_or(visuals.dark_mode)).bg.to_normalized_gamma_f32()
    }
}
