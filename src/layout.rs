//! Turns one document block into positioned galleys and shapes.
//!
//! Layout is per block and independent of every other block, so the viewer can lay out only
//! what is on screen, measure the rest in idle time, and drop galleys that scroll far away.
//! All coordinates are relative to the block's top-left corner (the column's left edge).

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use eframe::egui::epaint::text::{ByteIndex, LayoutSection, TextWrapping};
use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Align, Color32, CornerRadius, Galley, Pos2, Rect, Stroke, Vec2, pos2, vec2};

use crate::doc::{Block, Dim, Flow, HAlign, Kind, Marker, NO_LINK, Piece, Role, style};
use crate::images::Images;
use crate::theme::{self, BASE, CODE_LINE, CODE_SIZE, Theme};

pub struct Env<'a> {
    pub ctx: &'a egui::Context,
    pub theme: &'a Theme,
    pub zoom: f32,
    /// Pixels per point of the window being laid out.
    pub ppp: f32,
    /// Width of the reading column in points.
    pub width: f32,
    pub images: &'a Images,
    pub base_dir: Option<&'a Path>,
}

impl Env<'_> {
    fn em(&self) -> f32 {
        BASE * self.zoom
    }

    fn snap(&self, v: f32) -> f32 {
        (v * self.ppp).round() / self.ppp
    }

    fn layout(&self, job: LayoutJob) -> Arc<Galley> {
        self.ctx.fonts_mut(|f| f.layout_job(job))
    }
}

pub struct LaidText {
    /// Index into `Block::pieces`.
    pub piece: u32,
    /// Where the galley is painted. With centred or right-aligned text this is the
    /// alignment anchor, not the left edge.
    pub pos: Vec2,
    pub galley: Arc<Galley>,
}

impl LaidText {
    pub fn rect(&self) -> Rect {
        self.galley.rect.translate(self.pos)
    }
}

pub enum Deco {
    /// A filled and/or stroked rounded rectangle, optionally clipped (used to draw panels
    /// that continue into the next block).
    Rect {
        rect: Rect,
        clip: Option<Rect>,
        radius: CornerRadius,
        fill: Color32,
        stroke: Stroke,
    },
    Line {
        a: Pos2,
        b: Pos2,
        stroke: Stroke,
    },
    Circle {
        center: Pos2,
        radius: f32,
        fill: Color32,
        stroke: Stroke,
    },
    Check {
        rect: Rect,
        checked: bool,
    },
    /// A small downward-pointing triangle.
    Triangle {
        center: Pos2,
        size: f32,
        color: Color32,
    },
}

pub struct LaidImage {
    pub rect: Rect,
    /// Index into `Block::images`.
    pub image: u32,
    /// Cache key (absolute path or URL), or `None` if the source cannot be loaded.
    pub key: Option<String>,
    /// Whether the size was known at layout time.
    pub sized: bool,
}

/// A horizontally scrollable region (wide code or tables).
pub struct Wide {
    pub clip: Rect,
    pub content_width: f32,
}

#[derive(Default)]
pub struct Laid {
    pub height: f32,
    pub texts: Vec<LaidText>,
    /// Non-selectable text: list numbers, footnote labels, the code language tag.
    pub labels: Vec<(Vec2, Arc<Galley>)>,
    pub decos: Vec<Deco>,
    /// Shapes that scroll with `wide` content.
    pub wide_decos: Vec<Deco>,
    /// Shapes painted over everything else (panel outlines).
    pub over: Vec<Deco>,
    pub images: Vec<LaidImage>,
    /// Link hit boxes: `(rect, index into Block::links)`.
    pub links: Vec<(Rect, u32)>,
    pub wide: Option<Wide>,
    /// The code panel, for the hover copy button.
    pub panel: Option<Rect>,
    /// An image had no known size yet; lay out again once it loads.
    pub pending_images: bool,
}

