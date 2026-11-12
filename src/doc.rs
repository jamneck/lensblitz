//! The document model, and the Markdown → model conversion.
//!
//! A document is a *flat* list of leaf blocks (paragraphs, headings, code, tables, rules).
//! Nesting (lists, block quotes, alerts) is baked into each leaf as an indent, a marker and
//! a set of "rails", so the viewer can lay out and virtualise blocks independently.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd, TextMergeStream,
};

use crate::highlight;
use crate::html::{self, Tok};

/// Inline style bits stored in `Span::style` (for prose; code blocks store a syntax token).
pub mod style {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 1 << 1;
    pub const CODE: u16 = 1 << 2;
    pub const STRIKE: u16 = 1 << 3;
    pub const UNDER: u16 = 1 << 4;
    pub const SUP: u16 = 1 << 5;
    pub const SUB: u16 = 1 << 6;
    pub const KBD: u16 = 1 << 7;
    pub const MARK: u16 = 1 << 8;
    pub const MUTED: u16 = 1 << 9;
}

pub const NO_LINK: u32 = u32::MAX;

/// Long code blocks are split into chunks of this many lines so they virtualise like prose.
const CODE_CHUNK_LINES: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Role {
    Body,
    Heading(u8),
    Summary,
    AlertTitle(AlertKind),
    Cell,
    HeadCell,
    Footnote,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum HAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Hash, Debug)]
pub struct Span {
    /// Byte offset one past the end of this run; it starts where the previous one ended.
    pub end: u32,
    pub style: u16,
    /// Index into `Block::links`, or `NO_LINK`.
    pub link: u32,
}

/// One run of styled text that becomes one galley.
#[derive(Clone, Hash, Debug, Default)]
pub struct Piece {
    pub text: String,
    pub spans: Vec<Span>,
}

impl Piece {
    fn push(&mut self, s: &str, style: u16, link: u32) {
        if s.is_empty() {
            return;
        }
        self.text.push_str(s);
        let end = self.text.len() as u32;
        match self.spans.last_mut() {
            Some(last) if last.style == style && last.link == link => last.end = end,
            _ => self.spans.push(Span { end, style, link }),
        }
    }

    fn trim_end(&mut self) {
        let len = self.text.trim_end().len();
        self.text.truncate(len);
        self.spans.retain_mut(|s| {
            s.end = s.end.min(len as u32);
            true
        });
        let mut prev = 0;
        self.spans.retain(|s| {
            let keep = s.end > prev;
            prev = prev.max(s.end);
            keep
        });
    }
}

#[derive(Clone, Copy, Hash, Debug, PartialEq)]
pub enum Dim {
    Px(u32),
    Percent(u32),
}

#[derive(Clone, Hash, Debug)]
pub struct Image {
    pub src: String,
    pub alt: String,
    pub link: u32,
    pub width: Option<Dim>,
    pub height: Option<Dim>,
}

/// What a text block is made of, in reading order.
#[derive(Clone, Copy, Hash, Debug)]
pub enum Flow {
    Text(u32),
    Image(u32),
    Break,
}

#[derive(Clone, Hash, Debug, PartialEq, Default)]
pub enum Marker {
    #[default]
    None,
    Bullet(u8),
    Number(String),
    Task(bool),
    Footnote(String),
    Disclosure,
}

/// A vertical bar (block quote) or tinted panel (alert) that this block sits inside.
#[derive(Clone, Hash, Debug)]
pub struct Rail {
    /// Left edge, in em from the column edge.
    pub x_em: u32,
    pub alert: Option<AlertKind>,
    /// First / last block inside the rail (controls where it starts, ends and rounds).
    pub first: bool,
    pub last: bool,
}

#[derive(Clone, Hash, Debug)]
pub enum Kind {
    Text(Role),
    /// `first`/`last` say whether this chunk opens/closes the visual panel.
    Code {
        lang: String,
        first: bool,
        last: bool,
    },
    /// `pieces` holds the cells row-major; the first `cols` cells are the header.
    Table {
        cols: usize,
        aligns: Vec<HAlign>,
    },
    Rule,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub kind: Kind,
    pub pieces: Vec<Piece>,
    pub flow: Vec<Flow>,
    pub links: Vec<String>,
    pub images: Vec<Image>,
    /// Left indent in hundredths of an em (integers so the block can be hashed).
    pub indent: u32,
    /// Space above the block, in hundredths of an em.
    pub gap: u32,
    pub pad_top: u32,
    pub pad_bottom: u32,
    pub marker: Marker,
    pub rails: Vec<Rail>,
    pub align: HAlign,
    /// For code chunks: index of the first chunk of the same code block.
    pub group: u32,
    /// Content hash, used to carry layout across live reloads.
    pub hash: u64,
}

impl Block {
    fn compute_hash(&mut self) {
        let mut h = DefaultHasher::new();
        self.kind.hash(&mut h);
        self.pieces.hash(&mut h);
        self.flow.hash(&mut h);
        self.links.hash(&mut h);
        self.images.hash(&mut h);
        (self.indent, self.gap, self.pad_top, self.pad_bottom).hash(&mut h);
        self.marker.hash(&mut h);
        self.rails.hash(&mut h);
        self.align.hash(&mut h);
        self.hash = h.finish();
    }

    /// The block's text, pieces joined by spaces.
    #[cfg(test)]
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for (i, p) in self.pieces.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&p.text);
        }
        out
    }
}

