//! One document window: loading, virtualised layout, painting and interaction.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2,
    ViewportCommand, ViewportId, pos2, vec2,
};

use crate::doc::{self, Block, Doc, Kind, NO_LINK};
use crate::images::{Images, Status};
use crate::layout::{self, Deco, Env, Laid, LaidImage, range_rects};
use crate::theme::{self, COLUMN, Theme};
use crate::watch::{self, Watcher};
use crate::{log_err, macos, preload};

/// Space between the title bar and the first line of the document.
const TOP_MARGIN: f32 = 40.0;
const PAD_BOTTOM: f32 = 120.0;
const SIDE: f32 = 40.0;
const TOC_WIDTH: f32 = 216.0;
const ZOOM_MIN: f32 = 0.6;
const ZOOM_MAX: f32 = 2.6;

pub struct OpenRequest {
    pub path: PathBuf,
    pub anchor: Option<String>,
}

/// State shared by every window.
pub struct Shared {
    pub images: Arc<Images>,
    pub watcher: Watcher,
    /// Documents that should open in a new window.
    pub requests: Mutex<Vec<OpenRequest>>,
    /// Bumped whenever retained galleys become invalid (font atlas rebuilt).
    pub fonts_epoch: AtomicU64,
    atlas_fill: Mutex<f32>,
    pub dev: Dev,
    pub start: Instant,
}

/// Developer switches read from the environment (see README, "Development").
#[derive(Default)]
pub struct Dev {
    pub exit_after_frames: Option<u64>,
    pub screenshot: Option<PathBuf>,
    pub scroll: Option<f32>,
    pub theme: Option<bool>,
    pub find: Option<String>,
    /// Close window N this many seconds (+N) after launch, to test window lifecycle.
    pub close_after: Option<f32>,
}

impl Dev {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            exit_after_frames: var("LENSBLITZ_EXIT_AFTER_FRAMES").and_then(|v| v.parse().ok()),
            screenshot: var("LENSBLITZ_SCREENSHOT").map(PathBuf::from),
            scroll: var("LENSBLITZ_SCROLL").and_then(|v| v.parse().ok()),
            theme: var("LENSBLITZ_THEME").map(|v| v.eq_ignore_ascii_case("dark")),
            find: var("LENSBLITZ_FIND"),
            close_after: var("LENSBLITZ_CLOSE_AFTER").and_then(|v| v.parse().ok()),
        }
    }
}

impl Shared {
    pub fn new(ctx: &egui::Context, start: Instant) -> Self {
        Self {
            images: Arc::new(Images::new(ctx.clone())),
            watcher: Watcher::new(ctx.clone()),
            requests: Mutex::default(),
            fonts_epoch: AtomicU64::new(0),
            atlas_fill: Mutex::new(0.0),
            dev: Dev::from_env(),
            start,
        }
    }

    pub fn request_window(&self, ctx: &egui::Context, path: PathBuf, anchor: Option<String>) {
        self.requests.lock().unwrap_or_else(|e| e.into_inner()).push(OpenRequest { path, anchor });
        ctx.request_repaint_of(ViewportId::ROOT);
    }

    /// Call at the start of a window's frame. egui throws away its glyph atlas when it
    /// fills up (or when fonts change), which invalidates the galleys we hold on to; the
    /// fill ratio only ever drops when that happens.
    fn begin_frame(&self, ctx: &egui::Context) -> u64 {
        let fill = ctx.fonts(|f| f.font_atlas_fill_ratio());
        let mut last = self.atlas_fill.lock().unwrap_or_else(|e| e.into_inner());
        if fill + 1e-5 < *last {
            self.fonts_epoch.fetch_add(1, Ordering::AcqRel);
            crate::trace(self.start, &format!("glyph atlas rebuilt (fill {:.2} -> {fill:.2})", *last));
        }
        *last = fill;
        self.fonts_epoch.load(Ordering::Acquire)
    }

    fn end_frame(&self, ctx: &egui::Context) {
        let fill = ctx.fonts(|f| f.font_atlas_fill_ratio());
        *self.atlas_fill.lock().unwrap_or_else(|e| e.into_inner()) = fill;
    }
}

#[derive(Default)]
struct Slot {
    height: f32,
    /// `height` is exact for the current layout key.
    measured: bool,
    laid: Option<Box<Laid>>,
}

/// A position in the document text.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Cur {
    block: usize,
    piece: usize,
    ch: usize,
}

#[derive(Clone, Copy)]
struct Selection {
    anchor: Cur,
    head: Cur,
}

impl Selection {
    fn range(&self) -> (Cur, Cur) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy)]
struct Match {
    block: u32,
    piece: u32,
    start: u32,
    end: u32,
}

#[derive(Default)]
struct Find {
    open: bool,
    query: String,
    matches: Vec<Match>,
    current: usize,
    dirty: bool,
    focus: bool,
    jump: bool,
}

#[derive(Clone, Copy)]
enum Target {
    /// An absolute scroll offset.
    Y(f32),
    /// Bring the point `y` points into block `idx` to `screen_y` points below the window top.
    /// Resolved every frame, so it stays right while block heights are still being measured.
    Block { idx: usize, y: f32, screen_y: f32 },
}

struct Anim {
    from: f32,
    to: Target,
    start: f64,
    duration: f32,
}

struct HistoryEntry {
    path: PathBuf,
    /// Reading position: block index and offset into it.
    position: (usize, f32),
}

/// A block painted this frame, for hit testing.
struct Vis {
    idx: usize,
    origin: Pos2,
    bottom: f32,
    hscroll: f32,
}

/// Things discovered while painting that are acted on after the pass.
#[derive(Default)]
struct Frame {
    pointer: Option<Pos2>,
    vis: Vec<Vis>,
    hover_link: Option<(usize, u32)>,
    over_text: bool,
    hover_wide: Option<(u32, f32, Rect)>,
    hover_panel: Option<(usize, Rect)>,
    relayout: Vec<usize>,
    loading_images: bool,
}

pub struct Win {
    pub id: u64,
    pub viewport: ViewportId,
    pub closed: bool,
    pub path: Option<PathBuf>,
    base_dir: Option<Arc<PathBuf>>,
    doc: Option<Arc<Doc>>,
    error: Option<String>,
    seen: watch::Signature,
    changed: Arc<AtomicBool>,

    slots: Vec<Slot>,
    tops: Vec<f32>,
    total: f32,
    tops_dirty: bool,
    needs_estimate: bool,
    key: (u32, u32, u32),
    dark: bool,
    epoch: u64,
    measure_cursor: usize,
    laid_range: (usize, usize),

    zoom: f32,
    /// Height of the transparent title bar floating over the content (0 in full screen).
    titlebar: f32,
    scroll_y: f32,
    view_h: f32,
    anim: Option<Anim>,
    pending_anchor: Option<String>,
    pending_scroll: Option<f32>,
    /// Reading position to restore once the new layout exists (after a reload or going back).
    restore: Option<(usize, f32)>,
    autoscroll: f32,

    find: Find,
    sel: Option<Selection>,
    selecting: bool,
    hscroll: HashMap<u32, f32>,
    group_width: HashMap<u32, f32>,
    hover_group: Option<u32>,
    copied: Option<(u32, f64)>,
    hover_url: Option<String>,
    toast: Option<(String, f64)>,
    back: Vec<HistoryEntry>,
    forward: Vec<HistoryEntry>,
    toc_hidden: bool,

    frames: u64,
    title_sent: String,
    shot_requested: bool,
    dev_scrolled: bool,
    dev_closed: bool,
}

fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| crate::MARKDOWN_EXTENSIONS.iter().any(|m| m.eq_ignore_ascii_case(e)))
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

