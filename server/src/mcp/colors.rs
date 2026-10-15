//! Excalidraw's colour palette (open-color shades 0, 2, 4, 6, 8, as in the
//! editor's colour picker), with names an agent can use instead of hex.

/// Hue name and its five picker shades, lightest first.
pub const PALETTE: &[(&str, [&str; 5])] = &[
    ("gray", ["#f8f9fa", "#e9ecef", "#ced4da", "#868e96", "#343a40"]),
    ("red", ["#fff5f5", "#ffc9c9", "#ff8787", "#fa5252", "#e03131"]),
    ("pink", ["#fff0f6", "#fcc2d7", "#f783ac", "#e64980", "#c2255c"]),
    ("grape", ["#f8f0fc", "#eebefa", "#da77f2", "#be4bdb", "#9c36b5"]),
    ("violet", ["#f3f0ff", "#d0bfff", "#9775fa", "#7950f2", "#6741d9"]),
    ("blue", ["#e7f5ff", "#a5d8ff", "#4dabf7", "#228be6", "#1971c2"]),
    ("cyan", ["#e3fafc", "#99e9f2", "#3bc9db", "#15aabf", "#0c8599"]),
    ("teal", ["#e6fcf5", "#96f2d7", "#38d9a9", "#12b886", "#099268"]),
    ("green", ["#ebfbee", "#b2f2bb", "#69db7c", "#40c057", "#2f9e44"]),
    ("yellow", ["#fff9db", "#ffec99", "#ffd43b", "#fab005", "#f08c00"]),
    ("orange", ["#fff4e6", "#ffd8a8", "#ffa94d", "#fd7e14", "#e8590c"]),
    ("bronze", ["#f8f1ee", "#eaddd7", "#d2bab0", "#a18072", "#846358"]),
];

pub const BLACK: &str = "#1e1e1e";
pub const WHITE: &str = "#ffffff";
pub const TRANSPARENT: &str = "transparent";

/// Hues that work well side by side, in order, for automatic colouring
/// (diagram clusters, mind-map branches).
pub const SERIES: &[&str] = &["blue", "green", "orange", "violet", "red", "teal", "yellow", "grape", "cyan", "pink"];

/// Which property a colour is for; a bare hue name means a different shade
/// for each (dark strokes, soft fills).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Stroke,
    Fill,
}

impl Slot {
    fn default_shade(self) -> usize {
        match self {
            Slot::Stroke => 4,
            Slot::Fill => 1,
        }
    }
}

fn hue(name: &str) -> Option<&'static [&'static str; 5]> {
    let name = match name {
        "grey" => "gray",
        "purple" => "violet",
        "magenta" => "grape",
        "brown" => "bronze",
        "aqua" => "cyan",
        "lime" => "green",
        other => other,
    };
    PALETTE.iter().find(|(n, _)| *n == name).map(|(_, shades)| shades)
}

/// Resolves a colour: a hue (`"blue"`), `"light-blue"` / `"dark-blue"`,
/// an exact shade (`"blue-0"`…`"blue-4"`), `black`, `white`, `transparent`
/// or a hex code. Returns `None` for anything else.
pub fn resolve(input: &str, slot: Slot) -> Option<String> {
    let s = input.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix('#') {
        let ok = matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit());
        return ok.then(|| format!("#{hex}"));
    }
    match s.as_str() {
        "transparent" | "none" | "no" | "clear" => return Some(TRANSPARENT.into()),
        "black" | "ink" | "dark" => return Some(BLACK.into()),
        "white" => return Some(WHITE.into()),
        _ => {}
    }
    if let Some(name) = s.strip_prefix("light-").or_else(|| s.strip_prefix("pale-")) {
        return hue(name).map(|h| h[match slot { Slot::Stroke => 2, Slot::Fill => 0 }].to_owned());
    }
    if let Some(name) = s.strip_prefix("dark-").or_else(|| s.strip_prefix("deep-")) {
        return hue(name).map(|h| h[4].to_owned());
    }
    if let Some((name, n)) = s.rsplit_once('-') {
        if let (Some(h), Ok(n)) = (hue(name), n.parse::<usize>()) {
            return h.get(n).map(|c| (*c).to_owned());
        }
    }
    hue(&s).map(|h| h[slot.default_shade()].to_owned())
}