/// Rectangles covering the characters `range` of `galley`, one per row, in galley space.
pub fn range_rects(galley: &Galley, range: Range<usize>) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut row_start = 0usize;
    for row in &galley.rows {
        let glyphs = row.glyphs.len();
        let row_end = row_start + glyphs;
        if range.start <= row_end && range.end > row_start {
            let mut a = range.start.saturating_sub(row_start).min(glyphs);
            let mut b = (range.end - row_start).min(glyphs);
            // Spaces at a wrap point are not part of the highlighted run.
            while b > a && row.glyphs[b - 1].chr.is_whitespace() && b == glyphs {
                b -= 1;
            }
            while a < b && row.glyphs[a].chr.is_whitespace() && a == 0 {
                a += 1;
            }
            let x = |col: usize| row.pos.x + row.glyphs.get(col).map_or(row.size.x, |g| g.pos.x);
            if b > a {
                out.push(Rect::from_min_max(pos2(x(a), row.min_y()), pos2(x(b), row.max_y())));
            }
        }
        row_start = row_end + row.ends_with_newline as usize;
        if row_start >= range.end {
            break;
        }
    }
    out
}

fn text_job(env: &Env<'_>, piece: &Piece, role: Role, wrap: f32, halign: HAlign) -> LayoutJob {
    let mut job = LayoutJob {
        text: piece.text.clone(),
        wrap: TextWrapping { max_width: wrap, ..Default::default() },
        halign: match halign {
            HAlign::Left => Align::LEFT,
            HAlign::Center => Align::Center,
            HAlign::Right => Align::RIGHT,
        },
        ..Default::default()
    };
    let mut start = 0usize;
    for span in &piece.spans {
        let end = (span.end as usize).min(piece.text.len());
        if end <= start {
            continue;
        }
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(start)..ByteIndex(end),
            format: theme::text_format(env.theme, env.zoom, env.ppp, role, span.style, span.link != NO_LINK),
        });
        start = end;
    }
    if start < piece.text.len() || job.sections.is_empty() {
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(start)..ByteIndex(piece.text.len()),
            format: theme::text_format(env.theme, env.zoom, env.ppp, role, 0, false),
        });
    }
    job
}

fn code_job(env: &Env<'_>, piece: &Piece) -> LayoutJob {
    let mut job = LayoutJob {
        text: piece.text.clone(),
        wrap: TextWrapping { max_width: f32::INFINITY, ..Default::default() },
        ..Default::default()
    };
    let mut start = 0usize;
    for span in &piece.spans {
        let end = (span.end as usize).min(piece.text.len());
        if end <= start {
            continue;
        }
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(start)..ByteIndex(end),
            format: theme::code_format(env.theme, env.zoom, env.ppp, span.style),
        });
        start = end;
    }
    if start < piece.text.len() || job.sections.is_empty() {
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(start)..ByteIndex(piece.text.len()),
            format: theme::code_format(env.theme, env.zoom, env.ppp, 0),
        });
    }
    job
}

/// A rough height for a block that has not been laid out yet (keeps the scroll bar honest).
pub fn estimate_height(block: &Block, env: &Env<'_>) -> f32 {
    let em = env.em();
    let x0 = block.indent as f32 / 100.0 * em;
    let avail = (env.width - x0).max(120.0);
    let frame = (block.gap + block.pad_top + block.pad_bottom) as f32 / 100.0 * em;
    let body = match &block.kind {
        Kind::Text(role) => {
            let st = theme::role_style(*role);
            let per_line = (avail / (st.size * env.zoom * 0.5)).max(8.0);
            let mut h = 0.0;
            for item in &block.flow {
                match item {
                    Flow::Text(i) => {
                        let text = &block.pieces[*i as usize].text;
                        let lines = (text.len() as f32 / per_line).ceil().max(1.0) + text.matches('\n').count() as f32;
                        h += lines * st.line * env.zoom;
                    }
                    Flow::Image(_) => h += 120.0 * env.zoom,
                    Flow::Break => {}
                }
            }
            h
        }
        Kind::Code { first, last, .. } => {
            let lines = block.pieces[0].text.matches('\n').count() + 1;
            let pad = 14.0 * env.zoom;
            lines as f32 * CODE_LINE * env.zoom + if *first { pad } else { 0.0 } + if *last { pad } else { 0.0 }
        }
        Kind::Table { cols, .. } => (block.pieces.len() / (*cols).max(1)) as f32 * 37.0 * env.zoom,
        Kind::Rule => 1.0,
    };
    frame + body
}