impl Win {
    pub fn new(id: u64, viewport: ViewportId) -> Self {
        Self {
            id,
            viewport,
            closed: false,
            path: None,
            base_dir: None,
            doc: None,
            error: None,
            seen: None,
            changed: Arc::new(AtomicBool::new(false)),
            slots: Vec::new(),
            tops: Vec::new(),
            total: 0.0,
            tops_dirty: false,
            needs_estimate: false,
            key: (0, 0, 0),
            dark: false,
            epoch: 0,
            measure_cursor: 0,
            laid_range: (0, 0),
            zoom: 1.0,
            titlebar: macos::titlebar_height(),
            scroll_y: 0.0,
            view_h: 0.0,
            anim: None,
            pending_anchor: None,
            pending_scroll: None,
            restore: None,
            autoscroll: 0.0,
            find: Find::default(),
            sel: None,
            selecting: false,
            hscroll: HashMap::new(),
            group_width: HashMap::new(),
            hover_group: None,
            copied: None,
            hover_url: None,
            toast: None,
            back: Vec::new(),
            forward: Vec::new(),
            toc_hidden: false,
            frames: 0,
            title_sent: String::new(),
            shot_requested: false,
            dev_scrolled: false,
            dev_closed: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.path.is_none()
    }

    pub fn title(&self) -> String {
        match self.path.as_deref().and_then(Path::file_name) {
            Some(name) => format!("LensBlitz — {}", name.to_string_lossy()),
            None => "LensBlitz".to_owned(),
        }
    }

    // --- loading -----------------------------------------------------------

    /// Show `path` in this window, replacing whatever is there.
    pub fn open(&mut self, path: &Path, anchor: Option<String>, shared: &Shared) {
        let path = absolute(path);
        self.base_dir = path.parent().map(|p| Arc::new(p.to_owned()));
        self.path = Some(path.clone());
        self.doc = None;
        self.slots.clear();
        self.sel = None;
        self.hscroll.clear();
        self.group_width.clear();
        self.scroll_y = 0.0;
        self.pending_scroll = Some(0.0);
        self.restore = None;
        self.anim = None;
        self.find.dirty = true;
        self.pending_anchor = anchor;
        self.read(false);
        shared.watcher.watch(self.id, path, self.seen, self.changed.clone(), self.viewport);
    }

    /// Forget the document (the window is going away or being hidden).
    pub fn unload(&mut self, shared: &Shared) {
        shared.watcher.unwatch(self.id);
        self.path = None;
        self.doc = None;
        self.error = None;
        self.slots.clear();
        self.back.clear();
        self.forward.clear();
        self.find = Find::default();
        self.sel = None;
    }

    /// Read and parse the file. With `keep`, layout of unchanged blocks and the reading
    /// position are carried over (live reload).
    fn read(&mut self, keep: bool) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let started = Instant::now();
        // A fresh open may already have been parsed in the background during launch.
        let loaded = (if keep { None } else { preload::take(&path) }).unwrap_or_else(|| preload::load(&path));
        let new = match loaded {
            Ok(parsed) => {
                self.seen = parsed.seen;
                parsed.doc
            }
            Err(err) => {
                log_err!("cannot read {}: {err}", path.display());
                self.seen = watch::signature(&path);
                if self.doc.is_none() {
                    self.error = Some(err);
                }
                return;
            }
        };
        self.error = None;

        let old = self.doc.take();
        let mut slots: Vec<Slot> = Vec::new();
        slots.resize_with(new.blocks.len(), Slot::default);
        let mut anchor = None;
        if let (true, Some(old)) = (keep, old.as_deref()) {
            // Blocks before and after the edit keep their measured height and galleys.
            let (ol, nl) = (old.blocks.len(), new.blocks.len());
            let prefix = old.blocks.iter().zip(&new.blocks).take_while(|(a, b)| a.hash == b.hash).count();
            let suffix = old.blocks[prefix..]
                .iter()
                .rev()
                .zip(new.blocks[prefix..].iter().rev())
                .take_while(|(a, b)| a.hash == b.hash)
                .count();
            let (a, off) = self.anchor();
            let new_a = if a < prefix {
                a
            } else if a >= ol - suffix {
                a + nl - ol
            } else {
                a.min(nl.saturating_sub(1))
            };
            anchor = Some((new_a, off));
            for (new, old) in slots.iter_mut().zip(&mut self.slots).take(prefix) {
                *new = std::mem::take(old);
            }
            for i in 0..suffix {
                slots[nl - 1 - i] = std::mem::take(&mut self.slots[ol - 1 - i]);
            }
        }
        self.slots = slots;
        self.tops = vec![0.0; self.slots.len()];
        self.doc = Some(Arc::new(new));
        self.needs_estimate = true;
        self.tops_dirty = true;
        self.measure_cursor = 0;
        self.laid_range = (0, self.slots.len());
        self.find.dirty = true;
        self.sel = None;
        if anchor.is_some() {
            // Resolved against the new layout on the next frame.
            self.pending_scroll = None;
            self.restore = anchor;
        }
        if std::env::var_os("LENSBLITZ_TRACE").is_some() {
            // The wall-clock stamp lets a test compare this with the moment it saved the file.
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            eprintln!(
                "lensblitz: loaded {} ({} blocks) in {:.2} ms [at {:.1} ms]",
                path.display(),
                self.slots.len(),
                started.elapsed().as_secs_f64() * 1e3,
                now.as_secs_f64() * 1e3 % 1e8
            );
        }
    }

    // --- geometry ----------------------------------------------------------

    /// Offset of the first block from the top of the scrolled content.
    fn pad_top(&self) -> f32 {
        self.titlebar + TOP_MARGIN
    }

    fn rebuild_tops(&mut self) {
        let mut y = 0.0;
        self.tops.resize(self.slots.len(), 0.0);
        for (top, slot) in self.tops.iter_mut().zip(&self.slots) {
            *top = y;
            y += slot.height;
        }
        self.total = y;
        self.tops_dirty = false;
    }

    /// Index of the block containing content offset `y` (0 = top of the first block).
    fn block_at(&self, y: f32) -> usize {
        match self.tops.binary_search_by(|t| t.total_cmp(&y)) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// The block at the top of the viewport and how far into it we have scrolled.
    fn anchor(&self) -> (usize, f32) {
        if self.tops.is_empty() {
            return (0, 0.0);
        }
        let y = self.scroll_y - self.pad_top() + self.titlebar;
        let i = self.block_at(y);
        (i, y - self.tops[i])
    }

    fn max_scroll(&self) -> f32 {
        (self.pad_top() + self.total + PAD_BOTTOM - self.view_h).max(0.0)
    }

    fn resolve(&self, target: Target) -> f32 {
        let y = match target {
            Target::Y(y) => y,
            Target::Block { idx, y, screen_y } => {
                self.tops.get(idx).copied().unwrap_or(0.0) + y + self.pad_top() - screen_y
            }
        };
        y.clamp(0.0, self.max_scroll())
    }

    fn scroll_to(&mut self, ctx: &egui::Context, target: Target) {
        let to = self.resolve(target);
        let distance = (to - self.scroll_y).abs();
        if distance < 0.5 {
            return;
        }
        self.anim = Some(Anim {
            from: self.scroll_y,
            to: target,
            start: ctx.input(|i| i.time),
            duration: (0.16 + distance / 9000.0).min(0.34),
        });
        ctx.request_repaint();
    }

    /// Scroll so that block `idx` (a heading, usually) sits just below the title bar.
    fn scroll_to_block(&mut self, ctx: &egui::Context, idx: usize) {
        let gap = self
            .doc
            .as_ref()
            .and_then(|d| d.blocks.get(idx))
            .map_or(0.0, |b| b.gap as f32 / 100.0 * theme::BASE * self.zoom);
        self.scroll_to(ctx, Target::Block { idx, y: gap, screen_y: self.titlebar + 20.0 });
    }

    fn scroll_by(&mut self, ctx: &egui::Context, delta: f32) {
        // Chain onto a running animation so held keys keep accelerating smoothly.
        let base = match &self.anim {
            Some(Anim { to: Target::Y(y), .. }) => *y,
            _ => self.scroll_y,
        };
        self.scroll_to(ctx, Target::Y((base + delta).clamp(0.0, self.max_scroll())));
    }

    fn scroll_to_anchor(&mut self, ctx: &egui::Context, anchor: &str) -> bool {
        let Some(doc) = &self.doc else {
            return false;
        };
        let wanted = anchor.trim_start_matches('#');
        let decoded = percent_decode(wanted);
        let hit = doc
            .anchors
            .get(wanted)
            .or_else(|| doc.anchors.get(&decoded))
            .or_else(|| doc.anchors.get(&decoded.to_lowercase()))
            .copied();
        match hit {
            Some(block) => {
                self.scroll_to_block(ctx, block);
                true
            }
            None => false,
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        // Land exactly on 100 % when stepping past it.
        self.zoom = if (zoom - 1.0).abs() < 0.03 { 1.0 } else { zoom };
    }

    // --- frame -------------------------------------------------------------

    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let ctx = ui.ctx().clone();
        let dark = shared.dev.theme.unwrap_or(ctx.theme() == egui::Theme::Dark);
        let theme = Theme::get(dark);
        let epoch = shared.begin_frame(&ctx);
        self.frames += 1;
        // In full screen the title bar is hidden, so nothing floats over the page.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        self.titlebar = if fullscreen { 0.0 } else { macos::titlebar_height() };

        if self.changed.swap(false, Ordering::AcqRel) {
            self.read(true);
        }
        self.dropped_files(&ctx, shared);

        let screen = ui.max_rect();
        ui.painter().rect_filled(screen, 0.0, theme.bg);

        self.keys(&ctx, shared);

        let mut frame = Frame::default();
        if let Some(doc) = self.doc.clone() {
            self.document(ui, shared, theme, epoch, &doc, &mut frame);
        } else {
            self.placeholder(ui, theme);
        }
        self.chrome(ui, theme, screen);
        if self.doc.is_some() {
            self.find_bar(&ctx, theme, screen);
        }
        self.status(&ctx, theme, screen);
        self.drop_hint(&ctx, theme, screen);

        let title = self.title();
        if title != self.title_sent {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title_sent = title;
        }
        shared.end_frame(&ctx);
        self.dev_hooks(&ctx, shared, &frame);
    }

    fn document(
        &mut self,
        ui: &mut egui::Ui,
        shared: &Arc<Shared>,
        theme: &'static Theme,
        epoch: u64,
        doc: &Arc<Doc>,
        frame: &mut Frame,
    ) {
        let ctx = ui.ctx().clone();
        let screen = ui.max_rect();
        let ppp = ctx.pixels_per_point();
        self.view_h = screen.height();

        // Column geometry: centred, or pushed right to make room for the outline.
        let col_w = snap((screen.width() - 2.0 * SIDE).min(COLUMN * self.zoom).max(160.0), ppp);
        let toc_fits = screen.width() >= 28.0 + TOC_WIDTH + 36.0 + col_w + SIDE;
        let show_toc = !self.toc_hidden && toc_fits && doc.outline.len() >= 3;
        let centered = screen.left() + (screen.width() - col_w) / 2.0;
        let col_x = snap(if show_toc { centered.max(screen.left() + 28.0 + TOC_WIDTH + 36.0) } else { centered }, ppp);

        // Anything that changes line breaks invalidates heights; anything that only
        // changes pixels (theme, glyph atlas) just drops the galleys.
        let anchor = self.anchor();
        let key = (col_w.to_bits(), self.zoom.to_bits(), ppp.to_bits());
        let mut rekeyed = false;
        if key != self.key {
            rekeyed = self.key != (0, 0, 0);
            self.key = key;
            for slot in &mut self.slots {
                slot.measured = false;
                slot.laid = None;
            }
            self.needs_estimate = true;
            self.measure_cursor = 0;
            self.group_width.clear();
        }
        if self.dark != theme.dark || self.epoch != epoch {
            self.dark = theme.dark;
            self.epoch = epoch;
            for slot in &mut self.slots {
                slot.laid = None;
            }
        }

        let base_dir = self.base_dir.clone();
        let env = Env {
            ctx: &ctx,
            theme,
            zoom: self.zoom,
            ppp,
            width: col_w,
            images: &shared.images,
            base_dir: base_dir.as_deref().map(|p| p.as_path()),
        };

        if self.needs_estimate {
            for (slot, block) in self.slots.iter_mut().zip(&doc.blocks) {
                if !slot.measured {
                    slot.height = layout::estimate_height(block, &env);
                }
            }
            self.needs_estimate = false;
            self.tops_dirty = true;
        }
        // Measure the rest of the document a few milliseconds at a time. The first frame
        // skips this entirely so the window appears as soon as possible.
        let more = self.frames > 1 && self.measure(doc, &env, Duration::from_micros(3500));
        if self.tops_dirty {
            self.rebuild_tops();
        }
        if more {
            ctx.request_repaint();
        }

        // Keep the same content under the top edge when heights above it changed.
        let mut forced = None;
        if let Some((a, off)) = self.restore.take().filter(|_| !self.tops.is_empty()) {
            self.pending_scroll = None;
            let a = a.min(self.tops.len() - 1);
            forced = Some(self.tops[a] + off + self.pad_top() - self.titlebar);
        } else if let Some(y) = self.pending_scroll.take() {
            forced = Some(y);
        } else if !self.tops.is_empty() {
            let (a, off) = anchor;
            let a = a.min(self.tops.len() - 1);
            let off = if rekeyed { off.min(self.slots[a].height) } else { off };
            let y = self.tops[a] + off + self.pad_top() - self.titlebar;
            if (y - self.scroll_y).abs() > 0.05 && self.scroll_y > 0.5 {
                forced = Some(y);
            }
        }
        if let Some(anchor) = self.pending_anchor.take()
            && !self.scroll_to_anchor(&ctx, &anchor)
        {
            log_err!("no heading matches #{anchor}");
        }
        if self.find.dirty {
            self.search(doc);
        }
        if self.find.jump {
            self.find.jump = false;
            self.jump_to_match(&ctx, doc, &env);
        }
        if self.autoscroll != 0.0 {
            forced = Some(forced.unwrap_or(self.scroll_y) + self.autoscroll);
            self.autoscroll = 0.0;
        }
        let user_scrolled = ctx.input(|i| i.smooth_scroll_delta.y != 0.0);
        if user_scrolled {
            self.anim = None;
        }
        if let Some(anim) = &self.anim {
            let t = ((ctx.input(|i| i.time) - anim.start) as f32 / anim.duration).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            let to = self.resolve(anim.to);
            forced = Some(anim.from + (to - anim.from) * eased);
            if t >= 1.0 {
                self.anim = None;
            }
            ctx.request_repaint();
        }
        if let Some(y) = &mut forced {
            *y = y.clamp(0.0, self.max_scroll());
        }

        frame.pointer = ctx.pointer_hover_pos().filter(|p| p.y > screen.top() + self.titlebar);

        let mut area = egui::ScrollArea::vertical()
            .id_salt(("doc", self.id))
            .auto_shrink(false)
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: egui::scroll_area::DragScroll::Never,
                ..Default::default()
            })
            .scroll_bar_rect(Rect::from_min_max(pos2(screen.left(), screen.top() + self.titlebar + 2.0), screen.max));
        if let Some(y) = forced {
            area = area.vertical_scroll_offset(y);
        }
        let content_h = self.pad_top() + self.total + PAD_BOTTOM;
        let out = area.show_viewport(ui, |ui, vp| {
            let origin = ui.cursor().min;
            ui.allocate_rect(Rect::from_min_size(origin, vec2(screen.width(), content_h)), Sense::hover());
            self.paint_blocks(ui, shared, theme, doc, &env, origin, vp, col_x, frame);
        });
        self.scroll_y = out.state.clamped_offset().y;
        if self.tops_dirty {
            self.rebuild_tops();
            ctx.request_repaint();
        }

