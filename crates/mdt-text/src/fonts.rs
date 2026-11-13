//! The bundled fonts, registered with both Parley and GPUI from the same bytes, and the map
//! from a Parley font (blob id + collection index) to a GPUI `FontId`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{FontId, FontStyle, FontWeight};
use parley::fontique::Blob;
use parley::{FontContext, FontData};

/// A role, not a family name. `Body` is Inter, `Mono` Meslo LG M and `Math` New Computer
/// Modern Math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Body,
    Mono,
    Math,
}

#[derive(Debug, Clone, Copy)]
pub struct BundledFace {
    pub role: Role,
    pub family: &'static str,
    pub bold: bool,
    pub italic: bool,
    pub bytes: &'static [u8],
}

macro_rules! face {
    ($role:ident, $family:literal, $bold:literal, $italic:literal, $file:literal) => {
        BundledFace {
            role: Role::$role,
            family: $family,
            bold: $bold,
            italic: $italic,
            bytes: include_bytes!(concat!("../fonts/", $file)),
        }
    };
}

pub const BODY_FAMILY: &str = "Inter";
pub const MONO_FAMILY: &str = "Meslo LG M";
pub const MATH_FAMILY: &str = "NewComputerModern Math";

/// New Computer Modern Math, also used to typeset math (`mdt-math`).
pub static MATH_FONT: &[u8] = include_bytes!("../fonts/NewCMMath-Regular.otf");

pub static BUNDLED: [BundledFace; 9] = [
    face!(Body, "Inter", false, false, "Inter-Regular.ttf"),
    face!(Body, "Inter", false, true, "Inter-Italic.ttf"),
    face!(Body, "Inter", true, false, "Inter-Bold.ttf"),
    face!(Body, "Inter", true, true, "Inter-BoldItalic.ttf"),
    face!(Mono, "Meslo LG M", false, false, "MesloLGM-Regular.ttf"),
    face!(Mono, "Meslo LG M", false, true, "MesloLGM-Italic.ttf"),
    face!(Mono, "Meslo LG M", true, false, "MesloLGM-Bold.ttf"),
    face!(Mono, "Meslo LG M", true, true, "MesloLGM-BoldItalic.ttf"),
    BundledFace {
        role: Role::Math,
        family: MATH_FAMILY,
        bold: false,
        italic: false,
        bytes: MATH_FONT,
    },
];

impl BundledFace {
    pub fn gpui_font(&self) -> gpui::Font {
        gpui::Font {
            family: self.family.into(),
            weight: if self.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if self.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..gpui::font(self.family)
        }
    }
}