pub fn layout_block(block: &Block, env: &Env<'_>) -> Laid {
    let em = env.em();
    let x0 = env.snap(block.indent as f32 / 100.0 * em);
    let avail = (env.width - x0).max(120.0);
    let gap = env.snap(block.gap as f32 / 100.0 * em);
    let top = gap + env.snap(block.pad_top as f32 / 100.0 * em);
    let mut laid = Laid::default();

    let bottom = match &block.kind {
        Kind::Text(role) => text_block(block, *role, env, &mut laid, x0, avail, top),
        Kind::Code { lang, first, last } => code_block(block, lang, *first, *last, env, &mut laid, x0, avail, top),
        Kind::Table { cols, aligns } => table_block(block, *cols, aligns, env, &mut laid, x0, avail, top),
        Kind::Rule => {
            let y = env.snap(top) + 0.5 / env.ppp;
            laid.decos.push(Deco::Line {
                a: pos2(x0, y),
                b: pos2(x0 + avail, y),
                stroke: Stroke::new(1.0, env.theme.border),
            });
            top + 1.0
        }
    };
    laid.height = env.snap(bottom + block.pad_bottom as f32 / 100.0 * em);

    rails(block, env, &mut laid, gap);
    laid
}

/// Block quote bars and alert panels behind the block.
fn rails(block: &Block, env: &Env<'_>, laid: &mut Laid, gap: f32) {
    if block.rails.is_empty() {
        return;
    }
    let em = env.em();
    let bar = (3.0 * env.zoom).max(2.0);
    let mut behind = Vec::new();
    for rail in &block.rails {
        let x = env.snap(rail.x_em as f32 / 100.0 * em);
        let y0 = if rail.first { gap } else { 0.0 };
        let r = (6.0 * env.zoom).round() as u8;
        match rail.alert {
            Some(kind) => {
                let color = env.theme.alert_color(kind);
                let radius = CornerRadius {
                    nw: if rail.first { r } else { 0 },
                    ne: if rail.first { r } else { 0 },
                    sw: if rail.last { r } else { 0 },
                    se: if rail.last { r } else { 0 },
                };
                let rect = Rect::from_min_max(pos2(x, y0), pos2(env.width, laid.height));
                behind.push(Deco::Rect {
                    rect,
                    clip: None,
                    radius,
                    fill: theme::mix(env.theme.bg, color, if env.theme.dark { 0.10 } else { 0.065 }),
                    stroke: Stroke::NONE,
                });
                behind.push(Deco::Rect {
                    rect: Rect::from_min_max(rect.min, pos2(x + bar, laid.height)),
                    clip: None,
                    radius: CornerRadius { ne: 0, se: 0, ..radius },
                    fill: color,
                    stroke: Stroke::NONE,
                });
            }
            None => {
                let round = (bar / 2.0).round() as u8;
                behind.push(Deco::Rect {
                    rect: Rect::from_min_max(pos2(x, y0), pos2(x + bar, laid.height)),
                    clip: None,
                    radius: CornerRadius {
                        nw: if rail.first { round } else { 0 },
                        ne: if rail.first { round } else { 0 },
                        sw: if rail.last { round } else { 0 },
                        se: if rail.last { round } else { 0 },
                    },
                    fill: env.theme.quote_bar,
                    stroke: Stroke::NONE,
                });
            }
        }
    }
    behind.append(&mut laid.decos);
    laid.decos = behind;
}