        self.interact(ui, shared, theme, doc, &env, frame, out.inner_rect);
        if show_toc {
            self.outline(ui, theme, doc, screen, col_x);
        }
    }

    /// Lay out not-yet-measured blocks until `budget` runs out. Returns true if work remains.
    fn measure(&mut self, doc: &Doc, env: &Env<'_>, budget: Duration) -> bool {
        let started = Instant::now();
        while self.measure_cursor < self.slots.len() {
            let i = self.measure_cursor;
            self.measure_cursor += 1;
            let slot = &mut self.slots[i];
            if slot.measured {
                continue;
            }
            let laid = layout::layout_block(&doc.blocks[i], env);
            if (slot.height - laid.height).abs() > 0.01 {
                self.tops_dirty = true;
            }
            slot.height = laid.height;
            slot.measured = true;
            // The galleys are dropped: only blocks near the viewport keep theirs.
            if started.elapsed() > budget {
                return self.measure_cursor < self.slots.len();
            }
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_blocks(
        &mut self,
        ui: &mut egui::Ui,
        shared: &Arc<Shared>,
        theme: &'static Theme,
        doc: &Doc,
        env: &Env<'_>,
        origin: Pos2,
        vp: Rect,
        col_x: f32,
        frame: &mut Frame,
    ) {
        if self.slots.is_empty() {
            return;
        }
        let ppp = env.ppp;
        let top = vp.min.y - self.pad_top();
        let bottom = vp.max.y - self.pad_top();
        let first = self.block_at(top);
        let mut y = self.tops[first];
        let mut i = first;
        while i < self.slots.len() && y < bottom {
            if self.slots[i].laid.is_none() {
                let laid = layout::layout_block(&doc.blocks[i], env);
                let slot = &mut self.slots[i];
                if (slot.height - laid.height).abs() > 0.01 {
                    self.tops_dirty = true;
                }
                slot.height = laid.height;
                slot.measured = true;
                slot.laid = Some(Box::new(laid));
            }
            let Some(laid) = self.slots[i].laid.take() else {
                break;
            };
            let block_origin = pos2(col_x, snap(origin.y + self.pad_top() + y, ppp));
            let hscroll = self.paint_block(ui, shared, theme, &doc.blocks[i], i, &laid, block_origin, frame);
            frame.vis.push(Vis { idx: i, origin: block_origin, bottom: block_origin.y + laid.height, hscroll });
            y += laid.height;
            self.slots[i].laid = Some(laid);
            i += 1;
        }

        // Drop galleys that scrolled far away so memory tracks the viewport, not the file.
        let keep = (first.saturating_sub(24), (i + 24).min(self.slots.len()));
        let (lo, hi) = self.laid_range;
        for j in (lo..hi.min(self.slots.len())).filter(|j| *j < keep.0 || *j >= keep.1) {
            self.slots[j].laid = None;
        }
        self.laid_range = (lo.max(keep.0).min(first), hi.min(keep.1).max(i));

        for &j in &frame.relayout {
            self.slots[j].laid = None;
            self.slots[j].measured = false;
            self.tops_dirty = true;
        }
    }

    /// Paint one block. Returns its horizontal scroll offset.
    #[allow(clippy::too_many_arguments)]
    fn paint_block(
        &mut self,
        ui: &egui::Ui,
        shared: &Arc<Shared>,
        theme: &'static Theme,
        block: &Block,
        idx: usize,
        laid: &Laid,
        origin: Pos2,
        frame: &mut Frame,
    ) -> f32 {
        let p = ui.painter();
        let ov = origin.to_vec2();
        let zoom = self.zoom;
        for d in &laid.decos {
            paint_deco(p, ov, d, theme, zoom);
        }

        // Wide content (code, tables) scrolls sideways inside its own clip rectangle.
        let mut hs = 0.0;
        let mut wide_painter = p.clone();
        if let Some(wide) = &laid.wide {
            let known = self.group_width.entry(block.group).or_insert(0.0);
            *known = known.max(wide.content_width);
            let max = (*known - wide.clip.width()).max(0.0);
            let clip = wide.clip.translate(ov);
            if max > 0.5 {
                let x = self.hscroll.entry(block.group).or_insert(0.0);
                *x = x.clamp(0.0, max);
                hs = *x;
                if frame.pointer.is_some_and(|ptr| clip.contains(ptr)) {
                    frame.hover_wide = Some((block.group, max, clip));
                }
            }
            wide_painter = p.with_clip_rect(clip.intersect(p.clip_rect()));
        }
        let wov = ov - vec2(hs, 0.0);
        for d in &laid.wide_decos {
            paint_deco(&wide_painter, wov, d, theme, zoom);
        }

        for im in &laid.images {
            self.paint_image(ui, shared, theme, block, idx, im, ov, frame);
        }

        // Find matches and the selection sit between the decorations and the text.
        if self.find.open && !self.find.matches.is_empty() {
            let lo = self.find.matches.partition_point(|m| (m.block as usize) < idx);
            for (n, m) in self.find.matches[lo..].iter().enumerate().take_while(|(_, m)| m.block as usize == idx) {
                let Some(t) = laid.texts.iter().find(|t| t.piece == m.piece) else {
                    continue;
                };
                let color = if lo + n == self.find.current { theme.find_active } else { theme.find };
                for r in range_rects(&t.galley, m.start as usize..m.end as usize) {
                    wide_painter.rect_filled(r.translate(wov + t.pos).expand2(vec2(1.0, -2.0 * zoom)), 3.0, color);
                }
            }
        }
        if let Some(sel) = self.sel.filter(|s| !s.is_empty()) {
            let (lo, hi) = sel.range();
            if idx >= lo.block && idx <= hi.block {
                for t in &laid.texts {
                    let here = (idx, t.piece as usize);
                    if here < (lo.block, lo.piece) || here > (hi.block, hi.piece) {
                        continue;
                    }
                    let start = if here == (lo.block, lo.piece) { lo.ch } else { 0 };
                    let end = if here == (hi.block, hi.piece) { hi.ch } else { usize::MAX };
                    for r in range_rects(&t.galley, start..end) {
                        wide_painter.rect_filled(r.translate(wov + t.pos), 0.0, theme.selection);
                    }
                }
            }
        }

        for t in &laid.texts {
            wide_painter.galley((wov + t.pos).to_pos2(), t.galley.clone(), theme.text);
        }
        // The language tag steps aside once code is scrolled under it.
        if !(matches!(block.kind, Kind::Code { .. }) && hs > 0.5) {
            for (pos, g) in &laid.labels {
                p.galley((ov + *pos).to_pos2(), g.clone(), theme.muted);
            }
        }

        // Scroll hint and thumb for wide content.
        if let Some(wide) = &laid.wide {
            let known = self.group_width.get(&block.group).copied().unwrap_or(wide.content_width);
            let max = (known - wide.clip.width()).max(0.0);
            if max > 0.5 {
                let clip = wide.clip.translate(ov);
                let fill = if matches!(block.kind, Kind::Code { .. }) { theme.surface } else { theme.bg };
                let fade_w = 28.0 * zoom;
                if hs < max - 0.5 {
                    fade(p, Rect::from_min_max(pos2(clip.right() - fade_w, clip.top()), clip.max), fill, true);
                }
                if hs > 0.5 {
                    fade(p, Rect::from_min_max(clip.min, pos2(clip.left() + fade_w, clip.bottom())), fill, false);
                }
                if self.hover_group == Some(block.group) {
                    let track = clip.width() - 16.0;
                    let w = (track * clip.width() / known).max(24.0);
                    let x = clip.left() + 8.0 + (track - w) * (hs / max);
                    let visible_bottom = clip.bottom().min(p.clip_rect().bottom());
                    let thumb = Rect::from_min_size(pos2(x, visible_bottom - 7.0), vec2(w, 3.0));
                    p.rect_filled(thumb, 2.0, theme::mix(fill, theme.muted, 0.55));
                }
            }
        }

        for d in &laid.over {
            paint_deco(p, ov, d, theme, zoom);
        }

        // Hover state for this block.
        if let Some(ptr) = frame.pointer
            && ptr.y >= origin.y
            && ptr.y < origin.y + laid.height
        {
            let in_wide = laid.wide.as_ref().is_none_or(|w| w.clip.translate(ov).contains(ptr));
            if in_wide {
                let local = ptr - wov;
                if let Some((_, link)) = laid.links.iter().find(|(r, _)| r.expand(1.0).contains(local)) {
                    frame.hover_link = Some((idx, *link));
                    for (r, _) in laid.links.iter().filter(|(_, l)| l == link) {
                        let r = r.translate(wov);
                        let y = snap(r.top() + r.height() * 0.84, ui.ctx().pixels_per_point()) + 0.5;
                        wide_painter.hline(r.x_range(), y, Stroke::new(1.0, theme.link));
                    }
                }
                frame.over_text |= laid.texts.iter().any(|t| t.rect().translate(wov).expand(2.0).contains(ptr));
            }
            for im in &laid.images {
                let link = block.images[im.image as usize].link;
                if link != NO_LINK && im.rect.translate(ov).contains(ptr) {
                    frame.hover_link = Some((idx, link));
                }
            }
            if let Some(panel) = laid.panel
                && panel.translate(ov).contains(ptr)
            {
                frame.hover_panel = Some((idx, panel.translate(ov)));
            }
        }
        hs
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_image(
        &self,
        ui: &egui::Ui,
        shared: &Arc<Shared>,
        theme: &Theme,
        block: &Block,
        idx: usize,
        im: &LaidImage,
        ov: Vec2,
        frame: &mut Frame,
    ) {
        let rect = im.rect.translate(ov);
        let p = ui.painter();
        if !p.clip_rect().expand(600.0).intersects(rect) {
            return;
        }
        let small = rect.height() < 40.0;
        let radius = CornerRadius::same(if small { 3 } else { (6.0 * self.zoom).round() as u8 });
        let image = &block.images[im.image as usize];
        let status = im.key.as_deref().map(|k| (k, shared.images.get(k)));
        let fade_id = Id::unique(("img", self.id, idx, im.image));
        match status {
            Some((key, Status::Ready(tex, opaque))) => {
                if !im.sized && shared.images.natural_size(key).is_some() {
                    frame.relayout.push(idx);
                }
                let t = ui.ctx().animate_bool_with_time(fade_id, true, 0.16);
                egui::Image::new((tex.id(), rect.size()))
                    .corner_radius(radius)
                    .tint(Color32::WHITE.gamma_multiply(t))
                    .paint_at(ui, rect);
                if opaque && !small && !theme.dark {
                    // A hairline keeps white-background screenshots from bleeding into the page.
                    p.rect_stroke(rect, radius, Stroke::new(1.0, Color32::from_black_alpha(14)), StrokeKind::Inside);
                }
            }
            Some((_, Status::Loading)) => {
                ui.ctx().animate_bool_with_time(fade_id, false, 0.16);
                frame.loading_images = true;
                p.rect_filled(rect, radius, theme.surface);
            }
            Some((_, Status::Failed)) | None => {
                p.rect(rect, radius, theme.surface, Stroke::new(1.0, theme.hairline), StrokeKind::Inside);
                if !small {
                    let label = if image.alt.is_empty() { "Image unavailable" } else { image.alt.as_str() };
                    let mut job = LayoutJob::single_section(
                        label.to_owned(),
                        theme::ui_format(12.5 * self.zoom, 450.0, theme.faint),
                    );
                    job.wrap.max_width = rect.width() - 24.0;
                    job.wrap.max_rows = 2;
                    job.halign = egui::Align::Center;
                    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                    let pos = pos2(rect.center().x, rect.center().y - galley.size().y / 2.0);
                    p.with_clip_rect(rect.intersect(p.clip_rect())).galley(pos, galley, theme.faint);
                }
            }
        }
    }

    // --- pointer -----------------------------------------------------------

    /// Map a screen position to the nearest text position among the painted blocks.
    fn hit(&self, frame: &Frame, pos: Pos2) -> Option<Cur> {
        let vis = frame.vis.iter().find(|v| pos.y < v.bottom).or(frame.vis.last())?;
        let laid = self.slots.get(vis.idx)?.laid.as_ref()?;
        let offset = vis.origin.to_vec2() - vec2(vis.hscroll, 0.0);
        let dist = |r: Rect| {
            let dx = (r.left() - pos.x).max(pos.x - r.right()).max(0.0);
            let dy = (r.top() - pos.y).max(pos.y - r.bottom()).max(0.0);
            dy * 4.0 + dx
        };
        let text = laid
            .texts
            .iter()
            .min_by(|a, b| dist(a.rect().translate(offset)).total_cmp(&dist(b.rect().translate(offset))));
        match text {
            Some(t) => {
                let cursor = t.galley.cursor_from_pos((pos - offset - t.pos).to_vec2());
                Some(Cur { block: vis.idx, piece: t.piece as usize, ch: cursor.index.0 })
            }
            // A block without text (rule, images): snap to its start.
            None => Some(Cur { block: vis.idx, piece: 0, ch: 0 }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn interact(
        &mut self,
        ui: &mut egui::Ui,
        shared: &Arc<Shared>,
        theme: &Theme,
        doc: &Doc,
        env: &Env<'_>,
        frame: &mut Frame,
        area: Rect,
    ) {
        let ctx = ui.ctx().clone();
        let _ = env;

        // Sideways scrolling of the wide block under the pointer.
        self.hover_group = frame.hover_wide.map(|w| w.0);
        if let Some((group, max, _)) = frame.hover_wide {
            let dx = ctx.input(|i| i.smooth_scroll_delta.x);
            if dx != 0.0 {
                let x = self.hscroll.entry(group).or_insert(0.0);
                *x = (*x - dx).clamp(0.0, max);
                ctx.request_repaint();
            }
        }

        // Copy button for the code block under the pointer.
        let mut on_button = false;
        if let Some((idx, panel)) = frame.hover_panel {
            let top = panel.top().max(area.top() + self.titlebar) + 6.0;
            let rect = Rect::from_min_size(pos2(panel.right() - 34.0, top), vec2(28.0, 26.0));
            if rect.bottom() < panel.bottom() + 2.0 {
                let group = doc.blocks[idx].group;
                let now = ctx.input(|i| i.time);
                let done = self.copied.is_some_and(|(g, t)| g == group && now - t < 1.4);
                let hovered = frame.pointer.is_some_and(|p| rect.contains(p));
                on_button = hovered;
                let p = ui.painter();
                p.rect(
                    rect,
                    6.0,
                    if hovered { theme.surface_hi } else { theme.surface },
                    Stroke::new(1.0, theme.border),
                    StrokeKind::Inside,
                );
                let color = if done {
                    theme.syn[crate::highlight::tok::INS as usize]
                } else if hovered {
                    theme.strong
                } else {
                    theme.muted
                };
                if done {
                    check_icon(p, rect.center(), 5.0, Stroke::new(1.6, color));
                    ctx.request_repaint();
                } else {
                    copy_icon(p, rect.center(), color, if hovered { theme.surface_hi } else { theme.surface });
                }
                if hovered && ctx.input(|i| i.pointer.primary_clicked()) {
                    let text: Vec<&str> = doc
                        .blocks
                        .iter()
                        .skip(group as usize)
                        .take_while(|b| b.group == group)
                        .map(|b| b.pieces[0].text.as_str())
                        .collect();
                    ctx.copy_text(text.join("\n"));
                    self.copied = Some((group, now));
                }
            }
        }

        // The document surface: click links, drag to select.
        // Leave the right edge to the scroll bar, which would otherwise sit underneath.
        let surface = Rect::from_min_max(area.min, pos2(area.right() - 14.0, area.bottom()));
        let response = ui.interact(surface, Id::unique(("surface", self.id)), Sense::click_and_drag());
        let link = frame.hover_link.and_then(|(b, l)| doc.blocks.get(b)?.links.get(l as usize).cloned());
        self.hover_url = link.clone().filter(|_| !on_button);
        if on_button || link.is_some() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        } else if frame.over_text || self.selecting {
            ctx.set_cursor_icon(CursorIcon::Text);
        }
        if on_button {
            return;
        }

        if response.drag_started()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(cur) = self.hit(frame, pos)
        {
            let extend = ctx.input(|i| i.modifiers.shift);
            self.sel = Some(match (extend, self.sel) {
                (true, Some(sel)) => Selection { anchor: sel.anchor, head: cur },
                _ => Selection { anchor: cur, head: cur },
            });
            self.selecting = true;
        }
        if self.selecting {
            if let Some(pos) = ctx.pointer_latest_pos()
                && let Some(cur) = self.hit(frame, pos)
                && let Some(sel) = &mut self.sel
            {
                sel.head = cur;
                // Keep scrolling while the pointer is held past the top or bottom edge.
                let edge = 36.0;
                if pos.y < area.top() + self.titlebar + edge {
                    self.autoscroll = -((area.top() + self.titlebar + edge - pos.y) * 0.35).min(40.0);
                } else if pos.y > area.bottom() - edge {
                    self.autoscroll = ((pos.y - (area.bottom() - edge)) * 0.35).min(40.0);
                }
                if self.autoscroll != 0.0 {
                    ctx.request_repaint();
                }
            }
            if !ctx.input(|i| i.pointer.primary_down()) {
                self.selecting = false;
            }
        }
        if response.triple_clicked() {
            if let Some(cur) = response.interact_pointer_pos().and_then(|p| self.hit(frame, p)) {
                self.sel = Some(Selection { anchor: Cur { ch: 0, ..cur }, head: Cur { ch: usize::MAX, ..cur } });
            }
        } else if response.double_clicked() {
            if let Some(cur) = response.interact_pointer_pos().and_then(|p| self.hit(frame, p))
                && let Some(piece) = doc.blocks.get(cur.block).and_then(|b| b.pieces.get(cur.piece))
            {
                let (a, b) = word_at(&piece.text, cur.ch);
                self.sel = Some(Selection { anchor: Cur { ch: a, ..cur }, head: Cur { ch: b, ..cur } });
            }
        } else if response.clicked() {
            match link {
                Some(url) => {
                    let new_window = ctx.input(|i| i.modifiers.command);
                    self.follow(&ctx, shared, &url, new_window);
                }
                None => self.sel = None,
            }
        }
    }

    // --- links, files ------------------------------------------------------

    fn follow(&mut self, ctx: &egui::Context, shared: &Arc<Shared>, url: &str, new_window: bool) {
        let url = url.trim();
        if let Some(anchor) = url.strip_prefix('#') {
            if !self.scroll_to_anchor(ctx, anchor) {
                self.toast(ctx, "No such heading");
            }
            return;
        }
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
            macos::open_external(url);
            return;
        }
        if lower.contains("://") && !lower.starts_with("file://") {
            log_err!("refusing to open link with unsupported scheme: {url}");
            self.toast(ctx, "Unsupported link");
            return;
        }
        // A local path, possibly with a `#heading`.
        let raw = url.strip_prefix("file://").unwrap_or(url);
        let (raw, anchor) = match raw.split_once('#') {
            Some((p, a)) => (p, Some(a.to_owned())),
            None => (raw, None),
        };
        let decoded = percent_decode(raw);
        let mut path = PathBuf::from(&decoded);
        if !path.is_absolute()
            && let Some(dir) = &self.base_dir
        {
            path = dir.join(path);
        }
        if !path.exists() && path.extension().is_none() {
            // Wiki-style links name the note without its extension.
            let with_ext = path.with_extension("md");
            if with_ext.exists() {
                path = with_ext;
            }
        }
        if !path.exists() {
            log_err!("link target does not exist: {}", path.display());
            self.toast(ctx, "File not found");
            return;
        }
        if path.is_file() && is_markdown(&path) {
            if new_window {
                shared.request_window(ctx, path, anchor);
            } else {
                self.navigate(shared, &path, anchor);
            }
        } else {
            // Never launch arbitrary files from a document; show them in Finder instead.
            macos::reveal(&path);
        }
    }

    fn navigate(&mut self, shared: &Shared, path: &Path, anchor: Option<String>) {
        if let Some(current) = self.path.clone() {
            self.back.push(HistoryEntry { path: current, position: self.anchor() });
            self.forward.clear();
        }
        self.open(path, anchor, shared);
    }

    fn go(&mut self, shared: &Shared, back: bool) {
        let position = self.anchor();
        let (from, to) = if back { (&mut self.back, &mut self.forward) } else { (&mut self.forward, &mut self.back) };
        let Some(entry) = from.pop() else {
            return;
        };
        if let Some(current) = self.path.clone() {
            to.push(HistoryEntry { path: current, position });
        }
        self.open(&entry.path, None, shared);
        self.restore = Some(entry.position);
    }

    fn dropped_files(&mut self, ctx: &egui::Context, shared: &Arc<Shared>) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        let mut first = true;
        for path in dropped {
            if path.is_dir() {
                log_err!("cannot open a folder: {}", path.display());
                continue;
            }
            if std::mem::take(&mut first) {
                self.back.clear();
                self.forward.clear();
                self.open(&path, None, shared);
            } else {
                shared.request_window(ctx, path, None);
            }
        }
    }

    fn toast(&mut self, ctx: &egui::Context, text: &str) {
        self.toast = Some((text.to_owned(), ctx.input(|i| i.time)));
        ctx.request_repaint();
    }

    // --- keyboard ----------------------------------------------------------

    fn keys(&mut self, ctx: &egui::Context, shared: &Arc<Shared>) {
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));

        if pressed(cmd, Key::W) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if pressed(cmd, Key::M) {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        if pressed(cmd, Key::O) {
            let mut first = self.is_empty();
            for path in macos::open_panel() {
                if std::mem::take(&mut first) {
                    self.open(&path, None, shared);
                } else {
                    shared.request_window(ctx, path, None);
                }
            }
        }
        if pressed(cmd, Key::R) && self.path.is_some() {
            self.read(true);
        }
        if pressed(cmd, Key::T) {
            self.toc_hidden = !self.toc_hidden;
        }
        if pressed(cmd, Key::OpenBracket) {
            self.go(shared, true);
        }
        if pressed(cmd, Key::CloseBracket) {
            self.go(shared, false);
        }

        // Zoom: ⌘+ / ⌘- / ⌘0 and pinch.
        if pressed(cmd, Key::Plus) || pressed(cmd, Key::Equals) || pressed(cmd_shift, Key::Equals) {
            self.set_zoom(self.zoom * 1.1);
        }
        if pressed(cmd, Key::Minus) {
            self.set_zoom(self.zoom / 1.1);
        }
        if pressed(cmd, Key::Num0) {
            self.set_zoom(1.0);
        }
        let pinch = ctx.input(|i| i.zoom_delta());
        if (pinch - 1.0).abs() > 0.001 {
            self.zoom = (self.zoom * pinch).clamp(ZOOM_MIN, ZOOM_MAX);
        }

        if self.doc.is_none() {
            return;
        }

        // Find.
        if pressed(cmd, Key::F) {
            self.find.open = true;
            self.find.focus = true;
            if let Some(text) = self.selection_text().filter(|t| !t.contains('\n') && t.len() < 200) {
                self.find.query = text;
                self.find.dirty = true;
            }
        }
        if pressed(cmd_shift, Key::G) {
            self.step_match(-1);
        } else if pressed(cmd, Key::G) {
            self.step_match(1);
        }
        if pressed(Modifiers::NONE, Key::Escape) {
            if self.find.open {
                self.find.open = false;
            } else {
                self.sel = None;
            }
        }

        // Copy / select all go to the document unless a text field has focus.
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing {
            if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
                && let Some(text) = self.selection_text()
            {
                ctx.copy_text(text);
            }
            if pressed(cmd, Key::A)
                && let Some(doc) = &self.doc
                && !doc.blocks.is_empty()
            {
                self.sel = Some(Selection {
                    anchor: Cur { block: 0, piece: 0, ch: 0 },
                    head: Cur { block: doc.blocks.len() - 1, piece: usize::MAX, ch: usize::MAX },
                });
            }

            let page = (self.view_h - self.titlebar) * 0.88;
            let line = 64.0;
            let shift = ctx.input(|i| i.modifiers.shift);
            if pressed(cmd, Key::ArrowUp) || pressed(Modifiers::NONE, Key::Home) {
                self.scroll_to(ctx, Target::Y(0.0));
            }
            if pressed(cmd, Key::ArrowDown) || pressed(Modifiers::NONE, Key::End) {
                self.scroll_to(ctx, Target::Y(f32::MAX));
            }
            if pressed(Modifiers::NONE, Key::PageDown) || (!shift && pressed(Modifiers::NONE, Key::Space)) {
                self.scroll_by(ctx, page);
            }
            if pressed(Modifiers::NONE, Key::PageUp) || pressed(Modifiers::SHIFT, Key::Space) {
                self.scroll_by(ctx, -page);
            }
            if pressed(Modifiers::NONE, Key::ArrowDown) || pressed(Modifiers::NONE, Key::J) {
                self.scroll_by(ctx, line);
            }
            if pressed(Modifiers::NONE, Key::ArrowUp) || pressed(Modifiers::NONE, Key::K) {
                self.scroll_by(ctx, -line);
            }
        }
    }

    // --- selection ---------------------------------------------------------

    fn selection_text(&self) -> Option<String> {
        let sel = self.sel.filter(|s| !s.is_empty())?;
        let doc = self.doc.as_ref()?;
        let (lo, hi) = sel.range();
        let mut out = String::new();
        for b in lo.block..=hi.block.min(doc.blocks.len().saturating_sub(1)) {
            let block = &doc.blocks[b];
            let cols = match &block.kind {
                Kind::Table { cols, .. } => *cols,
                _ => 0,
            };
            let mut any = false;
            for (p, piece) in block.pieces.iter().enumerate() {
                if (b, p) < (lo.block, lo.piece) || (b, p) > (hi.block, hi.piece) {
                    continue;
                }
                let start = if (b, p) == (lo.block, lo.piece) { lo.ch } else { 0 };
                let end = if (b, p) == (hi.block, hi.piece) { hi.ch } else { usize::MAX };
                let text = char_slice(&piece.text, start, end);
                if any {
                    out.push(if cols > 0 && p % cols != 0 { '\t' } else { '\n' });
                }
                out.push_str(text);
                any = true;
            }
            if b < hi.block && any {
                let tight = matches!(block.kind, Kind::Code { last: false, .. })
                    || (block.marker != doc::Marker::None
                        && doc.blocks.get(b + 1).is_some_and(|n| n.marker != doc::Marker::None));
                out.push_str(if tight { "\n" } else { "\n\n" });
            }
        }
        (!out.is_empty()).then_some(out)
    }

    // --- find --------------------------------------------------------------

    fn search(&mut self, doc: &Doc) {
        self.find.dirty = false;
        self.find.matches.clear();
        let query = self.find.query.to_lowercase();
        if query.is_empty() || !self.find.open {
            return;
        }
        let needle: Vec<char> = query.chars().collect();
        'blocks: for (b, block) in doc.blocks.iter().enumerate() {
            for (p, piece) in block.pieces.iter().enumerate() {
                if piece.text.len() < query.len() {
                    continue;
                }
                if piece.text.is_ascii() && query.is_ascii() {
                    // Bytes are characters: search without building a char vector.
                    let hay = piece.text.to_ascii_lowercase();
                    let mut from = 0;
                    while let Some(pos) = hay[from..].find(&query) {
                        let start = from + pos;
                        self.find.matches.push(Match {
                            block: b as u32,
                            piece: p as u32,
                            start: start as u32,
                            end: (start + query.len()) as u32,
                        });
                        from = start + query.len();
                    }
                } else {
                    let hay: Vec<char> = piece.text.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect();
                    let mut i = 0;
                    while i + needle.len() <= hay.len() {
                        if hay[i..i + needle.len()] == needle[..] {
                            self.find.matches.push(Match {
                                block: b as u32,
                                piece: p as u32,
                                start: i as u32,
                                end: (i + needle.len()) as u32,
                            });
                            i += needle.len();
                        } else {
                            i += 1;
                        }
                    }
                }
                if self.find.matches.len() >= 20_000 {
                    break 'blocks;
                }
            }
        }
        // Start from the first match at or below the top of the viewport.
        let (top, _) = self.anchor();
        self.find.current = self
            .find
            .matches
            .partition_point(|m| (m.block as usize) < top)
            .min(self.find.matches.len().saturating_sub(1));
        self.find.jump = !self.find.matches.is_empty();
    }

    fn step_match(&mut self, dir: i32) {
        if !self.find.open {
            self.find.open = true;
            self.find.dirty = true;
            return;
        }
        let n = self.find.matches.len();
        if n == 0 {
            return;
        }
        self.find.current = (self.find.current as i64 + dir as i64).rem_euclid(n as i64) as usize;
        self.find.jump = true;
    }

    fn jump_to_match(&mut self, ctx: &egui::Context, doc: &Doc, env: &Env<'_>) {
        let Some(m) = self.find.matches.get(self.find.current).copied() else {
            return;
        };
        let b = m.block as usize;
        if b >= self.slots.len() {
            return;
        }
        // Lay the block out to find where in it the match sits.
        let laid = layout::layout_block(&doc.blocks[b], env);
        let y_in_block = laid
            .texts
            .iter()
            .find(|t| t.piece == m.piece)
            .and_then(|t| {
                range_rects(&t.galley, m.start as usize..m.end as usize).first().map(|r| r.center().y + t.pos.y)
            })
            .unwrap_or(0.0);
        let y = self.tops[b] + y_in_block + self.pad_top();
        let visible = self.scroll_y + self.titlebar + 60.0..self.scroll_y + self.view_h - 80.0;
        if !visible.contains(&y) {
            self.scroll_to(ctx, Target::Block { idx: b, y: y_in_block, screen_y: self.view_h * 0.38 });
        }
        ctx.request_repaint();
    }

    fn find_bar(&mut self, ctx: &egui::Context, theme: &Theme, screen: Rect) {
        if !self.find.open {
            return;
        }
        let width = 320.0f32.min(screen.width() - 24.0);
        let pos = pos2(screen.right() - width - 14.0, screen.top() + self.titlebar + 8.0);
        let edit_id = Id::unique(("find-edit", self.id));
        let mut step = 0;
        let mut close = false;
        egui::Area::new(Id::unique(("find", self.id))).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme.bg)
                .stroke(Stroke::new(1.0, theme.border))
                .corner_radius(10)
                .inner_margin(egui::Margin { left: 10, right: 6, top: 5, bottom: 5 })
                .shadow(egui::Shadow {
                    offset: [0, 6],
                    blur: 22,
                    spread: 0,
                    color: Color32::from_black_alpha(if theme.dark { 110 } else { 32 }),
                })
                .show(ui, |ui| {
                    ui.set_width(width - 18.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let (icon, _) = ui.allocate_exact_size(vec2(16.0, 24.0), Sense::hover());
                        search_icon(ui.painter(), icon.center(), theme.faint);

                        let count = if self.find.query.is_empty() {
                            String::new()
                        } else if self.find.matches.is_empty() {
                            "No results".to_owned()
                        } else {
                            format!("{} of {}", self.find.current + 1, self.find.matches.len())
                        };
                        let count_galley = ui.ctx().fonts_mut(|f| {
                            f.layout_job(LayoutJob::single_section(count, theme::ui_format(12.0, 450.0, theme.muted)))
                        });
                        let buttons = 3.0 * 26.0;
                        let field = (ui.available_width() - buttons - count_galley.size().x - 16.0).max(60.0);

                        let edit = egui::TextEdit::singleline(&mut self.find.query)
                            .id(edit_id)
                            .desired_width(field)
                            .frame(egui::Frame::NONE)
                            .hint_text(egui::RichText::new("Find").color(theme.faint))
                            .font(egui::FontId::proportional(13.5))
                            .text_color(theme.strong);
                        let resp = ui.add(edit);
                        if std::mem::take(&mut self.find.focus) {
                            resp.request_focus();
                        }
                        if resp.changed() {
                            self.find.dirty = true;
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            step = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
                            resp.request_focus();
                        }

                        let (count_rect, _) =
                            ui.allocate_exact_size(vec2(count_galley.size().x + 6.0, 24.0), Sense::hover());
                        ui.painter().galley(
                            pos2(count_rect.left(), count_rect.center().y - count_galley.size().y / 2.0),
                            count_galley,
                            theme.muted,
                        );
                        if icon_button(ui, theme, |p, c, s| chevron(p, c, s, true)).clicked() {
                            step = -1;
                        }
                        if icon_button(ui, theme, |p, c, s| chevron(p, c, s, false)).clicked() {
                            step = 1;
                        }
                        if icon_button(ui, theme, cross).clicked() {
                            close = true;
                        }
                    });
                });
        });
        if step != 0 {
            self.step_match(step);
        }
        if close {
            self.find.open = false;
        }
    }

    // --- chrome ------------------------------------------------------------

    /// The strip under the transparent title bar: page-coloured, with a hairline that
    /// appears once content has scrolled beneath it.
    fn chrome(&mut self, ui: &mut egui::Ui, theme: &Theme, screen: Rect) {
        let bar = Rect::from_min_max(screen.min, pos2(screen.right(), screen.top() + self.titlebar));
        let p = ui.painter();
        p.rect_filled(bar, 0.0, theme.bg);
        let t = ui.ctx().animate_bool_with_time(
            Id::unique(("scrolled", self.id)),
            self.doc.is_some() && self.scroll_y > 2.0,
            0.12,
        );
        if t > 0.0 {
            let y = bar.bottom() - 0.5 / ui.ctx().pixels_per_point();
            p.hline(bar.x_range(), y, Stroke::new(1.0, theme.hairline.gamma_multiply(t)));
        }
    }

    fn placeholder(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let screen = ui.max_rect();
        let p = ui.painter();
        let center = screen.center();
        let (title, detail) = match (&self.error, &self.path) {
            (Some(err), Some(path)) => (
                format!(
                    "Can’t open {}",
                    path.file_name().map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
                ),
                err.clone(),
            ),
            _ => ("Drop a Markdown file here".to_owned(), "or press ⌘O to choose one".to_owned()),
        };
        // A quiet page glyph above the message.
        let page = Rect::from_center_size(center - vec2(0.0, 58.0), vec2(40.0, 50.0));
        p.rect_stroke(page, 7.0, Stroke::new(1.5, theme.border), StrokeKind::Inside);
        for (n, w) in [20.0, 24.0, 14.0].into_iter().enumerate() {
            let y = page.top() + 16.0 + n as f32 * 9.0;
            p.hline(
                page.left() + 8.0..=page.left() + 8.0 + w,
                y,
                Stroke::new(1.5, if self.error.is_some() { theme.border } else { theme.faint }),
            );
        }
        let text = |s: String, size: f32, weight: f32, color: Color32| {
            let mut job = LayoutJob::single_section(s, theme::ui_format(size, weight, color));
            job.halign = egui::Align::Center;
            job.wrap.max_width = (screen.width() - 80.0).max(120.0);
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };
        let title = text(title, 17.0, 600.0, theme.strong);
        let detail = text(detail, 13.5, 420.0, theme.muted);
        p.galley(pos2(center.x, center.y - 8.0), title.clone(), theme.strong);
        p.galley(pos2(center.x, center.y - 8.0 + title.size().y + 6.0), detail, theme.muted);
    }

    /// The table of contents in the left gutter.
    fn outline(&mut self, ui: &mut egui::Ui, theme: &Theme, doc: &Doc, screen: Rect, col_x: f32) {
        let ctx = ui.ctx().clone();
        let x0 = screen.left() + 28.0;
        let width = (col_x - 36.0 - x0).min(TOC_WIDTH);
        let top = screen.top() + self.pad_top() + 6.0;
        let bottom = screen.bottom() - 40.0;
        if width < 120.0 || bottom - top < 80.0 {
            return;
        }
        let min_level = doc.outline.iter().map(|h| h.level).min().unwrap_or(1);
        let entries: Vec<&doc::Heading> = doc.outline.iter().filter(|h| h.level <= min_level + 2).collect();
        if entries.is_empty() {
            return;
        }
        // The active entry is the last heading that has reached the top of the window.
        let reading_line = self.scroll_y - self.pad_top() + self.titlebar + 56.0;
        let active =
            entries.iter().rposition(|h| self.tops.get(h.block).is_some_and(|t| *t <= reading_line)).unwrap_or(0);

        let row = 27.0;
        let view = Rect::from_min_max(pos2(x0, top), pos2(x0 + width, bottom));
        let content = entries.len() as f32 * row;
        let target = (active as f32 * row - view.height() / 2.0 + row).clamp(0.0, (content - view.height()).max(0.0));
        let scroll = ctx.animate_value_with_time(Id::unique(("toc-scroll", self.id)), target, 0.2);

        let p = ui.painter().with_clip_rect(view.expand2(vec2(4.0, 0.0)));
        p.vline(
            x0 + 0.5,
            view.top().max(top - scroll)..=(top - scroll + content).min(view.bottom()),
            Stroke::new(1.0, theme.hairline),
        );
        let mut clicked = None;
        for (n, h) in entries.iter().enumerate() {
            let y = top + n as f32 * row - scroll;
            if y + row < view.top() || y > view.bottom() {
                continue;
            }
            let rect = Rect::from_min_size(pos2(x0, y), vec2(width, row));
            let resp = ui.interact(rect.intersect(view), Id::unique(("toc", self.id, n)), Sense::click());
            let is_active = n == active;
            let color = if is_active {
                theme.strong
            } else if resp.hovered() {
                theme.text
            } else {
                theme.muted
            };
            let indent = 14.0 + (h.level - min_level) as f32 * 12.0;
            let mut job = LayoutJob::single_section(
                h.title.clone(),
                theme::ui_format(12.5, if is_active { 560.0 } else { 430.0 }, color),
            );
            job.wrap.max_width = width - indent;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ctx.fonts_mut(|f| f.layout_job(job));
            p.galley(pos2(x0 + indent, y + (row - galley.size().y) / 2.0), galley, color);
            if is_active {
                p.rect_filled(Rect::from_min_size(pos2(x0 - 0.5, y + 5.0), vec2(2.0, row - 10.0)), 1.0, theme.accent);
            }
            if resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(h.block);
            }
        }
        if let Some(block) = clicked {
            self.scroll_to_block(&ctx, block);
        }
    }

    /// Hovered link target (bottom left) and transient messages (bottom centre).
    fn status(&mut self, ctx: &egui::Context, theme: &Theme, screen: Rect) {
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique(("status", self.id))));
        let pill = |text: &str, mono: bool, center: bool, alpha: f32| {
            let fmt = if mono {
                theme::mono_format(11.5, 400.0, theme.muted)
            } else {
                theme::ui_format(12.5, 520.0, theme.strong)
            };
            let mut job = LayoutJob::single_section(text.to_owned(), fmt);
            job.wrap.max_width = (screen.width() * 0.7).max(160.0);
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ctx.fonts_mut(|f| f.layout_job(job));
            let size = galley.size() + vec2(20.0, 10.0);
            let min = if center {
                pos2(screen.center().x - size.x / 2.0, screen.bottom() - size.y - 22.0)
            } else {
                pos2(screen.left() + 10.0, screen.bottom() - size.y - 10.0)
            };
            let rect = Rect::from_min_size(min, size);
            p.rect(
                rect,
                7.0,
                theme.surface.gamma_multiply(alpha),
                Stroke::new(1.0, theme.border.gamma_multiply(alpha)),
                StrokeKind::Inside,
            );
            p.galley_with_override_text_color(
                rect.min + vec2(10.0, 5.0),
                galley,
                if mono { theme.muted } else { theme.strong }.gamma_multiply(alpha),
            );
        };
        if let Some(url) = &self.hover_url {
            pill(url, true, false, 1.0);
        }
        if let Some((text, at)) = &self.toast {
            let age = (ctx.input(|i| i.time) - at) as f32;
            if age > 1.6 {
                self.toast = None;
            } else {
                pill(text, false, true, ((1.6 - age) / 0.3).clamp(0.0, 1.0));
                ctx.request_repaint();
            }
        }
    }

    fn drop_hint(&mut self, ctx: &egui::Context, theme: &Theme, screen: Rect) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique(("drop", self.id))));
        p.rect_filled(screen, 0.0, theme.bg.gamma_multiply(0.82));
        let inner = screen.shrink(14.0);
        p.rect_stroke(inner, 12.0, Stroke::new(2.0, theme.accent), StrokeKind::Inside);
        let galley = ctx.fonts_mut(|f| {
            f.layout_job(LayoutJob::single_section(
                "Drop to open".to_owned(),
                theme::ui_format(17.0, 600.0, theme.strong),
            ))
        });
        p.galley(screen.center() - galley.size() / 2.0, galley, theme.strong);
    }

    // --- development hooks -------------------------------------------------

    fn dev_hooks(&mut self, ctx: &egui::Context, shared: &Shared, frame: &Frame) {
        let dev = &shared.dev;
        if self.frames == 1 {
            crate::trace(shared.start, "first frame built");
            if let Some(query) = &dev.find {
                self.find.open = true;
                self.find.query = query.clone();
                self.find.dirty = true;
                ctx.request_repaint();
            }
        }
        if self.frames == 2 {
            // The second frame starts right after the first was presented.
            crate::trace(shared.start, "first frame on screen");
            if std::env::var_os("LENSBLITZ_TRACE").is_some() {
                let fill = ctx.fonts(|f| f.font_atlas_fill_ratio());
                crate::trace(shared.start, &format!("glyph atlas {:.0} % full", fill * 100.0));
            }
        }
        if let Some(n) = dev.exit_after_frames {
            if self.frames >= n {
                std::process::exit(0);
            }
            ctx.request_repaint();
        }
        if let Some(after) = dev.close_after {
            if shared.start.elapsed().as_secs_f32() > after + self.id as f32 && !self.dev_closed {
                self.dev_closed = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let measured = self.frames > 3 && self.measure_cursor >= self.slots.len();
        if measured && !self.dev_scrolled {
            self.dev_scrolled = true;
            if let Some(y) = dev.scroll {
                self.pending_scroll = Some(y);
                ctx.request_repaint();
            }
        }
        let Some(path) = &dev.screenshot else {
            return;
        };
        // Wait until layout has settled and images are in, then capture one frame.
        let settled = measured && self.dev_scrolled && !frame.loading_images && self.anim.is_none();
        let elapsed = shared.start.elapsed();
        if !self.shot_requested
            && ((settled && self.frames > 16 && elapsed > Duration::from_millis(500))
                || elapsed > Duration::from_secs(8))
        {
            self.shot_requested = true;
            let path = path.clone();
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                let [w, h] = image.size;
                match image::save_buffer(&path, image.as_raw(), w as u32, h as u32, image::ColorType::Rgba8) {
                    Ok(()) => eprintln!("lensblitz: wrote {} ({w}x{h})", path.display()),
                    Err(err) => log_err!("could not write {}: {err}", path.display()),
                }
                std::process::exit(0);
            })));
        }
        ctx.request_repaint();
    }
}

