//! Colours, type scale and fonts. Everything visual that is not layout lives here.

use std::sync::Arc;

use eframe::egui::epaint::text::{FontTweak, VariationCoords};
use eframe::egui::{self, Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextFormat};

use crate::doc::{AlertKind, Role, style};

pub const INTER: &[u8] = include_bytes!("../assets/fonts/Inter.ttf");
pub const INTER_ITALIC: &[u8] = include_bytes!("../assets/fonts/Inter-Italic.ttf");
pub const MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono.ttf");

pub const ITALIC_FAMILY: &str = "italic";

/// Reading column width in points at 100 % zoom.
pub const COLUMN: f32 = 744.0;
/// Body size in points at 100 % zoom.
pub const BASE: f32 = 16.0;

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// `fg` at `alpha` over `bg`, as an opaque colour.
pub fn mix(bg: Color32, fg: Color32, alpha: f32) -> Color32 {
    let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * alpha).round() as u8;
    Color32::from_rgb(l(bg.r(), fg.r()), l(bg.g(), fg.g()), l(bg.b(), fg.b()))
}

pub struct Theme {
    pub dark: bool,
    pub bg: Color32,
    /// Code panels and table headers.
    pub surface: Color32,
    /// Inline code pills, hovered controls.
    pub surface_hi: Color32,
    pub hairline: Color32,
    pub border: Color32,
    pub text: Color32,
    pub strong: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub link: Color32,
    pub selection: Color32,
    pub find: Color32,
    pub find_active: Color32,
    pub mark: Color32,
    pub quote_bar: Color32,
    /// Indexed by `highlight::tok`.
    pub syn: [Color32; 16],
    /// Indexed by `AlertKind as usize`.
    pub alert: [Color32; 5],
}

impl Theme {
    pub fn get(dark: bool) -> &'static Theme {
        if dark { &DARK } else { &LIGHT }
    }

    pub fn alert_color(&self, kind: AlertKind) -> Color32 {
        self.alert[kind as usize]
    }
}

static LIGHT: Theme = Theme {
    dark: false,
    bg: rgb(0xFFFFFF),
    surface: rgb(0xF6F7F9),
    surface_hi: rgb(0xEEF0F4),
    hairline: rgb(0xECEEF2),
    border: rgb(0xE1E4EA),
    text: rgb(0x242933),
    strong: rgb(0x0D1017),
    muted: rgb(0x5F6B7A),
    faint: rgb(0x9AA3B2),
    accent: rgb(0x2F5BFF),
    link: rgb(0x2450E6),
    selection: rgb(0xCFDAFF),
    find: rgb(0xFFE9A6),
    find_active: rgb(0xFFB938),
    mark: rgb(0xFFF1B8),
    quote_bar: rgb(0xD6DBE3),
    syn: [
        rgb(0x242933), // plain
        rgb(0x8B95A5), // comment
        rgb(0x0F7B5F), // string
        rgb(0xB5500D), // number
        rgb(0x7B3FE4), // keyword
        rgb(0x0B7285), // type
        rgb(0x1F56D8), // function
        rgb(0xB5500D), // constant
        rgb(0x5A6472), // punctuation
        rgb(0x9C4A00), // attribute
        rgb(0x214FC6), // property / key
        rgb(0xC0245C), // tag
        rgb(0xA8480C), // variable
        rgb(0x1A7F4B), // inserted
        rgb(0xCF3A3A), // deleted
        rgb(0x8B95A5), // meta
    ],
    alert: [rgb(0x2F6FEB), rgb(0x1A7F4B), rgb(0x7C4DDB), rgb(0x9A6700), rgb(0xCF3A3A)],
};

static DARK: Theme = Theme {
    dark: true,
    bg: rgb(0x0D0F13),
    surface: rgb(0x14171D),
    surface_hi: rgb(0x1C2029),
    hairline: rgb(0x1F232B),
    border: rgb(0x2A2F3A),
    text: rgb(0xC9CED8),
    strong: rgb(0xF2F4F8),
    muted: rgb(0x8A93A3),
    faint: rgb(0x5A6373),
    accent: rgb(0x7B93FF),
    link: rgb(0x8CA2FF),
    selection: rgb(0x2A3768),
    find: rgb(0x5C4B10),
    find_active: rgb(0xA87A0A),
    mark: rgb(0x4F4208),
    quote_bar: rgb(0x343A46),
    syn: [
        rgb(0xC9CED8), // plain
        rgb(0x5E6878), // comment
        rgb(0x7FD8B0), // string
        rgb(0xF2A869), // number
        rgb(0xC3A6FF), // keyword
        rgb(0x6CCFE4), // type
        rgb(0x82AAFF), // function
        rgb(0xF2A869), // constant
        rgb(0x8F99A8), // punctuation
        rgb(0xE8C07D), // attribute
        rgb(0x82AAFF), // property / key
        rgb(0xFF8FA8), // tag
        rgb(0xF2B880), // variable
        rgb(0x6DD49C), // inserted
        rgb(0xFF8080), // deleted
        rgb(0x5E6878), // meta
    ],
    alert: [rgb(0x5B9BFF), rgb(0x3FB97A), rgb(0xA98BFF), rgb(0xD9A52B), rgb(0xF26D6D)],
};