pub struct Heading {
    pub block: usize,
    pub level: u8,
    pub title: String,
}

#[derive(Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub outline: Vec<Heading>,
    /// Heading slugs and footnote ids → block index.
    pub anchors: HashMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Builder

enum Ctx {
    Quote { alert: Option<AlertKind>, start: usize, started: bool },
    List { next: Option<u64>, items: u32 },
    Item { marker: Option<Marker>, first_in_list: bool },
    Footnote { label: String, used: bool },
}

struct Inline {
    role: Role,
    /// Implicit paragraph (a tight list item).
    tight: bool,
    align: HAlign,
    pieces: Vec<Piece>,
    flow: Vec<Flow>,
    piece: Piece,
    links: Vec<String>,
    images: Vec<Image>,
}

impl Inline {
    fn finish_piece(&mut self) {
        self.piece.trim_end();
        if !self.piece.text.is_empty() {
            let idx = self.pieces.len() as u32;
            self.pieces.push(std::mem::take(&mut self.piece));
            self.flow.push(Flow::Text(idx));
        } else {
            self.piece = Piece::default();
        }
    }

    fn link_index(&mut self, url: &str) -> u32 {
        if let Some(i) = self.links.iter().position(|l| l == url) {
            return i as u32;
        }
        self.links.push(url.to_owned());
        (self.links.len() - 1) as u32
    }
}

struct ImgAcc {
    src: String,
    alt: String,
}

struct TableAcc {
    aligns: Vec<HAlign>,
    cells: Vec<Piece>,
    links: Vec<String>,
    in_head: bool,
    row_start: usize,
}

#[derive(Default)]
struct Styles {
    bold: u8,
    italic: u8,
    strike: u8,
    code: u8,
    under: u8,
    sup: u8,
    sub: u8,
    kbd: u8,
    mark: u8,
}

impl Styles {
    fn flags(&self) -> u16 {
        let bit = |n: u8, f: u16| if n > 0 { f } else { 0 };
        bit(self.bold, style::BOLD)
            | bit(self.italic, style::ITALIC)
            | bit(self.strike, style::STRIKE)
            | bit(self.code, style::CODE)
            | bit(self.under, style::UNDER)
            | bit(self.sup, style::SUP)
            | bit(self.sub, style::SUB)
            | bit(self.kbd, style::KBD)
            | bit(self.mark, style::MARK)
    }
}

#[derive(Default)]
struct Builder {
    doc: Doc,
    stack: Vec<Ctx>,
    cur: Option<Inline>,
    sty: Styles,
    /// Open links (innermost last). `None` for an `<a>` without `href`.
    links: Vec<Option<String>>,
    img: Option<ImgAcc>,
    code: Option<(String, String)>,
    table: Option<TableAcc>,
    aligns: Vec<(String, HAlign)>,
    html_buf: Option<String>,
    html_pre: Option<(String, String)>,
    html_skip: u32,
    slugs: HashMap<String, u32>,
    prev_below: u32,
    prev_heading: bool,
}