// ---------------------------------------------------------------------------
// Painting helpers

fn paint_deco(p: &egui::Painter, ov: Vec2, d: &Deco, theme: &Theme, zoom: f32) {
    match d {
        Deco::Rect { rect, clip, radius, fill, stroke } => {
            let r = rect.translate(ov);
            match clip {
                Some(clip) => {
                    p.with_clip_rect(clip.translate(ov).intersect(p.clip_rect())).rect(
                        r,
                        *radius,
                        *fill,
                        *stroke,
                        StrokeKind::Inside,
                    );
                }
                None => {
                    p.rect(r, *radius, *fill, *stroke, StrokeKind::Inside);
                }
            }
        }
        Deco::Line { a, b, stroke } => {
            p.line_segment([*a + ov, *b + ov], *stroke);
        }
        Deco::Circle { center, radius, fill, stroke } => {
            p.circle(*center + ov, *radius, *fill, *stroke);
        }
        Deco::Check { rect, checked } => {
            let r = rect.translate(ov);
            let radius = (r.width() * 0.26).round();
            if *checked {
                p.rect_filled(r, radius, theme.accent);
                check_icon(p, r.center(), r.width() * 0.27, Stroke::new((1.7 * zoom).max(1.4), theme.bg));
            } else {
                p.rect_stroke(r, radius, Stroke::new((1.4 * zoom).max(1.0), theme.faint), StrokeKind::Inside);
            }
        }
        Deco::Triangle { center, size, color } => {
            let c = *center + ov;
            let s = *size;
            p.add(egui::Shape::convex_polygon(
                vec![c + vec2(-s, -s * 0.55), c + vec2(s, -s * 0.55), c + vec2(0.0, s * 0.75)],
                *color,
                Stroke::NONE,
            ));
        }
    }
}