fn anchor_x(halign: HAlign, x0: f32, avail: f32) -> f32 {
    match halign {
        HAlign::Left => x0,
        HAlign::Center => x0 + avail / 2.0,
        HAlign::Right => x0 + avail,
    }
}

/// Add link hit boxes and inline-code pills for a laid-out prose piece.
fn decorate_piece(env: &Env<'_>, piece: &Piece, galley: &Galley, pos: Vec2, laid: &mut Laid, wide: bool) {
    let mut char_pos = 0usize;
    let mut byte_pos = 0usize;
    // Consecutive spans of the same link (e.g. bold inside a link) share hit boxes anyway.
    for span in &piece.spans {
        let end = (span.end as usize).min(piece.text.len());
        if end <= byte_pos {
            continue;
        }
        let chars = piece.text[byte_pos..end].chars().count();
        let range = char_pos..char_pos + chars;
        if span.link != NO_LINK {
            for r in range_rects(galley, range.clone()) {
                laid.links.push((r.translate(pos), span.link));
            }
        }
        if span.style & (style::CODE | style::KBD) != 0 {
            let kbd = span.style & style::KBD != 0;
            let size =
                galley.job.sections.first().map_or(BASE, |s| s.format.font_id.size).max(CODE_SIZE * env.zoom * 0.8);
            let half = (size * 0.66).round();
            let pad = (3.0 * env.zoom).round();
            for r in range_rects(galley, range) {
                let cy = r.center().y;
                let rect = Rect::from_min_max(pos2(r.left() - pad, cy - half), pos2(r.right() + pad, cy + half))
                    .translate(pos);
                let deco = Deco::Rect {
                    rect,
                    clip: None,
                    radius: CornerRadius::same((4.0 * env.zoom).round() as u8),
                    fill: env.theme.surface_hi,
                    stroke: if kbd { Stroke::new(1.0, env.theme.border) } else { Stroke::NONE },
                };
                if wide { laid.wide_decos.push(deco) } else { laid.decos.push(deco) }
            }
        }
        char_pos += chars;
        byte_pos = end;
    }
}

fn marker(block: &Block, role: Role, env: &Env<'_>, laid: &mut Laid, x0: f32, top: f32, first_row_h: f32) {
    let em = env.em();
    let cy = top + first_row_h / 2.0;
    let t = env.theme;
    match &block.marker {
        Marker::None => {}
        Marker::Bullet(depth) => {
            let center = pos2(x0 - 0.92 * em, cy);
            let r = 0.155 * em;
            match depth % 3 {
                0 => laid.decos.push(Deco::Circle { center, radius: r, fill: t.faint, stroke: Stroke::NONE }),
                1 => laid.decos.push(Deco::Circle {
                    center,
                    radius: r,
                    fill: Color32::TRANSPARENT,
                    stroke: Stroke::new((1.3 * env.zoom).max(1.0), t.faint),
                }),
                _ => laid.decos.push(Deco::Rect {
                    rect: Rect::from_center_size(center, Vec2::splat(r * 1.7)),
                    clip: None,
                    radius: CornerRadius::same(1),
                    fill: t.faint,
                    stroke: Stroke::NONE,
                }),
            }
        }
        Marker::Number(n) | Marker::Footnote(n) => {
            let footnote = matches!(block.marker, Marker::Footnote(_));
            let st = theme::role_style(if footnote { Role::Footnote } else { role });
            let mut fmt = theme::ui_format(st.size * env.zoom * if footnote { 0.9 } else { 1.0 }, 450.0, t.muted);
            if footnote {
                fmt.color = t.link;
            }
            let galley = env.ctx.fonts_mut(|f| f.layout_job(LayoutJob::single_section(n.clone(), fmt)));
            let x = x0 - 0.5 * em - galley.size().x;
            let y = cy - galley.size().y / 2.0;
            laid.labels.push((vec2(x, env.snap(y)), galley));
        }
        Marker::Task(checked) => {
            let side = env.snap(0.95 * em);
            let rect = Rect::from_center_size(pos2(x0 - 0.95 * em, cy), Vec2::splat(side));
            laid.decos.push(Deco::Check { rect, checked: *checked });
        }
        Marker::Disclosure => {
            laid.decos.push(Deco::Triangle { center: pos2(x0 - 0.75 * em, cy), size: 0.3 * em, color: t.muted });
        }
    }
}