/// The GPUI side of font registration. Implemented for the app's `TextSystem` and for a
/// bare `PlatformTextSystem` (tests and benchmarks, which have no window).
pub trait GpuiFonts {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()>;
    /// The exact font, or `None`; never a silent fallback to another family.
    fn exact_font_id(&self, font: &gpui::Font) -> Option<FontId>;
    /// The font and glyph GPUI's own shaper uses for `text` set in the body font at `size`,
    /// that is, the system fallback it picks, and the advance it gives the text in ems.
    /// This reaches fonts GPUI won't load by name: on macOS it skips every font without an
    /// `m`, which rules out Apple Color Emoji.
    fn shaped_glyph(&self, _text: &str, _size: f32) -> Option<Shaped> {
        None
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Shaped {
    pub font_id: FontId,
    pub glyph: u32,
    pub advance_em: f32,
}

fn first_glyph(layout: &gpui::LineLayout, size: f32) -> Option<Shaped> {
    let run = layout.runs.first()?;
    Some(Shaped {
        font_id: run.font_id,
        glyph: run.glyphs.first()?.id.0,
        advance_em: f32::from(layout.width) / size,
    })
}

impl GpuiFonts for Arc<gpui::TextSystem> {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        gpui::TextSystem::add_fonts(self, fonts)
    }

    fn shaped_glyph(&self, text: &str, size: f32) -> Option<Shaped> {
        let run = gpui::TextRun {
            len: text.len(),
            font: BUNDLED[0].gpui_font(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let ts = gpui::WindowTextSystem::new(self.clone());
        first_glyph(&ts.layout_line(text, gpui::px(size), &[run], None), size)
    }

    fn exact_font_id(&self, font: &gpui::Font) -> Option<FontId> {
        // `resolve_font` falls back silently; check what it actually resolved to.
        let id = self.resolve_font(font);
        let got = self.get_font_for_id(id)?;
        (got.family == font.family && got.weight == font.weight && got.style == font.style)
            .then_some(id)
    }
}

impl GpuiFonts for dyn gpui::PlatformTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        gpui::PlatformTextSystem::add_fonts(self, fonts)
    }

    fn exact_font_id(&self, font: &gpui::Font) -> Option<FontId> {
        self.font_id(font).ok()
    }

    fn shaped_glyph(&self, text: &str, size: f32) -> Option<Shaped> {
        let font_id = self.font_id(&BUNDLED[0].gpui_font()).ok()?;
        let run = gpui::FontRun {
            len: text.len(),
            font_id,
        };
        first_glyph(&self.layout_line(text, gpui::px(size), &[run]), size)
    }
}

/// Parley font identity: blob id and index within a collection file.
pub type FontKey = (u64, u32);

pub fn font_key(font: &FontData) -> FontKey {
    (font.data.id(), font.index)
}

/// GPUI descriptor for a font file face: typographic family (name 16, else 1), the OS/2
/// weight class, and italic from `fsSelection`.
pub fn describe(font: &FontData) -> Option<gpui::Font> {
    use read_fonts::{FontRef, TableProvider, types::NameId};
    let f = FontRef::from_index(font.data.data(), font.index).ok()?;
    let name = f.name().ok()?;
    let lookup = |id: NameId| {
        let records = name.name_record().iter().filter(|r| r.name_id() == id);
        // Prefer Windows US English, then any record that decodes.
        let mut best: Option<(bool, String)> = None;
        for r in records {
            let Ok(s) = r.string(name.string_data()) else {
                continue;
            };
            let english = r.platform_id() == 3 && r.language_id() == 0x409;
            if best.as_ref().is_none_or(|(e, _)| english && !e) {
                best = Some((english, s.chars().collect()));
            }
        }
        best.map(|(_, s)| s)
    };
    let family = lookup(NameId::TYPOGRAPHIC_FAMILY_NAME).or_else(|| lookup(NameId::FAMILY_NAME))?;
    let (weight, italic) = match f.os2() {
        Ok(os2) => (
            os2.us_weight_class() as f32,
            os2.fs_selection().bits() & 1 != 0,
        ),
        Err(_) => (400.0, false),
    };
    Some(gpui::Font {
        family: family.into(),
        weight: FontWeight(weight),
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..gpui::font("")
    })
}

/// A character the face maps to `glyph`, from its character map.
fn char_for_glyph(font: &FontData, glyph: u32) -> Option<char> {
    use read_fonts::{FontRef, TableProvider};
    let f = FontRef::from_index(font.data.data(), font.index).ok()?;
    f.cmap()
        .ok()?
        .encoding_records()
        .iter()
        .filter_map(|r| r.subtable(f.cmap().ok()?.offset_data()).ok())
        .flat_map(|t| t.iter().collect::<Vec<_>>())
        .find(|&(_, g)| g.to_u32() == glyph)
        .and_then(|(c, _)| char::from_u32(c))
}

fn is_emoji_char(c: char) -> bool {
    use icu_properties::{CodePointSetData, props::EmojiPresentation};
    CodePointSetData::new::<EmojiPresentation>().contains(c)
}

/// Up to `n` characters the face maps that are shown as emoji by default, with their glyphs.
fn emoji_chars(font: &FontData, n: usize) -> Vec<(char, u32)> {
    use read_fonts::{FontRef, TableProvider};
    let Ok(f) = FontRef::from_index(font.data.data(), font.index) else {
        return Vec::new();
    };
    let Ok(cmap) = f.cmap() else {
        return Vec::new();
    };
    ('\u{1F300}'..='\u{1FAFF}')
        .filter(|&c| is_emoji_char(c))
        .filter_map(|c| Some((c, cmap.map_codepoint(c)?.to_u32())))
        .take(n)
        .collect()
}

/// Whether the face draws its glyphs from colour tables: bitmaps (`sbix`, Apple Color
/// Emoji; `CBDT`, Noto Color Emoji), layered vectors (`COLR`) or SVG.
fn is_colour_font(font: &FontData) -> bool {
    use read_fonts::{FontRef, types::Tag};
    FontRef::from_index(font.data.data(), font.index).is_ok_and(|f| {
        [b"sbix", b"CBDT", b"COLR", b"SVG "]
            .iter()
            .any(|t| f.table_data(Tag::new(t)).is_some())
    })
}

/// Holds Parley's font context and the Parley → GPUI font map.
pub struct FontRegistry {
    pub fcx: FontContext,
    map: HashMap<FontKey, FontId>,
    /// Fallback fonts whose glyphs are colour images (emoji).
    colour: std::collections::HashSet<FontKey>,
    /// Fallback fonts GPUI could not take. They are not tried again: each attempt reads the
    /// font's tables, scans its character map and leaks a copy of its blob.
    failed: std::collections::HashSet<FontKey>,
    /// (font size, extra advance in ems) that the platform gives emoji beyond the font's
    /// own advance; empty when there is none (see `register_fallback`).
    emoji_extra: Vec<(f32, f32)>,
    math: FontId,
    /// Bumps whenever a font is added, so cached layouts made before can be told apart.
    version: u64,
}

impl FontRegistry {
    /// Registers every bundled face with Parley and GPUI and maps each one.
    pub fn new(gpui: &(impl GpuiFonts + ?Sized)) -> anyhow::Result<Self> {
        let mut fcx = FontContext::new();
        let mut map = HashMap::new();
        let mut math = None;
        gpui.add_fonts(BUNDLED.iter().map(|f| Cow::Borrowed(f.bytes)).collect())?;
        for face in &BUNDLED {
            let blob = Blob::new(Arc::new(face.bytes));
            let id = blob.id();
            let registered = fcx.collection.register_fonts(blob, None);
            anyhow::ensure!(
                !registered.is_empty(),
                "Parley rejected the bundled {} face",
                face.family
            );
            let font_id = gpui.exact_font_id(&face.gpui_font()).ok_or_else(|| {
                anyhow::anyhow!("GPUI cannot resolve bundled font {:?}", face.gpui_font())
            })?;
            map.insert((id, 0), font_id);
            if matches!(face.role, Role::Math) {
                math = Some(font_id);
            }
        }
        Ok(Self {
            fcx,
            map,
            colour: Default::default(),
            failed: Default::default(),
            emoji_extra: Vec::new(),
            math: math.expect("a bundled math face"),
            version: 1,
        })
    }

