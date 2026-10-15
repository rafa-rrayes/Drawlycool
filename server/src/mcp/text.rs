//! Text measurement with Excalidraw's own fonts, so labels the agent writes
//! get the same size, wrapping and position the editor would give them.
//!
//! The fonts in `assets/fonts` are the editor's bundled `.woff2` files
//! converted to TrueType (hinting stripped). Each family comes in several
//! unicode-range subsets; a character is looked up in each until one has it.

use std::sync::OnceLock;
use ttf_parser::{Face, GlyphId};

/// Excalidraw's `FONT_FAMILY` ids.
pub const EXCALIFONT: u32 = 5;
pub const NUNITO: u32 = 6;
pub const LILITA: u32 = 7;
pub const COMIC_SHANNS: u32 = 8;

/// `BOUND_TEXT_PADDING`: gap between a container's edge and its label.
pub const BOUND_TEXT_PADDING: f64 = 5.0;

macro_rules! fonts {
    ($($name:literal),* $(,)?) => {
        &[$(include_bytes!(concat!("../../assets/fonts/", $name, ".ttf"))),*]
    };
}

const EXCALIFONT_FILES: &[&[u8]] =
    fonts!["Excalifont-4", "Excalifont-6", "Excalifont-5", "Excalifont-2", "Excalifont-0", "Excalifont-1", "Excalifont-3"];
const NUNITO_FILES: &[&[u8]] = fonts!["Nunito-1", "Nunito-3", "Nunito-0", "Nunito-2", "Nunito-4"];
const COMIC_FILES: &[&[u8]] = fonts!["ComicShanns-0", "ComicShanns-3", "ComicShanns-2", "ComicShanns-1"];
const LILITA_FILES: &[&[u8]] = fonts!["Lilita-1", "Lilita-0"];
const LIBERATION_FILES: &[&[u8]] = fonts!["Liberation-0"];

pub struct Faces {
    faces: Vec<Face<'static>>,
}

impl Faces {
    fn load(files: &[&'static [u8]]) -> Self {
        Self { faces: files.iter().filter_map(|b| Face::parse(b, 0).ok()).collect() }
    }

    /// The face that has `c`, and its glyph.
    pub fn glyph(&self, c: char) -> Option<(&Face<'static>, GlyphId)> {
        self.faces.iter().find_map(|f| f.glyph_index(c).map(|g| (f, g)))
    }
}

fn faces(which: &'static OnceLock<Faces>, files: &[&'static [u8]]) -> &'static Faces {
    which.get_or_init(|| Faces::load(files))
}

static EXCALIFONT_FACES: OnceLock<Faces> = OnceLock::new();
static NUNITO_FACES: OnceLock<Faces> = OnceLock::new();
static COMIC_FACES: OnceLock<Faces> = OnceLock::new();
static LILITA_FACES: OnceLock<Faces> = OnceLock::new();
static LIBERATION_FACES: OnceLock<Faces> = OnceLock::new();

/// One of Excalidraw's font families, with the metrics the editor uses for it
/// (`FONT_METADATA`), which can differ from the font file's own.
#[derive(Clone, Copy)]
pub struct Font {
    pub family: u32,
    units_per_em: f64,
    ascender: f64,
    descender: f64,
    pub line_height: f64,
}

impl Font {
    pub fn new(family: u32) -> Self {
        let (units_per_em, ascender, descender, line_height) = match family {
            NUNITO => (1000.0, 1011.0, -353.0, 1.35),
            LILITA => (1000.0, 923.0, -220.0, 1.15),
            COMIC_SHANNS => (1000.0, 750.0, -250.0, 1.25),
            2 => (2048.0, 1577.0, -471.0, 1.15), // Helvetica
            3 => (2048.0, 1900.0, -480.0, 1.2),  // Cascadia
            9 => (2048.0, 1854.0, -434.0, 1.15), // Liberation Sans
            _ => (1000.0, 886.0, -374.0, 1.25),  // Excalifont, Virgil
        };
        Self { family, units_per_em, ascender, descender, line_height }
    }