/// The on-screen size of an image, given its natural size (if known) and HTML attributes.
fn image_size(env: &Env<'_>, natural: Option<[f32; 2]>, w: Option<Dim>, h: Option<Dim>, avail: f32) -> (Vec2, bool) {
    let px = |d: Dim| match d {
        Dim::Px(v) => v as f32 * env.zoom,
        Dim::Percent(v) => avail * v as f32 / 100.0,
    };
    let aspect = natural.filter(|n| n[0] > 0.0 && n[1] > 0.0).map(|n| n[1] / n[0]);
    let mut size = match (w.map(px), h.map(px), natural) {
        (Some(w), Some(h), _) => vec2(w, aspect.map_or(h, |a| w * a)),
        (Some(w), None, _) => vec2(w, aspect.map_or(w * 0.5, |a| w * a)),
        (None, Some(h), _) => vec2(aspect.map_or(h * 2.0, |a| h / a), h),
        (None, None, Some(n)) => vec2(n[0], n[1]) * env.zoom,
        (None, None, None) => vec2(avail.min(320.0 * env.zoom), 180.0 * env.zoom),
    };
    if size.x > avail {
        size *= avail / size.x;
    }
    (vec2(env.snap(size.x.max(1.0)), env.snap(size.y.max(1.0))), aspect.is_some() || (w.is_some() && h.is_some()))
}

fn text_block(block: &Block, role: Role, env: &Env<'_>, laid: &mut Laid, x0: f32, avail: f32, top: f32) -> f32 {
    let em = env.em();
    let mut y = top;
    let mut first_row_h: Option<f32> = None;
    let mut prev_was_image = false;
    let mut i = 0;
    while i < block.flow.len() {
        match block.flow[i] {
            Flow::Text(p) => {
                if prev_was_image {
                    y += 0.5 * em;
                }
                let piece = &block.pieces[p as usize];
                let galley = env.layout(text_job(env, piece, role, avail, block.align));
                let pos = vec2(anchor_x(block.align, x0, avail), env.snap(y));
                first_row_h.get_or_insert_with(|| galley.rows.first().map_or(0.0, |r| r.height()));
                decorate_piece(env, piece, &galley, pos, laid, false);
                y += galley.size().y;
                laid.texts.push(LaidText { piece: p, pos, galley });
                prev_was_image = false;
                i += 1;
            }
            Flow::Break => i += 1,
            Flow::Image(_) => {
                if i > 0 && !prev_was_image {
                    y += 0.5 * em;
                }
                // Lay consecutive images out as wrapping rows (badges, logo strips).
                let gap = env.snap(6.0 * env.zoom);
                let mut row: Vec<(u32, Vec2, Option<String>, bool)> = Vec::new();
                let mut row_w = 0.0;
                let flush =
                    |row: &mut Vec<(u32, Vec2, Option<String>, bool)>, row_w: f32, y: &mut f32, laid: &mut Laid| {
                        if row.is_empty() {
                            return;
                        }
                        let row_h = row.iter().map(|r| r.1.y).fold(0.0, f32::max);
                        let mut x = match block.align {
                            HAlign::Left => x0,
                            HAlign::Center => x0 + (avail - row_w) / 2.0,
                            HAlign::Right => x0 + avail - row_w,
                        };
                        for (image, size, key, sized) in row.drain(..) {
                            let rect =
                                Rect::from_min_size(pos2(env.snap(x), env.snap(*y + (row_h - size.y) / 2.0)), size);
                            laid.images.push(LaidImage { rect, image, key, sized });
                            x += size.x + gap;
                        }
                        *y += row_h + gap;
                    };
                while i < block.flow.len() {
                    match block.flow[i] {
                        Flow::Image(idx) => {
                            let img = &block.images[idx as usize];
                            let key = Images::key(env.base_dir, &img.src);
                            let natural = key.as_deref().and_then(|k| env.images.natural_size(k));
                            let (size, sized) = image_size(env, natural, img.width, img.height, avail);
                            if !sized && key.is_some() {
                                laid.pending_images = true;
                            }
                            if !row.is_empty() && row_w + gap + size.x > avail + 0.5 {
                                flush(&mut row, row_w, &mut y, laid);
                                row_w = 0.0;
                            }
                            row_w += if row.is_empty() { size.x } else { gap + size.x };
                            row.push((idx, size, key, sized));
                        }
                        Flow::Break => {
                            flush(&mut row, row_w, &mut y, laid);
                            row_w = 0.0;
                        }
                        Flow::Text(_) => break,
                    }
                    i += 1;
                }
                flush(&mut row, row_w, &mut y, laid);
                y -= gap;
                first_row_h.get_or_insert(theme::role_style(Role::Body).line * env.zoom);
                prev_was_image = true;
            }
        }
    }
    marker(block, role, env, laid, x0, top, first_row_h.unwrap_or(26.0 * env.zoom));
    y
}