/// A horizontal fade from transparent to `color` (towards the right if `to_right`).
fn fade(p: &egui::Painter, rect: Rect, color: Color32, to_right: bool) {
    let clear = Color32::from_rgba_premultiplied(0, 0, 0, 0);
    let (l, r) = if to_right { (clear, color) } else { (color, clear) };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), l);
    mesh.colored_vertex(rect.right_top(), r);
    mesh.colored_vertex(rect.right_bottom(), r);
    mesh.colored_vertex(rect.left_bottom(), l);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(mesh));
}

fn check_icon(p: &egui::Painter, c: Pos2, s: f32, stroke: Stroke) {
    p.line(vec![c + vec2(-s, 0.05 * s), c + vec2(-0.3 * s, 0.75 * s), c + vec2(s, -0.7 * s)], stroke);
}

fn copy_icon(p: &egui::Painter, c: Pos2, color: Color32, bg: Color32) {
    let stroke = Stroke::new(1.3, color);
    let back = Rect::from_center_size(c + vec2(1.8, -1.8), vec2(9.0, 10.0));
    let front = Rect::from_center_size(c + vec2(-1.8, 1.8), vec2(9.0, 10.0));
    p.rect_stroke(back, 2.5, stroke, StrokeKind::Middle);
    p.rect(front, 2.5, bg, stroke, StrokeKind::Middle);
}