/// A readable name for a colour that's in the palette (`"blue"` when it's
/// the default shade for the slot, else `"blue-3"`), or the colour itself.
pub fn name(color: &str, slot: Slot) -> String {
    let c = color.trim().to_ascii_lowercase();
    match c.as_str() {
        "#1e1e1e" | "#000000" | "#000" => return "black".into(),
        "#ffffff" | "#fff" => return "white".into(),
        "transparent" => return "transparent".into(),
        _ => {}
    }
    for (hue, shades) in PALETTE {
        if let Some(i) = shades.iter().position(|s| *s == c) {
            return if i == slot.default_shade() { (*hue).to_owned() } else { format!("{hue}-{i}") };
        }
    }
    color.to_owned()
}

/// Parses `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` (and `transparent`)
/// into RGBA bytes.
pub fn rgba(color: &str) -> Option<[u8; 4]> {
    let c = color.trim();
    if c.eq_ignore_ascii_case("transparent") {
        return Some([0, 0, 0, 0]);
    }
    let hex = c.strip_prefix('#')?;
    let nib = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => Some([nib(0)?, nib(1)?, nib(2)?, 255]),
        4 => Some([nib(0)?, nib(1)?, nib(2)?, nib(3)?]),
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

/// Relative luminance, 0 (black) to 1 (white).
pub fn luminance(color: &str) -> Option<f64> {
    let [r, g, b, _] = rgba(color)?;
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    Some(0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b))
}

/// WCAG contrast ratio between two colours (1 to 21).
pub fn contrast(a: &str, b: &str) -> Option<f64> {
    let (la, lb) = (luminance(a)?, luminance(b)?);
    Some((la.max(lb) + 0.05) / (la.min(lb) + 0.05))
}

/// Ink for text drawn in a `stroke`-outlined shape filled with `fill` on a
/// `canvas` background: the stroke colour when it reads well, else the
/// darkest shade of its hue, else black.
pub fn readable_ink(stroke: &str, fill: &str, canvas: &str) -> String {
    let behind = if fill.eq_ignore_ascii_case(TRANSPARENT) { canvas } else { fill };
    let ok = |c: &str| contrast(c, behind).is_none_or(|r| r >= 3.0);
    if ok(stroke) {
        return stroke.to_owned();
    }
    let s = stroke.trim().to_ascii_lowercase();
    if let Some((_, shades)) = PALETTE.iter().find(|(_, shades)| shades.contains(&s.as_str())) {
        if ok(shades[4]) {
            return shades[4].to_owned();
        }
    }
    plain_ink(behind)
}

/// Black or white, whichever reads better on `behind`.
pub fn plain_ink(behind: &str) -> String {
    let on = |c: &str| contrast(c, behind).unwrap_or(21.0);
    if on(WHITE) > on(BLACK) { WHITE } else { BLACK }.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_to_picker_shades() {
        assert_eq!(resolve("blue", Slot::Stroke).unwrap(), "#1971c2");
        assert_eq!(resolve("blue", Slot::Fill).unwrap(), "#a5d8ff");
        assert_eq!(resolve("light-blue", Slot::Fill).unwrap(), "#e7f5ff");
        assert_eq!(resolve("Dark Green", Slot::Fill).unwrap(), "#2f9e44");
        assert_eq!(resolve("red-2", Slot::Fill).unwrap(), "#ff8787");
        assert_eq!(resolve("#ABC", Slot::Fill).unwrap(), "#abc");
        assert_eq!(resolve("none", Slot::Fill).unwrap(), "transparent");
        assert!(resolve("sparkly", Slot::Fill).is_none());
        assert_eq!(name("#a5d8ff", Slot::Fill), "blue");
        assert_eq!(name("#a5d8ff", Slot::Stroke), "blue-1");
        assert_eq!(rgba("#ff8000").unwrap(), [255, 128, 0, 255]);
    }
}