pub fn parse(src: &str) -> Doc {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_WIKILINKS;
    let mut b = Builder::default();
    // Merged text events let bare URLs containing `_` or `*` be recognised in one piece.
    for event in TextMergeStream::new(Parser::new_ext(src, options)) {
        b.event(event);
    }
    b.flush_inline();
    b.doc
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// `(above, below)` margins in hundredths of an em.
fn margins(kind: &Kind, image_only: bool) -> (u32, u32) {
    match kind {
        Kind::Text(Role::Heading(l)) => {
            let i = (*l as usize).clamp(1, 6) - 1;
            ([250, 230, 190, 165, 150, 150][i], [100, 85, 65, 55, 50, 50][i])
        }
        Kind::Text(Role::Summary) => (100, 60),
        Kind::Text(Role::AlertTitle(_)) => (120, 30),
        Kind::Text(_) if image_only => (125, 125),
        Kind::Text(_) => (100, 100),
        Kind::Code { .. } => (115, 115),
        Kind::Table { .. } => (125, 125),
        Kind::Rule => (200, 200),
    }
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        // Inside a code block or front matter everything is literal text.
        if let Some((_, text)) = &mut self.code {
            match event {
                Event::Text(t) => text.push_str(&t),
                Event::End(TagEnd::CodeBlock) | Event::End(TagEnd::MetadataBlock(_)) => self.end_code(),
                _ => {}
            }
            return;
        }
        if let Some(buf) = &mut self.html_buf {
            match event {
                Event::Html(t) | Event::Text(t) => buf.push_str(&t),
                Event::End(TagEnd::HtmlBlock) => {
                    let buf = self.html_buf.take().unwrap_or_default();
                    self.html(&buf, true);
                    self.flush_inline();
                }
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.sty.code > 0 || !self.links.is_empty() || self.img.is_some() {
                    self.text(&t);
                } else {
                    self.text_with_autolinks(&t);
                }
            }
            Event::Code(t) => {
                self.sty.code += 1;
                self.text(&t);
                self.sty.code -= 1;
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                self.sty.code += 1;
                self.text(&t);
                self.sty.code -= 1;
            }
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => {
                self.flush_inline();
                self.push_block(Kind::Rule, Inline::empty(), false);
            }
            Event::TaskListMarker(checked) => {
                if let Some(Ctx::Item { marker, .. }) = self.stack.last_mut()
                    && marker.is_some()
                {
                    *marker = Some(Marker::Task(checked));
                }
            }
            Event::FootnoteReference(label) => {
                self.sty.sup += 1;
                self.links.push(Some(format!("#fn-{label}")));
                self.text(&label);
                self.links.pop();
                self.sty.sup -= 1;
            }
            Event::InlineHtml(t) => self.html(&t, false),
            Event::Html(t) => self.html(&t, true),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush_inline();
                self.begin_inline(Role::Body, false);
            }
            Tag::Heading { level, .. } => {
                self.flush_inline();
                self.begin_inline(Role::Heading(heading_level(level)), false);
            }
            Tag::BlockQuote(kind) => {
                self.flush_inline();
                let alert = kind.map(|k| match k {
                    BlockQuoteKind::Note => AlertKind::Note,
                    BlockQuoteKind::Tip => AlertKind::Tip,
                    BlockQuoteKind::Important => AlertKind::Important,
                    BlockQuoteKind::Warning => AlertKind::Warning,
                    BlockQuoteKind::Caution => AlertKind::Caution,
                });
                self.stack.push(Ctx::Quote { alert, start: self.doc.blocks.len(), started: false });
                if let Some(kind) = alert {
                    let mut inl = Inline::new(Role::AlertTitle(kind), false, HAlign::Left);
                    inl.piece.push(kind.label(), 0, NO_LINK);
                    inl.finish_piece();
                    self.push_block(Kind::Text(Role::AlertTitle(kind)), inl, false);
                }
            }
            Tag::CodeBlock(kind) => {
                self.flush_inline();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split(|c: char| c.is_whitespace() || c == ',' || c == '{').next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::MetadataBlock(_) => {
                self.flush_inline();
                self.code = Some(("yaml".to_owned(), String::new()));
            }
            Tag::HtmlBlock => self.html_buf = Some(String::new()),
            Tag::List(first) => {
                self.flush_inline();
                self.stack.push(Ctx::List { next: first, items: 0 });
            }
            Tag::Item => {
                self.flush_inline();
                let depth = self.stack.iter().filter(|c| matches!(c, Ctx::List { .. })).count();
                let mut marker = Marker::Bullet(depth.saturating_sub(1) as u8);
                let mut first_in_list = false;
                if let Some(Ctx::List { next, items }) = self.stack.last_mut() {
                    if let Some(n) = next {
                        marker = Marker::Number(format!("{n}."));
                        *n += 1;
                    }
                    first_in_list = *items == 0;
                    *items += 1;
                }
                self.stack.push(Ctx::Item { marker: Some(marker), first_in_list });
            }
            Tag::FootnoteDefinition(label) => {
                self.flush_inline();
                self.stack.push(Ctx::Footnote { label: label.to_string(), used: false });
            }
            Tag::Table(aligns) => {
                self.flush_inline();
                let aligns = aligns
                    .iter()
                    .map(|a| match a {
                        Alignment::Center => HAlign::Center,
                        Alignment::Right => HAlign::Right,
                        _ => HAlign::Left,
                    })
                    .collect();
                self.table =
                    Some(TableAcc { aligns, cells: Vec::new(), links: Vec::new(), in_head: false, row_start: 0 });
            }
            Tag::TableHead => {
                if let Some(t) = &mut self.table {
                    t.in_head = true;
                    t.row_start = t.cells.len();
                }
            }
            Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.row_start = t.cells.len();
                }
            }
            Tag::TableCell => {
                let head = self.table.as_ref().is_some_and(|t| t.in_head);
                self.cur = Some(Inline::new(if head { Role::HeadCell } else { Role::Cell }, false, HAlign::Left));
            }
            Tag::Emphasis => self.sty.italic += 1,
            Tag::Strong => self.sty.bold += 1,
            Tag::Strikethrough => self.sty.strike += 1,
            Tag::Superscript => self.sty.sup += 1,
            Tag::Subscript => self.sty.sub += 1,
            Tag::Link { dest_url, .. } => self.links.push(Some(dest_url.to_string())),
            Tag::Image { dest_url, .. } => self.img = Some(ImgAcc { src: dest_url.to_string(), alt: String::new() }),
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => self.flush_inline(),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush_inline(),
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                self.end_quote();
            }
            TagEnd::List(_) => {
                self.flush_inline();
                if matches!(self.stack.last(), Some(Ctx::List { .. })) {
                    self.stack.pop();
                }
                self.prev_below = self.prev_below.max(90);
            }
            TagEnd::Item => {
                self.flush_inline();
                if let Some(Ctx::Item { marker: Some(_), .. }) = self.stack.last() {
                    // An empty item still shows its marker.
                    let mut inl = Inline::new(Role::Body, true, HAlign::Left);
                    inl.pieces.push(Piece::default());
                    inl.flow.push(Flow::Text(0));
                    self.push_block(Kind::Text(Role::Body), inl, true);
                }
                if matches!(self.stack.last(), Some(Ctx::Item { .. })) {
                    self.stack.pop();
                }
            }
            TagEnd::FootnoteDefinition => {
                self.flush_inline();
                if matches!(self.stack.last(), Some(Ctx::Footnote { .. })) {
                    self.stack.pop();
                }
            }
            TagEnd::TableCell => {
                if let (Some(mut inl), Some(t)) = (self.cur.take(), self.table.as_mut()) {
                    inl.finish_piece();
                    let mut piece = inl.pieces.into_iter().next().unwrap_or_default();
                    // Re-home the cell's links into the table's link list.
                    let remap: Vec<u32> = inl
                        .links
                        .iter()
                        .map(|url| {
                            t.links.iter().position(|l| l == url).unwrap_or_else(|| {
                                t.links.push(url.clone());
                                t.links.len() - 1
                            }) as u32
                        })
                        .collect();
                    for s in &mut piece.spans {
                        if s.link != NO_LINK {
                            s.link = remap[s.link as usize];
                        }
                    }
                    t.cells.push(piece);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(t) = &mut self.table {
                    // Every row has exactly one cell per column.
                    let cols = t.aligns.len().max(1);
                    t.cells.resize(t.row_start + cols, Piece::default());
                    t.in_head = false;
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    let cols = t.aligns.len().max(1);
                    let mut inl = Inline::empty();
                    inl.pieces = t.cells;
                    inl.links = t.links;
                    self.push_block(Kind::Table { cols, aligns: t.aligns }, inl, false);
                }
            }
            TagEnd::Emphasis => self.sty.italic = self.sty.italic.saturating_sub(1),
            TagEnd::Strong => self.sty.bold = self.sty.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.sty.strike = self.sty.strike.saturating_sub(1),
            TagEnd::Superscript => self.sty.sup = self.sty.sup.saturating_sub(1),
            TagEnd::Subscript => self.sty.sub = self.sty.sub.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                if let Some(img) = self.img.take() {
                    self.image(Image { src: img.src, alt: img.alt, link: NO_LINK, width: None, height: None });
                }
            }
            _ => self.flush_inline(),
        }
    }

    fn end_quote(&mut self) {
        let Some(pos) = self.stack.iter().rposition(|c| matches!(c, Ctx::Quote { .. })) else {
            return;
        };
        let Ctx::Quote { alert, start, .. } = self.stack.remove(pos) else {
            return;
        };
        self.stack.truncate(pos);
        let depth = self.stack.iter().filter(|c| matches!(c, Ctx::Quote { .. })).count();
        if self.doc.blocks.len() > start
            && let Some(block) = self.doc.blocks.last_mut()
        {
            if let Some(rail) = block.rails.get_mut(depth) {
                rail.last = true;
            }
            if alert.is_some() {
                block.pad_bottom += 85;
            }
            block.compute_hash();
        }
        self.prev_below = self.prev_below.max(115);
        self.prev_heading = false;
    }

    fn end_code(&mut self) {
        let Some((lang, mut text)) = self.code.take() else {
            return;
        };
        if text.ends_with('\n') {
            text.pop();
        }
        let tokens = highlight::highlight(&lang, &text);
        // Split into chunks of whole lines; each chunk is its own block.
        let mut chunks: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        let mut lines = 0;
        for (i, c) in text.bytes().enumerate() {
            if c == b'\n' {
                lines += 1;
                if lines == CODE_CHUNK_LINES {
                    chunks.push((start, i));
                    start = i + 1;
                    lines = 0;
                }
            }
        }
        chunks.push((start, text.len()));

        let group = self.doc.blocks.len() as u32;
        let count = chunks.len();
        let mut tok = 0;
        for (n, (s, e)) in chunks.into_iter().enumerate() {
            let mut piece = Piece { text: text[s..e].to_owned(), spans: Vec::new() };
            while tok < tokens.len() {
                let (end, class) = tokens[tok];
                let end = end as usize;
                if end > s {
                    let local = (end.min(e) - s) as u32;
                    if local > piece.spans.last().map_or(0, |sp| sp.end) {
                        piece.spans.push(Span { end: local, style: class, link: NO_LINK });
                    }
                }
                if end > e {
                    break;
                }
                tok += 1;
            }
            if piece.spans.last().is_none_or(|sp| (sp.end as usize) < piece.text.len()) {
                piece.spans.push(Span { end: piece.text.len() as u32, style: 0, link: NO_LINK });
            }
            let mut inl = Inline::empty();
            inl.pieces.push(piece);
            let kind = Kind::Code { lang: lang.clone(), first: n == 0, last: n + 1 == count };
            let idx = self.push_block(kind, inl, false);
            self.doc.blocks[idx].group = group;
        }
    }

    // --- inline text -------------------------------------------------------

    fn begin_inline(&mut self, role: Role, tight: bool) {
        let role = if role == Role::Body && self.stack.iter().any(|c| matches!(c, Ctx::Footnote { .. })) {
            Role::Footnote
        } else {
            role
        };
        let align = self.aligns.last().map_or(HAlign::Left, |a| a.1);
        self.cur = Some(Inline::new(role, tight, align));
    }

    fn inline(&mut self) -> &mut Inline {
        if self.cur.is_none() {
            self.begin_inline(Role::Body, true);
        }
        self.cur.as_mut().expect("inline was just created")
    }

    fn text(&mut self, s: &str) {
        if let Some(img) = &mut self.img {
            img.alt.push_str(s);
            return;
        }
        if self.html_skip > 0 {
            return;
        }
        let flags = self.sty.flags();
        let link = self.links.last().cloned().flatten();
        let inl = self.inline();
        let s = if inl.piece.text.is_empty() { s.trim_start_matches([' ', '\n', '\t']) } else { s };
        if s.is_empty() {
            return;
        }
        let link = link.map_or(NO_LINK, |url| inl.link_index(&url));
        inl.piece.push(s, flags, link);
    }

    fn text_with_autolinks(&mut self, s: &str) {
        let mut last = 0;
        for (start, end) in find_urls(s) {
            self.text(&s[last..start]);
            let url = &s[start..end];
            let href = if url.starts_with("www.") { format!("https://{url}") } else { url.to_owned() };
            self.links.push(Some(href));
            self.text(url);
            self.links.pop();
            last = end;
        }
        self.text(&s[last..]);
    }

    fn soft_break(&mut self) {
        if self.img.is_some() {
            self.text(" ");
            return;
        }
        if let Some(inl) = &mut self.cur
            && !inl.piece.text.is_empty()
            && !inl.piece.text.ends_with([' ', '\n'])
        {
            self.text(" ");
        }
    }

    fn hard_break(&mut self) {
        let Some(inl) = &mut self.cur else {
            return;
        };
        if !inl.piece.text.is_empty() {
            let flags = self.sty.flags();
            inl.piece.push("\n", flags, NO_LINK);
        } else if matches!(inl.flow.last(), Some(Flow::Image(_))) {
            inl.flow.push(Flow::Break);
        }
    }

    fn image(&mut self, mut img: Image) {
        if self.html_skip > 0 {
            return;
        }
        let link = self.links.last().cloned().flatten();
        let in_cell = self.table.is_some();
        let inl = self.inline();
        if in_cell {
            // Table cells are text only: show the alt text instead.
            let link = link.map_or(NO_LINK, |url| inl.link_index(&url));
            let alt = if img.alt.is_empty() { "image" } else { img.alt.as_str() };
            inl.piece.push(alt, style::MUTED | style::ITALIC, link);
            return;
        }
        inl.finish_piece();
        img.link = link.map_or(NO_LINK, |url| inl.link_index(&url));
        inl.images.push(img);
        inl.flow.push(Flow::Image((inl.images.len() - 1) as u32));
    }

    fn flush_inline(&mut self) {
        if self.table.is_some() {
            return;
        }
        let Some(mut inl) = self.cur.take() else {
            return;
        };
        inl.finish_piece();
        while matches!(inl.flow.last(), Some(Flow::Break)) {
            inl.flow.pop();
        }
        if inl.flow.is_empty() {
            return;
        }
        let role = inl.role;
        let tight = inl.tight;
        let title = match role {
            Role::Heading(_) => inl.pieces.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        };
        let idx = self.push_block(Kind::Text(role), inl, tight);
        if let Role::Heading(level) = role {
            let slug = self.unique_slug(&title);
            self.doc.anchors.insert(slug, idx);
            if !title.trim().is_empty() {
                self.doc.outline.push(Heading { block: idx, level, title });
            }
        }
    }

    fn unique_slug(&mut self, title: &str) -> String {
        let base = slugify(title);
        let n = self.slugs.entry(base.clone()).or_insert(0);
        let slug = if *n == 0 { base } else { format!("{base}-{n}") };
        *n += 1;
        slug
    }

    // --- blocks ------------------------------------------------------------

    fn push_block(&mut self, kind: Kind, inl: Inline, tight: bool) -> usize {
        let idx = self.doc.blocks.len();
        let mut indent = 0u32;
        let mut rails = Vec::new();
        let mut marker = Marker::None;
        let mut pad_top = 0;
        let mut first_in_item = false;
        let mut list_start = false;
        let mut anchor = None;
        let list_depth = self.stack.iter().filter(|c| matches!(c, Ctx::List { .. })).count();
        for ctx in &mut self.stack {
            match ctx {
                Ctx::Quote { alert, started, .. } => {
                    rails.push(Rail { x_em: indent, alert: *alert, first: !*started, last: false });
                    if !*started && alert.is_some() {
                        pad_top += 85;
                    }
                    *started = true;
                    indent += if alert.is_some() { 115 } else { 125 };
                }
                Ctx::List { .. } => {}
                Ctx::Item { marker: m, first_in_list } => {
                    indent += 165;
                    if let Some(m) = m.take() {
                        marker = m;
                        first_in_item = true;
                        list_start = *first_in_list;
                    }
                }
                Ctx::Footnote { label, used } => {
                    indent += 190;
                    if !*used {
                        *used = true;
                        marker = Marker::Footnote(label.clone());
                        anchor = Some(format!("fn-{label}"));
                    }
                }
            }
        }
        if matches!(kind, Kind::Text(Role::Summary)) {
            marker = Marker::Disclosure;
            indent += 125;
        }

        let image_only = matches!(kind, Kind::Text(_)) && !inl.flow.iter().any(|f| matches!(f, Flow::Text(_)));
        let (above, below) = margins(&kind, image_only);
        let continues_code = matches!(kind, Kind::Code { first: false, .. });
        let gap = if idx == 0 || continues_code {
            0
        } else if self.prev_heading {
            // Content hugs its heading; a heading directly under another keeps some air.
            if matches!(kind, Kind::Text(Role::Heading(_))) {
                self.prev_below.max(above * 55 / 100)
            } else {
                self.prev_below
            }
        } else if first_in_item {
            let item_gap = if tight { 28 } else { 70 };
            if list_start && list_depth <= 1 { self.prev_below.clamp(60, 80) } else { item_gap }
        } else {
            self.prev_below.max(above)
        };
        self.prev_below = below;
        // The block after a heading (or an alert title, or a summary) hugs it.
        self.prev_heading = matches!(kind, Kind::Text(Role::Heading(_) | Role::AlertTitle(_) | Role::Summary));

        let mut block = Block {
            kind,
            pieces: inl.pieces,
            flow: inl.flow,
            links: inl.links,
            images: inl.images,
            indent,
            gap,
            pad_top,
            pad_bottom: 0,
            marker,
            rails,
            align: inl.align,
            group: idx as u32,
            hash: 0,
        };
        block.compute_hash();
        self.doc.blocks.push(block);
        if let Some(anchor) = anchor {
            self.doc.anchors.insert(anchor, idx);
        }
        idx
    }

    // --- HTML --------------------------------------------------------------

    fn html(&mut self, raw: &str, block: bool) {
        for tok in html::tokens(raw) {
            if let Some((_, text)) = &mut self.html_pre {
                match tok {
                    Tok::Text(t) => text.push_str(&html::decode_entities(t)),
                    Tok::Close(name) if name == "pre" => {
                        self.code = self.html_pre.take();
                        self.end_code();
                    }
                    _ => {}
                }
                continue;
            }
            match tok {
                Tok::Text(t) => {
                    if self.html_skip > 0 {
                        continue;
                    }
                    let collapsed = collapse_ws(t);
                    if block && self.cur.is_none() && collapsed.trim().is_empty() {
                        continue;
                    }
                    if self.cur.is_none() {
                        self.begin_inline(Role::Body, false);
                    }
                    self.text(&html::decode_entities(&collapsed));
                }
                Tok::Open { name, attrs } => self.html_open(&name, &attrs, block),
                Tok::Close(name) => self.html_close(&name, block),
            }
        }
    }

    fn html_open(&mut self, name: &str, attrs: &[(String, String)], block: bool) {
        match name {
            "script" | "style" | "svg" | "template" | "head" | "video" | "audio" | "iframe" | "object" => {
                self.html_skip += 1
            }
            _ if self.html_skip > 0 => {}
            "br" => {
                if self.cur.is_some() {
                    self.hard_break();
                }
            }
            "hr" => {
                self.flush_inline();
                self.push_block(Kind::Rule, Inline::empty(), false);
            }
            "img" => {
                let Some(src) = html::attr(attrs, "src") else {
                    return;
                };
                if self.cur.is_none() {
                    self.begin_inline(Role::Body, false);
                }
                self.image(Image {
                    src: src.to_owned(),
                    alt: html::attr(attrs, "alt").unwrap_or("").to_owned(),
                    link: NO_LINK,
                    width: html::attr(attrs, "width").and_then(parse_dim),
                    height: html::attr(attrs, "height").and_then(parse_dim),
                });
            }
            "b" | "strong" => self.sty.bold += 1,
            "i" | "em" | "cite" | "var" | "dfn" => self.sty.italic += 1,
            "s" | "del" | "strike" => self.sty.strike += 1,
            "code" | "tt" | "samp" => self.sty.code += 1,
            "kbd" => self.sty.kbd += 1,
            "u" | "ins" => self.sty.under += 1,
            "sup" => self.sty.sup += 1,
            "sub" => self.sty.sub += 1,
            "mark" => self.sty.mark += 1,
            "a" => self.links.push(html::attr(attrs, "href").map(str::to_owned)),
            "p" | "div" | "center" | "section" | "article" | "header" | "footer" | "figure" | "figcaption" | "h1"
            | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "caption" | "dt" | "dd"
                if block =>
            {
                self.flush_inline();
                let inherited = self.aligns.last().map_or(HAlign::Left, |a| a.1);
                let align = match html::attr(attrs, "align").map(str::to_ascii_lowercase).as_deref() {
                    Some("center") | Some("middle") => HAlign::Center,
                    Some("right") => HAlign::Right,
                    Some("left") => HAlign::Left,
                    _ if name == "center" => HAlign::Center,
                    _ => inherited,
                };
                self.aligns.push((name.to_owned(), align));
                if let Some(level) = name.strip_prefix('h').and_then(|l| l.parse::<u8>().ok()) {
                    self.begin_inline(Role::Heading(level), false);
                }
            }
            "td" | "th" if block => {
                // HTML tables are flattened: cells on one row, separated by spacing.
                if self.cur.as_ref().is_some_and(|c| !c.piece.text.is_empty()) {
                    self.text("   ");
                }
            }
            "details" if block => self.flush_inline(),
            "summary" if block => {
                self.flush_inline();
                self.begin_inline(Role::Summary, false);
            }
            "blockquote" if block => {
                self.flush_inline();
                self.stack.push(Ctx::Quote { alert: None, start: self.doc.blocks.len(), started: false });
            }
            "ul" | "ol" if block => {
                self.flush_inline();
                self.stack.push(Ctx::List { next: (name == "ol").then_some(1), items: 0 });
            }
            "li" if block => {
                self.flush_inline();
                if matches!(self.stack.last(), Some(Ctx::Item { .. })) {
                    self.stack.pop();
                }
                if matches!(self.stack.last(), Some(Ctx::List { .. })) {
                    self.start(Tag::Item);
                    self.begin_inline(Role::Body, true);
                }
            }
            "pre" if block => {
                self.flush_inline();
                self.html_pre = Some((String::new(), String::new()));
            }
            _ => {}
        }
    }

    fn html_close(&mut self, name: &str, block: bool) {
        let dec = |n: &mut u8| *n = n.saturating_sub(1);
        match name {
            "script" | "style" | "svg" | "template" | "head" | "video" | "audio" | "iframe" | "object" => {
                self.html_skip = self.html_skip.saturating_sub(1)
            }
            _ if self.html_skip > 0 => {}
            "b" | "strong" => dec(&mut self.sty.bold),
            "i" | "em" | "cite" | "var" | "dfn" => dec(&mut self.sty.italic),
            "s" | "del" | "strike" => dec(&mut self.sty.strike),
            "code" | "tt" | "samp" => dec(&mut self.sty.code),
            "kbd" => dec(&mut self.sty.kbd),
            "u" | "ins" => dec(&mut self.sty.under),
            "sup" => dec(&mut self.sty.sup),
            "sub" => dec(&mut self.sty.sub),
            "mark" => dec(&mut self.sty.mark),
            "a" => {
                self.links.pop();
            }
            "p" | "div" | "center" | "section" | "article" | "header" | "footer" | "figure" | "figcaption" | "h1"
            | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "caption" | "dt" | "dd" | "summary" | "details" | "table"
                if block =>
            {
                self.flush_inline();
                if let Some(pos) = self.aligns.iter().rposition(|a| a.0 == name) {
                    self.aligns.truncate(pos);
                }
            }
            "blockquote" if block => {
                self.flush_inline();
                self.end_quote();
            }
            "li" if block => {
                self.flush_inline();
                if matches!(self.stack.last(), Some(Ctx::Item { .. })) {
                    self.stack.pop();
                }
            }
            "ul" | "ol" if block => {
                self.flush_inline();
                if matches!(self.stack.last(), Some(Ctx::Item { .. })) {
                    self.stack.pop();
                }
                if matches!(self.stack.last(), Some(Ctx::List { .. })) {
                    self.stack.pop();
                }
            }
            _ => {}
        }
    }
}