    /// Glyph source. Legacy families are drawn with the closest bundled font.
    pub fn faces(&self) -> &'static Faces {
        match self.family {
            NUNITO => faces(&NUNITO_FACES, NUNITO_FILES),
            LILITA => faces(&LILITA_FACES, LILITA_FILES),
            COMIC_SHANNS | 3 => faces(&COMIC_FACES, COMIC_FILES),
            2 | 9 => faces(&LIBERATION_FACES, LIBERATION_FILES),
            _ => faces(&EXCALIFONT_FACES, EXCALIFONT_FILES),
        }
    }

    /// Advance width of one character at `size` px.
    pub fn advance(&self, c: char, size: f64) -> f64 {
        if c == '\t' {
            return self.advance(' ', size) * 8.0;
        }
        if let Some((face, glyph)) = self.faces().glyph(c) {
            let upm = face.units_per_em() as f64;
            let adv = face.glyph_hor_advance(glyph).unwrap_or(0) as f64;
            // Cascadia is wider than the Comic Shanns stand-in.
            let stretch = if self.family == 3 { 1.07 } else { 1.0 };
            return adv / upm * size * stretch;
        }
        if c.is_control() || is_zero_width(c) {
            0.0
        } else if is_wide(c) {
            size
        } else {
            size * 0.6
        }
    }

    pub fn line_width(&self, line: &str, size: f64) -> f64 {
        line.chars().map(|c| self.advance(c, size)).sum()
    }

    /// Width of the widest line (Excalidraw's `getTextWidth`).
    pub fn text_width(&self, text: &str, size: f64) -> f64 {
        text.split('\n').map(|l| self.line_width(l, size)).fold(0.0, f64::max)
    }

    pub fn line_height_px(&self, size: f64) -> f64 {
        size * self.line_height
    }

    /// Height of `text` (Excalidraw's `getTextHeight`).
    pub fn text_height(&self, text: &str, size: f64) -> f64 {
        self.line_height_px(size) * text.split('\n').count() as f64
    }

    /// Distance from a line box's top to its baseline (`getVerticalOffset`).
    pub fn baseline_offset(&self, size: f64) -> f64 {
        let em = size / self.units_per_em;
        let line_gap = (self.line_height_px(size) - em * self.ascender + em * self.descender) / 2.0;
        em * self.ascender + line_gap
    }

    /// Wraps `text` to `max_width` the way the editor does (`wrapText`):
    /// break at spaces, after hyphens and around CJK characters, and split
    /// words that don't fit on a line of their own.
    pub fn wrap(&self, text: &str, size: f64, max_width: f64) -> String {
        if !max_width.is_finite() || max_width < 0.0 {
            return text.to_owned();
        }
        let mut lines = Vec::new();
        for original in text.split('\n') {
            if self.line_width(original, size) <= max_width {
                lines.push(original.to_owned());
                continue;
            }
            let mut current = String::new();
            let mut current_width = 0.0;
            for token in tokens(original) {
                let token_width = self.line_width(&token, size);
                let is_space = token.chars().all(char::is_whitespace);
                if is_space || current_width + token_width <= max_width {
                    current.push_str(&token);
                    current_width += token_width;
                    continue;
                }
                if !current.trim().is_empty() {
                    lines.push(current.trim_end().to_owned());
                }
                current.clear();
                current_width = 0.0;
                // A word wider than a whole line is split by character.
                if token_width > max_width {
                    for c in token.chars() {
                        let w = self.advance(c, size);
                        if current_width + w > max_width && !current.is_empty() {
                            lines.push(std::mem::take(&mut current));
                            current_width = 0.0;
                        }
                        current.push(c);
                        current_width += w;
                    }
                } else {
                    current.push_str(&token);
                    current_width = token_width;
                }
            }
            if !current.is_empty() || lines.is_empty() {
                let trimmed = current.trim_end();
                lines.push(if self.line_width(&current, size) > max_width { trimmed.to_owned() } else { current });
            }
        }
        lines.join("\n")
    }
}

fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{FE00}'..='\u{FE0F}' | '\u{2060}' | '\u{FEFF}')
}

/// Full-width characters: CJK, kana, hangul, emoji.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF |
        0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 |
        0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF)
}

/// Break opportunities: each whitespace and CJK character is its own token;
/// words break after hyphens.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in line.chars() {
        if c.is_whitespace() || is_cjk(c) {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            out.push(c.to_string());
        } else {
            word.push(c);
            if c == '-' {
                out.push(std::mem::take(&mut word));
            }
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_with_the_bundled_fonts() {
        let font = Font::new(EXCALIFONT);
        let w = font.line_width("Hello world", 20.0);
        assert!(w > 80.0 && w < 140.0, "unexpected width {w}");
        assert_eq!(font.text_height("a\nb", 20.0), 50.0);
        assert!(Font::new(NUNITO).faces().glyph('é').is_some());
        assert!(Font::new(COMIC_SHANNS).faces().glyph('{').is_some());
    }

    #[test]
    fn wraps_like_the_editor() {
        let font = Font::new(EXCALIFONT);
        let wrapped = font.wrap("The quick brown fox jumps over the lazy dog", 20.0, 120.0);
        assert!(wrapped.lines().count() >= 3);
        for line in wrapped.lines() {
            assert!(font.line_width(line, 20.0) <= 120.0, "{line:?} is too wide");
            assert_eq!(line, line.trim());
        }
        assert_eq!(font.wrap("short", 20.0, 500.0), "short");
        let long = font.wrap("Supercalifragilisticexpialidocious", 20.0, 60.0);
        assert!(long.lines().count() > 2);
    }
}