fn search_icon(p: &egui::Painter, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    p.circle_stroke(c + vec2(-1.5, -1.5), 4.5, stroke);
    p.line_segment([c + vec2(1.9, 1.9), c + vec2(5.5, 5.5)], stroke);
}

fn chevron(p: &egui::Painter, c: Pos2, stroke: Stroke, up: bool) {
    let d = if up { -1.0 } else { 1.0 };
    p.line(vec![c + vec2(-4.0, -2.0 * d), c + vec2(0.0, 2.0 * d), c + vec2(4.0, -2.0 * d)], stroke);
}

fn cross(p: &egui::Painter, c: Pos2, stroke: Stroke) {
    p.line_segment([c + vec2(-3.5, -3.5), c + vec2(3.5, 3.5)], stroke);
    p.line_segment([c + vec2(3.5, -3.5), c + vec2(-3.5, 3.5)], stroke);
}

fn icon_button(ui: &mut egui::Ui, theme: &Theme, draw: impl FnOnce(&egui::Painter, Pos2, Stroke)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, theme.surface_hi);
    }
    draw(ui.painter(), rect.center(), Stroke::new(1.5, if resp.hovered() { theme.strong } else { theme.muted }));
    resp
}

// ---------------------------------------------------------------------------
// Text helpers

/// `text[start..end]` with the bounds given in characters (clamped).
fn char_slice(text: &str, start: usize, end: usize) -> &str {
    let mut indices = text.char_indices().map(|(i, _)| i);
    let a = indices.by_ref().nth(start).unwrap_or(text.len());
    if end <= start {
        return "";
    }
    let b = if end == usize::MAX {
        text.len()
    } else {
        text[a..].char_indices().nth(end - start).map_or(text.len(), |(i, _)| a + i)
    };
    &text[a..b]
}