impl Inline {
    fn new(role: Role, tight: bool, align: HAlign) -> Self {
        Self {
            role,
            tight,
            align,
            pieces: Vec::new(),
            flow: Vec::new(),
            piece: Piece::default(),
            links: Vec::new(),
            images: Vec::new(),
        }
    }

    fn empty() -> Self {
        Self::new(Role::Body, false, HAlign::Left)
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_ascii_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out
}

fn parse_dim(s: &str) -> Option<Dim> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        return p.trim().parse::<f32>().ok().filter(|v| *v > 0.0).map(|v| Dim::Percent(v as u32));
    }
    let digits = s.trim_end_matches("px").trim();
    digits.parse::<f32>().ok().filter(|v| *v > 0.0).map(|v| Dim::Px(v as u32))
}

/// GitHub-style heading anchor.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.extend(c.to_lowercase());
        } else if c == ' ' {
            out.push('-');
        }
    }
    out
}

/// Byte ranges of bare URLs (`https://…`, `http://…`, `www.…`) in `s`, GFM autolink style.
fn find_urls(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if !s.contains("://") && !s.contains("www.") {
        return out;
    }
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let rest = &s[i..];
        let prefix = ["https://", "http://", "www."].into_iter().find(|p| rest.starts_with(p));
        let boundary = i == 0
            || matches!(b[i - 1], b' ' | b'\n' | b'\t' | b'(' | b'*' | b'_' | b'~' | b'"' | b'\'' | b'[' | b':' | b',');
        let Some(prefix) = prefix.filter(|_| boundary) else {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let mut end = i + rest.find(|c: char| c.is_whitespace() || c == '<').unwrap_or(rest.len());
        // Trailing punctuation is not part of the link; neither is an unbalanced `)`.
        loop {
            let url = &s[i..end];
            let Some(last) = url.chars().last() else {
                break;
            };
            let trim = match last {
                '?' | '!' | '.' | ',' | ':' | ';' | '*' | '_' | '~' | '\'' | '"' => true,
                ')' => url.matches(')').count() > url.matches('(').count(),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= last.len_utf8();
        }
        let host = &s[i + prefix.len()..end];
        if host.is_empty() || (prefix == "www." && !host.contains('.')) {
            i += prefix.len();
            continue;
        }
        out.push((i, end));
        i = end.max(i + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(doc: &Doc) -> Vec<String> {
        doc.blocks.iter().map(Block::plain_text).collect()
    }

    #[test]
    fn headings_and_anchors() {
        let doc = parse("# Hello World\n\ntext\n\n## Hello World\n\n## C++ & more!\n");
        assert_eq!(doc.outline.len(), 3);
        assert_eq!(doc.anchors.get("hello-world"), Some(&0));
        assert_eq!(doc.anchors.get("hello-world-1"), Some(&2));
        assert!(doc.anchors.contains_key("c--more"));
    }

    #[test]
    fn lists_nest_and_number() {
        let doc = parse("1. one\n2. two\n   - inner\n3. three\n\n- [x] done\n- [ ] todo\n");
        let markers: Vec<_> = doc.blocks.iter().map(|b| b.marker.clone()).collect();
        assert_eq!(
            markers,
            vec![
                Marker::Number("1.".into()),
                Marker::Number("2.".into()),
                Marker::Bullet(1),
                Marker::Number("3.".into()),
                Marker::Task(true),
                Marker::Task(false),
            ]
        );
        assert!(doc.blocks[2].indent > doc.blocks[1].indent);
        assert_eq!(texts(&doc), ["one", "two", "inner", "three", "done", "todo"]);
    }

    #[test]
    fn table_cells_are_row_major() {
        let doc = parse("| a | b |\n|:-:|--:|\n| 1 | 2 |\n| 3 |\n");
        let Kind::Table { cols, aligns } = &doc.blocks[0].kind else { panic!("not a table") };
        assert_eq!(*cols, 2);
        assert_eq!(aligns, &[HAlign::Center, HAlign::Right]);
        let cells: Vec<_> = doc.blocks[0].pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(cells, ["a", "b", "1", "2", "3", ""]);
    }

    #[test]
    fn quotes_and_alerts_have_rails() {
        let doc = parse("> one\n>\n> two\n\n> [!WARNING]\n> careful\n");
        assert_eq!(doc.blocks.len(), 4);
        assert!(doc.blocks[0].rails[0].first && !doc.blocks[0].rails[0].last);
        assert!(doc.blocks[1].rails[0].last);
        assert_eq!(doc.blocks[2].kind_role(), Some(Role::AlertTitle(AlertKind::Warning)));
        assert_eq!(doc.blocks[3].rails[0].alert, Some(AlertKind::Warning));
        assert!(doc.blocks[3].pad_bottom > 0);
    }

    #[test]
    fn html_images_alignment_and_inline_tags() {
        let doc = parse(
            "<p align=\"center\">\n  <img src=\"logo.png\" width=\"120\" alt=\"Logo\">\n</p>\n\nPress <kbd>⌘</kbd> + <b>B</b><br>now\n",
        );
        assert_eq!(doc.blocks[0].align, HAlign::Center);
        assert_eq!(doc.blocks[0].images[0].src, "logo.png");
        assert_eq!(doc.blocks[0].images[0].width, Some(Dim::Px(120)));
        assert_eq!(doc.blocks[1].pieces[0].text, "Press ⌘ + B\nnow");
        let styles: Vec<_> = doc.blocks[1].pieces[0].spans.iter().map(|s| s.style).collect();
        assert!(styles.contains(&style::KBD) && styles.contains(&style::BOLD));
    }

    #[test]
    fn images_split_paragraph_flow() {
        let doc = parse("before ![a](a.png) after\n\n[![badge](b.svg)](https://x.dev) [![c](c.svg)](https://y.dev)\n");
        assert_eq!(doc.blocks[0].flow.len(), 3);
        assert_eq!(doc.blocks[1].images.len(), 2);
        assert_eq!(doc.blocks[1].links, ["https://x.dev", "https://y.dev"]);
        assert_ne!(doc.blocks[1].images[0].link, NO_LINK);
    }

    #[test]
    fn bare_urls_become_links() {
        let doc = parse("See https://example.com/a_(b). And www.rust-lang.org, ok (http://x.io).\n");
        assert_eq!(doc.blocks[0].links, ["https://example.com/a_(b)", "https://www.rust-lang.org", "http://x.io"]);
        assert_eq!(find_urls("no links here: a.b/c"), []);
    }

    #[test]
    fn long_code_blocks_are_chunked_losslessly() {
        let body: String = (0..150).map(|i| format!("let x{i} = {i}; // n\n")).collect();
        let doc = parse(&format!("```rust\n{body}```\n"));
        assert_eq!(doc.blocks.len(), 3);
        let joined = doc.blocks.iter().map(|b| b.pieces[0].text.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(joined, body.trim_end());
        for b in &doc.blocks {
            let p = &b.pieces[0];
            assert_eq!(p.spans.last().map(|s| s.end as usize), Some(p.text.len()));
            assert!(p.spans.windows(2).all(|w| w[0].end < w[1].end));
            assert_eq!(b.group, 0);
        }
        assert!(matches!(doc.blocks[0].kind, Kind::Code { first: true, last: false, .. }));
        assert!(matches!(doc.blocks[2].kind, Kind::Code { first: false, last: true, .. }));
    }

    #[test]
    fn front_matter_and_footnotes() {
        let doc = parse("---\ntitle: x\n---\n\nText[^1].\n\n[^1]: The note.\n");
        assert!(matches!(&doc.blocks[0].kind, Kind::Code { lang, .. } if lang == "yaml"));
        assert_eq!(doc.blocks[1].links, ["#fn-1"]);
        assert_eq!(doc.blocks[2].marker, Marker::Footnote("1".into()));
        assert_eq!(doc.anchors.get("fn-1"), Some(&2));
    }

    #[test]
    fn unchanged_blocks_keep_their_hash() {
        let a = parse("# T\n\none\n\ntwo\n");
        let b = parse("# T\n\none\n\nchanged\n\ntwo\n");
        assert_eq!(a.blocks[1].hash, b.blocks[1].hash);
        assert_eq!(a.blocks[2].hash, b.blocks[3].hash);
        assert_ne!(a.blocks[2].hash, b.blocks[2].hash);
    }

    impl Block {
        fn kind_role(&self) -> Option<Role> {
            match self.kind {
                Kind::Text(role) => Some(role),
                _ => None,
            }
        }
    }
}