// ---------------------------------------------------------------------------
// Type scale

/// Size, line height (both in points at 100 %), weight and tracking (em) of a text role.
#[derive(Clone, Copy)]
pub struct TextStyle {
    pub size: f32,
    pub line: f32,
    pub weight: f32,
    pub tracking: f32,
}

pub const CODE_SIZE: f32 = 13.5;
pub const CODE_LINE: f32 = 21.0;

pub fn role_style(role: Role) -> TextStyle {
    let s = |size, line, weight, tracking| TextStyle { size, line, weight, tracking };
    match role {
        Role::Body => s(BASE, 26.0, 400.0, 0.0),
        Role::Heading(1) => s(32.0, 40.0, 700.0, -0.021),
        Role::Heading(2) => s(24.0, 32.0, 650.0, -0.017),
        Role::Heading(3) => s(19.5, 28.0, 620.0, -0.012),
        Role::Heading(4) => s(17.0, 26.0, 620.0, -0.006),
        Role::Heading(5) => s(15.0, 24.0, 620.0, 0.0),
        Role::Heading(_) => s(13.5, 22.0, 620.0, 0.012),
        Role::Summary => s(BASE, 26.0, 600.0, 0.0),
        Role::AlertTitle(_) => s(14.0, 22.0, 620.0, 0.0),
        Role::Cell => s(14.5, 22.0, 400.0, 0.0),
        Role::HeadCell => s(13.5, 22.0, 600.0, 0.004),
        Role::Footnote => s(14.0, 22.0, 400.0, 0.0),
    }
}

fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

/// The `TextFormat` for one run of inline text.
pub fn text_format(theme: &Theme, zoom: f32, ppp: f32, role: Role, flags: u16, link: bool) -> TextFormat {
    let st = role_style(role);
    let mut size = st.size * zoom;
    let line = snap(st.line * zoom, ppp);

    let mut color = match role {
        Role::Heading(6) => theme.muted,
        Role::Heading(_) | Role::Summary | Role::HeadCell => theme.strong,
        Role::AlertTitle(kind) => theme.alert_color(kind),
        Role::Footnote => theme.muted,
        _ => theme.text,
    };
    let mut weight = st.weight;
    if flags & style::BOLD != 0 {
        weight = if weight >= 600.0 { 700.0 } else { 640.0 };
        if matches!(role, Role::Body | Role::Cell | Role::Footnote) {
            color = theme.strong;
        }
    }
    if flags & style::MUTED != 0 {
        color = theme.muted;
    }
    if link {
        color = theme.link;
    }

    let mono = flags & (style::CODE | style::KBD) != 0;
    let script = flags & (style::SUP | style::SUB) != 0;
    if mono {
        size *= 0.875;
    }
    if script {
        size *= 0.74;
    }

    let (family, coords) = if mono {
        (FontFamily::Monospace, VariationCoords::new([(b"wght", if weight > 500.0 { 600.0 } else { 400.0 })]))
    } else if flags & style::ITALIC != 0 {
        (FontFamily::Name(ITALIC_FAMILY.into()), VariationCoords::new([(b"wght", weight)]))
    } else {
        (FontFamily::Proportional, VariationCoords::new([(b"wght", weight), (b"opsz", size.clamp(14.0, 32.0))]))
    };

    let mut f = TextFormat {
        font_id: FontId::new(size, family),
        extra_letter_spacing: if mono { 0.0 } else { st.tracking * size },
        line_height: Some(line),
        color,
        coords,
        ..Default::default()
    };
    if flags & style::STRIKE != 0 {
        if !link {
            f.color = theme.muted;
        }
        f.strikethrough = Stroke::new((1.1 * zoom).max(1.0), theme.muted);
    }
    if flags & style::UNDER != 0 {
        f.underline = Stroke::new((1.0 * zoom).max(1.0), color);
    }
    if flags & style::MARK != 0 {
        f.background = theme.mark;
        f.expand_bg = 1.5;
        if theme.dark {
            f.color = theme.strong;
        }
    }
    if flags & style::SUP != 0 {
        f.valign = Align::TOP;
        f.line_height = Some(snap(line * 0.74, ppp));
    } else if flags & style::SUB != 0 {
        f.valign = Align::BOTTOM;
        f.line_height = Some(snap(line * 0.74, ppp));
    }
    f
}