/// Character range of the word around character `at`.
fn word_at(text: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let at = at.min(chars.len() - 1);
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    if !is_word(chars[at]) {
        return (at, at + 1);
    }
    let mut a = at;
    while a > 0 && is_word(chars[a - 1]) {
        a -= 1;
    }
    let mut b = at + 1;
    while b < chars.len() && is_word(chars[b]) {
        b += 1;
    }
    (a, b)
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
    fn char_slicing() {
        assert_eq!(char_slice("héllo wörld", 1, 4), "éll");
        assert_eq!(char_slice("héllo", 2, usize::MAX), "llo");
        assert_eq!(char_slice("abc", 5, 9), "");
        assert_eq!(char_slice("abc", 2, 1), "");
    }

    #[test]
    fn words() {
        assert_eq!(word_at("foo bar_baz, qux", 5), (4, 11));
        assert_eq!(word_at("foo bar", 3), (3, 4));
        assert_eq!(word_at("", 0), (0, 0));
        assert_eq!(word_at("abc", 99), (0, 3));
    }

    fn window_with(src: &str) -> Win {
        let mut win = Win::new(1, ViewportId::ROOT);
        win.doc = Some(Arc::new(doc::parse(src)));
        win
    }

    #[test]
    fn copies_selection_across_blocks() {
        let mut win = window_with("# Title\n\nFirst paragraph.\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        // From the middle of the heading to the end of the document.
        win.sel = Some(Selection {
            anchor: Cur { block: 0, piece: 0, ch: 2 },
            head: Cur { block: 4, piece: usize::MAX, ch: usize::MAX },
        });
        assert_eq!(win.selection_text().as_deref(), Some("tle\n\nFirst paragraph.\n\none\ntwo\n\na\tb\n1\t2"));
        // Backwards within one paragraph.
        win.sel =
            Some(Selection { anchor: Cur { block: 1, piece: 0, ch: 15 }, head: Cur { block: 1, piece: 0, ch: 6 } });
        assert_eq!(win.selection_text().as_deref(), Some("paragraph"));
        win.sel =
            Some(Selection { anchor: Cur { block: 1, piece: 0, ch: 3 }, head: Cur { block: 1, piece: 0, ch: 3 } });
        assert_eq!(win.selection_text(), None);
    }

    #[test]
    fn find_is_case_insensitive_and_counts_characters() {
        let mut win = window_with("Über cool: the THE The\n\n```\nlet the = 1;\n```\n");
        win.tops = vec![0.0, 0.0];
        win.find.open = true;
        win.find.query = "The".to_owned();
        let doc = win.doc.clone().unwrap();
        win.search(&doc);
        let found: Vec<_> = win.find.matches.iter().map(|m| (m.block, m.start, m.end)).collect();
        assert_eq!(found, [(0, 11, 14), (0, 15, 18), (0, 19, 22), (1, 4, 7)]);
        win.find.query = "über".to_owned();
        win.search(&doc);
        assert_eq!(win.find.matches.len(), 1);
        assert_eq!((win.find.matches[0].start, win.find.matches[0].end), (0, 4));
    }

    #[test]
    fn selection_orders_its_ends() {
        let a = Cur { block: 2, piece: 0, ch: 5 };
        let b = Cur { block: 1, piece: 3, ch: 9 };
        assert_eq!(Selection { anchor: a, head: b }.range(), (b, a));
    }
}