#[allow(clippy::too_many_arguments)]
fn code_block(
    block: &Block,
    lang: &str,
    first: bool,
    last: bool,
    env: &Env<'_>,
    laid: &mut Laid,
    x0: f32,
    avail: f32,
    top: f32,
) -> f32 {
    let t = env.theme;
    let pad_x = env.snap(16.0 * env.zoom);
    let pad_y = env.snap(14.0 * env.zoom);
    let galley = env.layout(code_job(env, &block.pieces[0]));
    let text_top = top + if first { pad_y } else { 0.0 };
    let bottom = text_top + galley.size().y + if last { pad_y } else { 0.0 };

    let r = (9.0 * env.zoom).round() as u8;
    let panel = Rect::from_min_max(pos2(x0, top), pos2(x0 + avail, bottom));
    // Extend past the open edges so the stroke and rounding only appear at the real ends.
    let mut drawn = panel;
    if !first {
        drawn.min.y -= 24.0;
    }
    if !last {
        drawn.max.y += 24.0;
    }
    laid.decos.push(Deco::Rect {
        rect: drawn,
        clip: Some(panel),
        radius: CornerRadius::same(r),
        fill: t.surface,
        stroke: Stroke::NONE,
    });
    // The outline goes on top so scrolled code and edge fades never cut through it.
    laid.over.push(Deco::Rect {
        rect: drawn,
        clip: Some(panel),
        radius: CornerRadius::same(r),
        fill: Color32::TRANSPARENT,
        stroke: Stroke::new(1.0, t.border),
    });
    laid.panel = Some(panel);

    let content_width = galley.size().x + 2.0 * pad_x;
    let first_row_w = galley.rows.first().map_or(0.0, |r| r.size.x);
    laid.wide = Some(Wide { clip: panel.shrink(1.0), content_width });
    laid.texts.push(LaidText { piece: 0, pos: vec2(x0 + pad_x, env.snap(text_top)), galley });

    if first && !lang.is_empty() && lang.len() <= 20 {
        let fmt = theme::mono_format(10.5 * env.zoom, 500.0, t.faint);
        let label = env.ctx.fonts_mut(|f| f.layout_job(LayoutJob::single_section(lang.to_ascii_lowercase(), fmt)));
        let pos = vec2(x0 + avail - pad_x * 0.75 - label.size().x, top + env.snap(7.0 * env.zoom));
        // Only where it cannot collide with the first line of code.
        if x0 + pad_x + first_row_w + 16.0 * env.zoom < x0 + pos.x - x0 {
            laid.labels.push((pos, label));
        }
    }
    bottom
}