/// The `TextFormat` for a syntax token in a code block.
pub fn code_format(theme: &Theme, zoom: f32, ppp: f32, token: u16) -> TextFormat {
    TextFormat {
        font_id: FontId::new(CODE_SIZE * zoom, FontFamily::Monospace),
        line_height: Some(snap(CODE_LINE * zoom, ppp)),
        color: theme.syn[(token as usize).min(15)],
        coords: VariationCoords::new([(b"wght", 400.0)]),
        ..Default::default()
    }
}

/// Small interface text (table of contents, find bar, status pill).
pub fn ui_format(size: f32, weight: f32, color: Color32) -> TextFormat {
    TextFormat {
        font_id: FontId::new(size, FontFamily::Proportional),
        color,
        coords: VariationCoords::new([(b"wght", weight), (b"opsz", size.clamp(14.0, 32.0))]),
        ..Default::default()
    }
}

pub fn mono_format(size: f32, weight: f32, color: Color32) -> TextFormat {
    TextFormat {
        font_id: FontId::new(size, FontFamily::Monospace),
        color,
        coords: VariationCoords::new([(b"wght", weight)]),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Fonts and egui chrome

pub fn font_definitions() -> FontDefinitions {
    // Retina text needs no grid fitting; hinting would only distort Inter's outlines.
    let plain = FontTweak { hinting: Some(false), ..Default::default() };
    let mut defs = FontDefinitions::empty();
    defs.font_data.insert("Inter".into(), Arc::new(FontData::from_static(INTER).tweak(plain.clone())));
    defs.font_data.insert("Inter-Italic".into(), Arc::new(FontData::from_static(INTER_ITALIC).tweak(plain.clone())));
    defs.font_data.insert(
        "JetBrainsMono".into(),
        Arc::new(FontData::from_static(MONO).tweak(FontTweak {
            // Sit on the same baseline as Inter when mixed into a line of prose.
            y_offset_factor: 0.035,
            ..plain
        })),
    );
    defs.families.insert(FontFamily::Proportional, vec!["Inter".into()]);
    defs.families.insert(FontFamily::Monospace, vec!["JetBrainsMono".into(), "Inter".into()]);
    defs.families.insert(FontFamily::Name(ITALIC_FAMILY.into()), vec!["Inter-Italic".into(), "Inter".into()]);
    defs
}

/// Style the few stock egui widgets we use (the find field and scroll bars).
pub fn apply_chrome(ctx: &egui::Context, theme: &Theme) {
    let mut visuals = if theme.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.panel_fill = theme.bg;
    visuals.window_fill = theme.bg;
    visuals.extreme_bg_color = theme.bg;
    visuals.override_text_color = Some(theme.strong);
    visuals.selection.bg_fill = theme.selection;
    visuals.selection.stroke = Stroke::new(1.0, theme.accent);
    visuals.text_cursor.stroke = Stroke::new(1.5, theme.accent);
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.bg_stroke = Stroke::NONE;
        w.corner_radius = egui::CornerRadius::same(4);
    }
    // Scroll bar handle colours.
    visuals.widgets.inactive.bg_fill = mix(theme.bg, theme.faint, 0.55);
    visuals.widgets.hovered.bg_fill = mix(theme.bg, theme.muted, 0.75);
    visuals.widgets.active.bg_fill = theme.muted;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, theme.faint);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, theme.muted);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, theme.muted);
    ctx.set_visuals_of(if theme.dark { egui::Theme::Dark } else { egui::Theme::Light }, visuals);

    ctx.all_styles_mut(|style| {
        let mut scroll = egui::style::ScrollStyle::floating();
        scroll.bar_width = 7.0;
        scroll.floating_width = 4.0;
        scroll.floating_allocated_width = 0.0;
        scroll.bar_inner_margin = 3.0;
        scroll.bar_outer_margin = 2.0;
        scroll.dormant_handle_opacity = 0.0;
        scroll.active_handle_opacity = 0.5;
        scroll.interact_handle_opacity = 0.9;
        scroll.dormant_background_opacity = 0.0;
        scroll.active_background_opacity = 0.0;
        scroll.interact_background_opacity = 0.0;
        style.spacing.scroll = scroll;
        style.interaction.selectable_labels = false;
        style.animation_time = 0.1;
    });
}