    /// Maps every font a layout uses. A system fallback font that Parley picked is
    /// registered with GPUI the first time it appears, and the font-set version bumps. A
    /// font that fails is reported once, then skipped.
    pub fn ensure_layout_fonts<B: parley::Brush>(
        &mut self,
        layout: &parley::Layout<B>,
        gpui: &(impl GpuiFonts + ?Sized),
    ) -> anyhow::Result<()> {
        let mut first_error = None;
        for line in layout.lines() {
            for item in line.items() {
                if let parley::PositionedLayoutItem::GlyphRun(run) = item {
                    let font = run.run().font();
                    let size = run.run().font_size();
                    let glyph = run.positioned_glyphs().find(|g| g.id != 0);
                    let key = font_key(font);
                    if let Some(g) = glyph
                        && !self.map.contains_key(&key)
                        && !self.failed.contains(&key)
                        && let Err(e) = self.register_fallback(font, g.id, g.advance / size, gpui)
                    {
                        self.failed.insert(key);
                        first_error.get_or_insert(e);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// GPUI gets a fallback font by family name, weight and style read from the font's
    /// own tables, which opens the same system file Parley read. Failing that, GPUI shapes
    /// a character of the run's first glyph itself, and its choice counts if it gives the
    /// same glyph. Only then are the bytes added; GPUI's macOS loader reads only the first
    /// face of a collection, so bytes alone cannot reach faces like Hiragino's.
    ///
    /// `advance_em` is Parley's advance for that glyph. CoreText widens Apple Color Emoji
    /// beyond the font's own advances at text sizes (1.31 em at 16 pt, 1 em from 24 pt) and
    /// draws the image to match; the extra is sampled here and added as letter spacing.
    fn register_fallback(
        &mut self,
        font: &FontData,
        glyph: u32,
        advance_em: f32,
        gpui: &(impl GpuiFonts + ?Sized),
    ) -> anyhow::Result<FontId> {
        let desc =
            describe(font).ok_or_else(|| anyhow::anyhow!("cannot read fallback font tables"))?;
        // Characters GPUI's shaper may set in this font: the run's own, unless that is an
        // emoji font's `❤` (from `❤️`), which GPUI would take from the body font. Then the
        // font's other emoji: on Linux, fallback is per character, and plain symbol fonts
        // also have some of them.
        let own = char_for_glyph(font, glyph).map(|c| (c, glyph));
        let probes: Vec<(char, u32)> =
            if is_colour_font(font) && !own.is_some_and(|(c, _)| is_emoji_char(c)) {
                emoji_chars(font, 16)
            } else {
                own.into_iter().collect()
            };
        let shaped = || {
            probes.iter().find_map(|&(c, g)| {
                let s = gpui.shaped_glyph(c.encode_utf8(&mut [0; 4]), 16.0)?;
                (s.glyph == g).then_some((s.font_id, Some(c)))
            })
        };
        let (font_id, probe) = match gpui
            .exact_font_id(&desc)
            .map(|id| (id, None))
            .or_else(shaped)
        {
            Some(found) => found,
            None => {
                // Keep the (usually memory-mapped) blob alive for the process so GPUI can
                // borrow its bytes without copying them.
                let blob: &'static Blob<u8> = Box::leak(Box::new(font.data.clone()));
                gpui.add_fonts(vec![Cow::Borrowed(blob.data())])?;
                let id = gpui.exact_font_id(&desc).ok_or_else(|| {
                    anyhow::anyhow!("GPUI cannot resolve fallback font {:?}", desc.family)
                })?;
                (id, None)
            }
        };
        if is_colour_font(font) {
            self.colour.insert(font_key(font));
            // Only fonts found by shaping need this: they are the ones GPUI hands to CoreText.
            if let Some(c) = probe
                && advance_em > 0.0
            {
                let text = c.encode_utf8(&mut [0; 4]).to_owned();
                self.emoji_extra = [10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 28.0]
                    .into_iter()
                    .filter_map(|size| {
                        let s = gpui.shaped_glyph(&text, size)?;
                        Some((size, (s.advance_em - advance_em).max(0.0)))
                    })
                    .collect();
            }
        }
        self.map.insert(font_key(font), font_id);
        self.version += 1;
        Ok(font_id)
    }

    pub fn is_colour(&self, font: &FontData) -> bool {
        self.colour.contains(&font_key(font))
    }

    /// The letter spacing, in pixels, that gives an emoji at `size` the platform's advance.
    pub fn emoji_spacing(&self, size: f32) -> f32 {
        let e = &self.emoji_extra;
        let Some(i) = e.iter().position(|&(s, _)| s >= size) else {
            return e.last().map_or(0.0, |&(_, x)| x * size);
        };
        let extra = match i {
            0 => e[0].1,
            _ => {
                let ((s0, x0), (s1, x1)) = (e[i - 1], e[i]);
                x0 + (x1 - x0) * (size - s0) / (s1 - s0)
            }
        };
        extra * size
    }

    pub fn font_id(&self, font: &FontData) -> Option<FontId> {
        self.map.get(&font_key(font)).copied()
    }

    /// GPUI's id for the bundled math font, which typeset math scenes use.
    pub fn math_font_id(&self) -> FontId {
        self.math
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::layout::{BaseStyle, layout_paragraph};
    use crate::paint::Brush;

    /// Knows only the bundled families (none once `refuse` is set), and counts
    /// `add_fonts` calls.
    #[derive(Default)]
    struct BundledOnly {
        added: Cell<usize>,
        refuse: Cell<bool>,
    }

    impl GpuiFonts for BundledOnly {
        fn add_fonts(&self, _: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
            self.added.set(self.added.get() + 1);
            Ok(())
        }
        fn exact_font_id(&self, font: &gpui::Font) -> Option<FontId> {
            if self.refuse.get() {
                return None;
            }
            let i = BUNDLED
                .iter()
                .position(|f| f.family == font.family.as_ref())?;
            Some(FontId(i))
        }
    }

    /// A fallback font GPUI can't take was registered again on every layout miss: tables
    /// read, the character map scanned, a blob leaked and `add_fonts` called each time.
    #[test]
    fn a_failed_fallback_font_is_not_retried() {
        let gpui = BundledOnly::default();
        let mut fonts = FontRegistry::new(&gpui).unwrap();
        // The body font now stands in for a system fallback font that GPUI cannot take,
        // whatever fonts this machine has.
        fonts.map.clear();
        gpui.refuse.set(true);
        let after_bundled = gpui.added.get();
        let base = BaseStyle {
            size: 16.0,
            line_height: 1.5,
            mono: false,
            bold: false,
            brush: Brush(0),
        };
        let mut lcx = parley::LayoutContext::new();
        let p = layout_paragraph(&mut fonts, &mut lcx, "Body text", base, &[], &[], 400.0);
        assert!(fonts.ensure_layout_fonts(&p.layout, &gpui).is_err());
        let tried = gpui.added.get() - after_bundled;
        assert!(tried > 0);
        for _ in 0..3 {
            assert!(fonts.ensure_layout_fonts(&p.layout, &gpui).is_ok());
        }
        assert_eq!(gpui.added.get() - after_bundled, tried, "not tried again");
    }
}