#[allow(clippy::too_many_arguments)]
fn table_block(
    block: &Block,
    cols: usize,
    aligns: &[HAlign],
    env: &Env<'_>,
    laid: &mut Laid,
    x0: f32,
    avail: f32,
    top: f32,
) -> f32 {
    let t = env.theme;
    let cols = cols.max(1);
    let rows = block.pieces.len() / cols;
    let pad_x = env.snap(12.0 * env.zoom);
    let pad_y = env.snap(7.0 * env.zoom);
    let role = |cell: usize| if cell < cols { Role::HeadCell } else { Role::Cell };
    let align = |col: usize| aligns.get(col).copied().unwrap_or_default();

    // Pass 1: natural (unwrapped) widths.
    let mut galleys: Vec<Arc<Galley>> = block
        .pieces
        .iter()
        .enumerate()
        .map(|(i, piece)| env.layout(text_job(env, piece, role(i), f32::INFINITY, align(i % cols))))
        .collect();
    let mut widths = vec![0.0f32; cols];
    for (i, g) in galleys.iter().enumerate() {
        widths[i % cols] = widths[i % cols].max(g.size().x.ceil());
    }

    // Shrink the widest columns until the table fits, but never below a readable minimum.
    let budget = avail - cols as f32 * 2.0 * pad_x;
    let min_col = (96.0 * env.zoom).min(budget / cols as f32).max(48.0 * env.zoom);
    if widths.iter().sum::<f32>() > budget {
        let mut order: Vec<usize> = (0..cols).collect();
        order.sort_by(|a, b| widths[*a].total_cmp(&widths[*b]));
        let mut remaining = budget;
        for (n, &col) in order.iter().enumerate() {
            let share = (remaining / (cols - n) as f32).max(min_col);
            widths[col] = widths[col].min(share);
            remaining -= widths[col];
        }
        // Pass 2: wrap the cells that no longer fit.
        for (i, piece) in block.pieces.iter().enumerate() {
            if galleys[i].size().x > widths[i % cols] + 0.5 {
                galleys[i] = env.layout(text_job(env, piece, role(i), widths[i % cols], align(i % cols)));
            }
        }
    }

    let col_w: Vec<f32> = widths.iter().map(|w| env.snap(w + 2.0 * pad_x)).collect();
    let table_w: f32 = col_w.iter().sum();
    let mut y = top;
    let hair = 1.0 / env.ppp;
    for row in 0..rows {
        let row_h = (0..cols).map(|c| galleys[row * cols + c].size().y).fold(0.0, f32::max) + 2.0 * pad_y;
        let row_h = env.snap(row_h);
        if row == 0 {
            laid.wide_decos.push(Deco::Rect {
                rect: Rect::from_min_size(pos2(x0, y), vec2(table_w, row_h)),
                clip: None,
                radius: CornerRadius::same((6.0 * env.zoom).round() as u8),
                fill: t.surface,
                stroke: Stroke::NONE,
            });
        } else if row > 1 {
            laid.wide_decos.push(Deco::Line {
                a: pos2(x0, y + hair / 2.0),
                b: pos2(x0 + table_w, y + hair / 2.0),
                stroke: Stroke::new(1.0, t.hairline),
            });
        }
        let mut x = x0;
        for (col, width) in col_w.iter().enumerate() {
            let i = row * cols + col;
            let galley = galleys[i].clone();
            let pos = vec2(anchor_x(align(col), x + pad_x, width - 2.0 * pad_x), env.snap(y + pad_y));
            decorate_piece(env, &block.pieces[i], &galley, pos, laid, true);
            laid.texts.push(LaidText { piece: i as u32, pos, galley });
            x += width;
        }
        y += row_h;
    }
    laid.wide = Some(Wide { clip: Rect::from_min_max(pos2(x0, top), pos2(x0 + avail, y)), content_width: table_w });
    y
}
