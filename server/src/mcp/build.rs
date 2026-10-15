//! Builds Excalidraw elements the way the editor itself would: complete
//! fields, labels wrapped and centred with the editor's rules, arrows bound
//! to shapes so they follow them when people move things around.

use super::args::Args;
use super::colors::{self, Slot};
use super::draft::{random_u32, Draft};
use super::geometry::{self as geo, Outline, Pt, Rect, Side};
use super::model;
use super::text::{self, Font, BOUND_TEXT_PADDING};
use serde_json::{json, Value};

/// `FIXED_BINDING_DISTANCE`: elbow arrows stop this far from a shape.
const ELBOW_GAP: f64 = 5.0;
/// Gap between a regular arrow's tip and the shape it points at.
const ARROW_GAP: f64 = 8.0;

#[derive(Clone, Debug)]
pub struct Style {
    pub stroke: String,
    pub fill: String,
    pub fill_style: String,
    pub stroke_width: f64,
    pub stroke_style: String,
    pub roughness: f64,
    pub opacity: f64,
    pub rounded: bool,
    pub font_family: u32,
    pub font_size: f64,
    pub text_align: Option<String>,
    pub vertical_align: Option<String>,
    pub text_color: Option<String>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            stroke: colors::BLACK.into(),
            fill: colors::TRANSPARENT.into(),
            fill_style: "solid".into(),
            stroke_width: 2.0,
            stroke_style: "solid".into(),
            roughness: 1.0,
            opacity: 100.0,
            rounded: true,
            font_family: text::EXCALIFONT,
            font_size: 20.0,
            text_align: None,
            vertical_align: None,
            text_color: None,
        }
    }
}

fn color(a: &Args, keys: &[&str], slot: Slot) -> Result<Option<String>, String> {
    match a.any(keys).and_then(Value::as_str) {
        None => Ok(None),
        Some(c) => colors::resolve(c, slot).map(Some).ok_or_else(|| {
            format!("Unknown colour “{c}”. Use a palette name (blue, light-blue, dark-blue, blue-0…blue-4, gray, red, pink, grape, violet, cyan, teal, green, yellow, orange, bronze, black, white, transparent) or a hex code.")
        }),
    }
}

pub fn font_family(name: &str) -> Option<u32> {
    match name.trim().to_ascii_lowercase().as_str() {
        "hand" | "handwritten" | "handwriting" | "excalifont" | "sketch" | "virgil" => Some(text::EXCALIFONT),
        "sans" | "normal" | "nunito" | "clean" | "sans-serif" | "helvetica" | "arial" => Some(text::NUNITO),
        "code" | "mono" | "monospace" | "comic shanns" | "cascadia" => Some(text::COMIC_SHANNS),
        "heading" | "display" | "lilita" | "lilita one" | "title" | "bold" => Some(text::LILITA),
        other => other.parse().ok().filter(|n| [1, 2, 3, 5, 6, 7, 8, 9].contains(n)),
    }
}

pub fn font_size(v: &Value) -> Option<f64> {
    if let Some(n) = super::args::as_num(v) {
        return Some(n.clamp(6.0, 400.0));
    }
    match v.as_str()?.trim().to_ascii_lowercase().as_str() {
        "xs" | "tiny" => Some(12.0),
        "s" | "small" | "sm" => Some(16.0),
        "m" | "medium" | "md" | "normal" | "regular" => Some(20.0),
        "l" | "large" | "lg" => Some(28.0),
        "xl" | "extra-large" | "heading" => Some(36.0),
        "xxl" | "2xl" | "huge" | "title" => Some(48.0),
        "3xl" | "display" => Some(64.0),
        _ => None,
    }
}

impl Style {
    /// Applies the style properties present in `a` (any spelling).
    pub fn apply(&mut self, a: &Args, closed_shape: bool) -> Result<(), String> {
        if let Some(c) = a.str("color") {
            self.stroke = colors::resolve(c, Slot::Stroke).ok_or_else(|| format!("Unknown colour “{c}”."))?;
            if closed_shape {
                self.fill = colors::resolve(c, Slot::Fill).unwrap_or_else(|| colors::TRANSPARENT.into());
            }
        }
        if let Some(c) = color(a, &["stroke", "stroke_color", "border", "line_color"], Slot::Stroke)? {
            self.stroke = c;
        }
        if let Some(c) = color(a, &["fill", "background", "background_color", "fill_color", "bg"], Slot::Fill)? {
            self.fill = c;
        }
        if let Some(c) = color(a, &["text_color", "font_color", "label_color"], Slot::Stroke)? {
            self.text_color = Some(c);
        }
        if let Some(s) = a.str("fill_style") {
            self.fill_style = match s.to_ascii_lowercase().as_str() {
                "hachure" | "hatch" | "hatched" | "sketch" => "hachure",
                "cross-hatch" | "crosshatch" | "cross_hatch" | "cross" => "cross-hatch",
                "zigzag" | "zig-zag" => "zigzag",
                _ => "solid",
            }
            .into();
        }
        if let Some(v) = a.get("stroke_width") {
            self.stroke_width = match v.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("thin" | "light" | "fine") => 1.0,
                Some("regular" | "normal" | "medium") => 2.0,
                Some("bold" | "thick" | "heavy") => 4.0,
                Some("extra-bold" | "extrabold" | "extra bold") => 6.0,
                _ => super::args::as_num(v).unwrap_or(self.stroke_width).clamp(0.5, 20.0),
            };
        }
        if let Some(s) = a.str("stroke_style") {
            self.stroke_style = match s.to_ascii_lowercase().as_str() {
                "dashed" | "dash" => "dashed",
                "dotted" | "dot" => "dotted",
                _ => "solid",
            }
            .into();
        }
        if let Some(v) = a.get("roughness") {
            self.roughness = match v.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("architect" | "clean" | "smooth" | "none") => 0.0,
                Some("artist" | "normal" | "hand-drawn") => 1.0,
                Some("cartoonist" | "sketchy" | "rough") => 2.0,
                _ => super::args::as_num(v).unwrap_or(self.roughness).clamp(0.0, 2.0).round(),
            };
        }
        if let Some(o) = a.num("opacity") {
            self.opacity = if o <= 1.0 && o > 0.0 && o.fract() != 0.0 { o * 100.0 } else { o }.clamp(0.0, 100.0).round();
        }
        if let Some(r) = a.bool("rounded") {
            self.rounded = r;
        }
        if a.bool("sharp") == Some(true) {
            self.rounded = false;
        }
        if let Some(f) = a.text("font").or_else(|| a.text("font_family")) {
            self.font_family = font_family(&f).ok_or_else(|| format!("Unknown font “{f}”. Use hand, sans, code or heading."))?;
        }
        if let Some(v) = a.get("font_size").or_else(|| a.get("size").filter(|v| v.is_string())) {
            self.font_size = font_size(v).ok_or("font_size takes a number or s, m, l, xl, xxl.")?;
        }
        if let Some(s) = a.str("text_align").or_else(|| a.str("align_text")) {
            self.text_align = Some(match s.to_ascii_lowercase().as_str() {
                "center" | "centre" | "middle" => "center",
                "right" | "end" => "right",
                _ => "left",
            }
            .into());
        }
        if let Some(s) = a.str("vertical_align") {
            self.vertical_align = Some(match s.to_ascii_lowercase().as_str() {
                "top" | "start" => "top",
                "bottom" | "end" => "bottom",
                _ => "middle",
            }
            .into());
        }
        Ok(())
    }

    pub fn font(&self) -> Font {
        Font::new(self.font_family)
    }
}

fn roundness(kind: &str, style: &Style, curved: bool) -> Value {
    match kind {
        "rectangle" | "image" | "embeddable" | "iframe" if style.rounded => json!({ "type": 3 }),
        "diamond" if style.rounded => json!({ "type": 2 }),
        "arrow" | "line" if curved => json!({ "type": 2 }),
        _ => Value::Null,
    }
}

/// An element with every common field set.
pub fn base(kind: &str, id: &str, r: Rect, style: &Style) -> Value {
    json!({
        "id": id,
        "type": kind,
        "x": round2(r.x),
        "y": round2(r.y),
        "width": round2(r.w),
        "height": round2(r.h),
        "angle": 0,
        "strokeColor": style.stroke,
        "backgroundColor": style.fill,
        "fillStyle": style.fill_style,
        "strokeWidth": style.stroke_width,
        "strokeStyle": style.stroke_style,
        "roughness": style.roughness,
        "opacity": style.opacity,
        "groupIds": [],
        "frameId": null,
        "index": null,
        "roundness": roundness(kind, style, false),
        "seed": random_u32(),
        "version": 1,
        "versionNonce": random_u32(),
        "isDeleted": false,
        "boundElements": null,
        "updated": crate::storage::now_ms(),
        "link": null,
        "locked": false,
    })
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Applies a style to an existing element (keeps its geometry).
pub fn restyle(el: &mut Value, style: &Style) {
    let kind = model::kind(el).to_owned();
    let is_text = kind == "text";
    el["strokeColor"] = (if is_text { style.text_color.clone().unwrap_or(style.stroke.clone()) } else { style.stroke.clone() }).into();
    if !is_text {
        el["backgroundColor"] = style.fill.clone().into();
    }
    el["fillStyle"] = style.fill_style.clone().into();
    el["strokeWidth"] = style.stroke_width.into();
    el["strokeStyle"] = style.stroke_style.clone().into();
    el["roughness"] = style.roughness.into();
    el["opacity"] = style.opacity.into();
    if matches!(kind.as_str(), "rectangle" | "diamond") {
        el["roundness"] = roundness(&kind, style, false);
    }
}

/// The style an existing element has (so updates keep what isn't changed).
pub fn style_of(el: &Value) -> Style {
    let mut s = Style {
        stroke: model::opt_str(el, "strokeColor").unwrap_or(colors::BLACK).to_owned(),
        fill: model::opt_str(el, "backgroundColor").unwrap_or(colors::TRANSPARENT).to_owned(),
        fill_style: model::opt_str(el, "fillStyle").unwrap_or("solid").to_owned(),
        stroke_width: model::opt_num(el, "strokeWidth").unwrap_or(2.0),
        stroke_style: model::opt_str(el, "strokeStyle").unwrap_or("solid").to_owned(),
        roughness: model::opt_num(el, "roughness").unwrap_or(1.0),
        opacity: model::opt_num(el, "opacity").unwrap_or(100.0),
        rounded: !el.get("roundness").is_none_or(Value::is_null),
        ..Style::default()
    };
    if model::kind(el) == "text" {
        s.font_family = model::opt_num(el, "fontFamily").map(|f| f as u32).unwrap_or(text::EXCALIFONT);
        s.font_size = model::opt_num(el, "fontSize").unwrap_or(20.0);
        s.text_align = model::opt_str(el, "textAlign").map(str::to_owned);
        s.vertical_align = model::opt_str(el, "verticalAlign").map(str::to_owned);
        s.text_color = Some(s.stroke.clone());
    }
    s
}

// ------------------------------------------------------------------ text

/// A text element. With a container it's a label (the caller positions it
/// with `fit_label`); otherwise `x`, `y` is its top-left corner.
pub fn text_element(id: &str, x: f64, y: f64, content: &str, style: &Style, container: Option<&str>) -> Value {
    let content = content.replace("\r\n", "\n").replace('\t', "    ");
    let font = style.font();
    let size = style.font_size;
    let mut st = style.clone();
    st.stroke = style.text_color.clone().unwrap_or_else(|| style.stroke.clone());
    st.fill = colors::TRANSPARENT.into();
    st.stroke_style = "solid".into();
    let w = font.text_width(&content, size);
    let h = font.text_height(&content, size);
    let mut el = base("text", id, Rect::new(x, y, w, h), &st);
    let default_align = if container.is_some() { "center" } else { "left" };
    let default_valign = if container.is_some() { "middle" } else { "top" };
    let obj = el.as_object_mut().expect("object");
    obj.insert("text".into(), content.clone().into());
    obj.insert("fontSize".into(), size.into());
    obj.insert("fontFamily".into(), font.family.into());
    obj.insert("textAlign".into(), style.text_align.clone().unwrap_or_else(|| default_align.into()).into());
    obj.insert("verticalAlign".into(), style.vertical_align.clone().unwrap_or_else(|| default_valign.into()).into());
    obj.insert("containerId".into(), container.map(Value::from).unwrap_or(Value::Null));
    obj.insert("originalText".into(), content.into());
    obj.insert("autoResize".into(), true.into());
    obj.insert("lineHeight".into(), font.line_height.into());
    el
}

/// Re-measures a standalone text after its content or font changed,
/// keeping its anchor (left/centre/right per alignment).
pub fn remeasure_text(el: &mut Value) {
    let font = Font::new(model::num(el, "fontFamily") as u32);
    let size = model::opt_num(el, "fontSize").unwrap_or(20.0);
    let content = model::original_text(el).to_owned();
    let (old_w, x) = (model::width(el), model::x(el));
    let w = font.text_width(&content, size);
    el["text"] = content.into();
    el["width"] = round2(w).into();
    el["height"] = round2(font.text_height(model::text_of(el), size)).into();
    el["lineHeight"] = font.line_height.into();
    match model::str_of(el, "textAlign") {
        "center" => el["x"] = round2(x + (old_w - w) / 2.0).into(),
        "right" => el["x"] = round2(x + old_w - w).into(),
        _ => {}
    }
}

/// Room a label has inside a container (`getBoundTextMaxWidth`).
pub fn label_max_width(container: &Value, font_size: f64) -> f64 {
    let w = model::width(container);
    match model::kind(container) {
        "arrow" => (0.7 * w).max(font_size * 11.0),
        "ellipse" => (w / 2.0 * std::f64::consts::SQRT_2).round() - BOUND_TEXT_PADDING * 2.0,
        "diamond" => (w / 2.0).round() - BOUND_TEXT_PADDING * 2.0,
        _ => w - BOUND_TEXT_PADDING * 2.0,
    }
}

fn label_max_height(container: &Value) -> f64 {
    let h = model::height(container);
    match model::kind(container) {
        "ellipse" => (h / 2.0 * std::f64::consts::SQRT_2).round() - BOUND_TEXT_PADDING * 2.0,
        "diamond" => (h / 2.0).round() - BOUND_TEXT_PADDING * 2.0,
        _ => h - BOUND_TEXT_PADDING * 2.0,
    }
}

/// `computeContainerDimensionForBoundText`.
fn container_dimension(text_dim: f64, kind: &str) -> f64 {
    let d = text_dim.ceil();
    let pad = BOUND_TEXT_PADDING * 2.0;
    match kind {
        "ellipse" => ((d + pad) / std::f64::consts::SQRT_2 * 2.0).round(),
        "diamond" => 2.0 * (d + pad),
        _ => d + pad,
    }
}

/// Wraps and positions a label inside its container, growing the container
/// if the label doesn't fit (like the editor). Returns the container's new
/// size when it had to grow.
pub fn fit_label(container: &mut Value, label: &mut Value) -> Option<(f64, f64)> {
    let font = Font::new(model::num(label, "fontFamily") as u32);
    let size = model::opt_num(label, "fontSize").unwrap_or(20.0);
    let kind = model::kind(container).to_owned();
    let mut grew = None;
    // Widen the shape rather than break a word in half (circles stay round).
    let longest = model::original_text(label).split_whitespace().map(|w| font.text_width(w, size)).fold(0.0, f64::max);
    if kind != "arrow" && longest > label_max_width(container, size) {
        let (w0, h0) = (model::width(container), model::height(container));
        let nw = container_dimension(longest, &kind).ceil();
        container["width"] = nw.into();
        if kind == "ellipse" && (w0 - h0).abs() < 0.5 {
            container["height"] = nw.into();
        }
        grew = Some((nw, model::height(container)));
    }
    let wrapped = font.wrap(model::original_text(label), size, label_max_width(container, size));
    let w = font.text_width(&wrapped, size);
    let h = font.text_height(&wrapped, size);
    label["text"] = wrapped.into();
    label["width"] = round2(w).into();
    label["height"] = round2(h).into();
    label["angle"] = container.get("angle").cloned().unwrap_or(0.into());
    if kind == "arrow" {
        place_arrow_label(container, label);
        return None;
    }
    if h > label_max_height(container) {
        let nh = container_dimension(h, &kind);
        container["height"] = nh.into();
        grew = Some((model::width(container), nh));
    }
    if w > label_max_width(container, size) {
        let nw = container_dimension(w, &kind);
        container["width"] = nw.into();
        grew = Some((nw, model::height(container)));
    }
    place_shape_label(container, label);
    grew
}

/// `computeBoundTextPosition` for shapes.
pub fn place_shape_label(container: &Value, label: &mut Value) {
    let (cw, ch) = (model::width(container), model::height(container));
    let (mut ox, mut oy) = (BOUND_TEXT_PADDING, BOUND_TEXT_PADDING);
    match model::kind(container) {
        "ellipse" => {
            ox += cw / 2.0 * (1.0 - std::f64::consts::SQRT_2 / 2.0);
            oy += ch / 2.0 * (1.0 - std::f64::consts::SQRT_2 / 2.0);
        }
        "diamond" => {
            ox += cw / 4.0;
            oy += ch / 4.0;
        }
        _ => {}
    }
    let (cx, cy) = (model::x(container) + ox, model::y(container) + oy);
    let size = model::opt_num(label, "fontSize").unwrap_or(20.0);
    let (max_w, max_h) = (label_max_width(container, size), label_max_height(container));
    let (w, h) = (model::width(label), model::height(label));
    let y = match model::str_of(label, "verticalAlign") {
        "top" => cy,
        "bottom" => cy + max_h - h,
        _ => cy + max_h / 2.0 - h / 2.0,
    };
    let x = match model::str_of(label, "textAlign") {
        "left" => cx,
        "right" => cx + max_w - w,
        _ => cx + max_w / 2.0 - w / 2.0,
    };
    label["x"] = round2(x).into();
    label["y"] = round2(y).into();
}

/// Where the editor puts an arrow's label: the middle point, or the middle
/// of the middle segment.
pub fn arrow_label_anchor(arrow: &Value) -> Pt {
    let pts = model::abs_points(arrow);
    if pts.is_empty() {
        return (model::x(arrow), model::y(arrow));
    }
    if pts.len() % 2 == 1 {
        return pts[pts.len() / 2];
    }
    let i = pts.len() / 2 - 1;
    let (a, b) = (pts[i], pts[i + 1]);
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

pub fn place_arrow_label(arrow: &Value, label: &mut Value) {
    let (mx, my) = arrow_label_anchor(arrow);
    label["x"] = round2(mx - model::width(label) / 2.0).into();
    label["y"] = round2(my - model::height(label) / 2.0).into();
}

/// A comfortable size for a shape holding `label`: the label wraps at
/// `max_text_width`, with generous padding, rounded to the 10px grid.
pub fn auto_size(kind: &str, label: Option<&str>, style: &Style, max_text_width: Option<f64>) -> (f64, f64) {
    let Some(label) = label.filter(|l| !l.trim().is_empty()) else {
        return match kind {
            "ellipse" => (150.0, 100.0),
            "diamond" => (160.0, 110.0),
            _ => (160.0, 80.0),
        };
    };
    let font = style.font();
    let size = style.font_size;
    let max = max_text_width.unwrap_or_else(|| (size * 11.0).max(200.0));
    let wrapped = font.wrap(label, size, max);
    let (tw, th) = (font.text_width(&wrapped, size), font.text_height(&wrapped, size));
    let up = |v: f64| (v / 10.0).ceil() * 10.0;
    let (w, h) = match kind {
        "ellipse" => (container_dimension(tw, "ellipse") + 30.0, container_dimension(th, "ellipse") + 16.0),
        "diamond" => (container_dimension(tw, "diamond") + 30.0, container_dimension(th, "diamond") + 16.0),
        _ => (tw + BOUND_TEXT_PADDING * 2.0 + size * 2.0, th + BOUND_TEXT_PADDING * 2.0 + size * 1.4),
    };
    let (min_w, min_h) = match kind {
        "ellipse" => (130.0, 80.0),
        "diamond" => (150.0, 100.0),
        _ => (size * 6.0, size * 3.0),
    };
    (up(w.max(min_w)), up(h.max(min_h)))
}

// ------------------------------------------------------------------ linear elements

/// A line or arrow through absolute points.
pub fn linear(kind: &str, id: &str, points: &[Pt], style: &Style, curved: bool) -> Value {
    let (ox, oy) = points.first().copied().unwrap_or((0.0, 0.0));
    let rel: Vec<Pt> = points.iter().map(|(x, y)| (round2(x - ox), round2(y - oy))).collect();
    let r = Rect::from_points(rel.iter().copied()).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
    let mut el = base(kind, id, Rect::new(ox, oy, r.w, r.h), style);
    let obj = el.as_object_mut().expect("object");
    obj.insert("roundness".into(), roundness(kind, style, curved));
    obj.insert("points".into(), json!(rel.iter().map(|(x, y)| [*x, *y]).collect::<Vec<_>>()));
    obj.insert("lastCommittedPoint".into(), Value::Null);
    obj.insert("startBinding".into(), Value::Null);
    obj.insert("endBinding".into(), Value::Null);
    obj.insert("startArrowhead".into(), Value::Null);
    obj.insert("endArrowhead".into(), if kind == "arrow" { "arrow".into() } else { Value::Null });
    if kind == "arrow" {
        obj.insert("elbowed".into(), false.into());
    }
    el
}

/// Replaces a linear element's points (absolute), keeping everything else.
pub fn set_points(el: &mut Value, points: &[Pt]) {
    let (ox, oy) = points.first().copied().unwrap_or((0.0, 0.0));
    let rel: Vec<Pt> = points.iter().map(|(x, y)| (round2(x - ox), round2(y - oy))).collect();
    let r = Rect::from_points(rel.iter().copied()).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
    el["x"] = round2(ox).into();
    el["y"] = round2(oy).into();
    el["width"] = round2(r.w).into();
    el["height"] = round2(r.h).into();
    el["points"] = json!(rel.iter().map(|(x, y)| [*x, *y]).collect::<Vec<_>>());
}

pub fn arrowhead(name: &str) -> Option<Value> {
    let v = match name.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
        "none" | "no" | "false" | "null" | "" => return Some(Value::Null),
        "arrow" | "default" | "true" | "yes" => "arrow",
        "triangle" | "filled" | "solid" => "triangle",
        "triangle_outline" | "hollow" | "open_triangle" => "triangle_outline",
        "dot" | "circle" | "filled_circle" => "circle",
        "circle_outline" | "hollow_circle" | "open_circle" => "circle_outline",
        "bar" | "line" | "tee" => "bar",
        "diamond" | "filled_diamond" => "diamond",
        "diamond_outline" | "hollow_diamond" | "open_diamond" => "diamond_outline",
        "crowfoot_one" | "one" => "crowfoot_one",
        "crowfoot_many" | "many" | "crowfoot" => "crowfoot_many",
        "crowfoot_one_or_many" | "one_or_many" => "crowfoot_one_or_many",
        _ => return None,
    };
    Some(v.into())
}

#[derive(Clone, Debug)]
pub enum End {
    Element(String),
    At(Pt),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    Straight,
    Elbow,
    Curved,
}

impl Route {
    pub fn parse(s: &str) -> Option<Route> {
        match s.trim().to_ascii_lowercase().as_str() {
            "straight" | "direct" | "line" => Some(Route::Straight),
            "elbow" | "elbowed" | "orthogonal" | "right-angle" | "manhattan" | "step" => Some(Route::Elbow),
            "curved" | "curve" | "round" | "smooth" | "arc" => Some(Route::Curved),
            _ => None,
        }
    }
}

pub struct Routed {
    pub points: Vec<Pt>,
    pub start: Value,
    pub end: Value,
    pub elbowed: bool,
    pub curved: bool,
}

fn target(draft: &Draft, id: &str) -> Result<(Rect, Outline), String> {
    let el = draft.get(id).ok_or_else(|| format!("No element with id “{id}” to connect to."))?;
    if model::is_linear(el) {
        return Err(format!("“{id}” is a line/arrow; arrows connect shapes, text, images or frames."));
    }
    Ok((model::rect(el), Outline::of(model::kind(el))))
}

fn dedupe(points: Vec<Pt>) -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::with_capacity(points.len());
    for p in points {
        if out.last().is_some_and(|q| geo::dist(*q, p) < 0.5) {
            continue;
        }
        // Drop the middle of three collinear axis-aligned points.
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            if ((a.0 - b.0).abs() < 0.5 && (b.0 - p.0).abs() < 0.5) || ((a.1 - b.1).abs() < 0.5 && (b.1 - p.1).abs() < 0.5) {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// An orthogonal path from `s` (leaving towards `s_side`) to `e` (arriving
/// through `e_side`).
fn elbow_path(s: Pt, s_side: Side, e: Pt, e_side: Side) -> Vec<Pt> {
    const STUB: f64 = 24.0;
    let (sn, en) = (s_side.normal(), e_side.normal());
    match (s_side.horizontal(), e_side.horizontal()) {
        (true, true) => {
            let forward = (e.0 - s.0) * sn.0 > 0.0 && (s.0 - e.0) * en.0 > 0.0;
            if forward {
                let mx = (s.0 + e.0) / 2.0;
                vec![s, (mx, s.1), (mx, e.1), e]
            } else {
                // Same side or backwards: go out, around, and in.
                let ox = if sn.0 == en.0 { if sn.0 > 0.0 { s.0.max(e.0) + STUB } else { s.0.min(e.0) - STUB } } else { s.0 + sn.0 * STUB };
                let ix = if sn.0 == en.0 { ox } else { e.0 + en.0 * STUB };
                let my = (s.1 + e.1) / 2.0;
                if sn.0 == en.0 {
                    vec![s, (ox, s.1), (ox, e.1), e]
                } else {
                    vec![s, (ox, s.1), (ox, my), (ix, my), (ix, e.1), e]
                }
            }
        }
        (false, false) => {
            let forward = (e.1 - s.1) * sn.1 > 0.0 && (s.1 - e.1) * en.1 > 0.0;
            if forward {
                let my = (s.1 + e.1) / 2.0;
                vec![s, (s.0, my), (e.0, my), e]
            } else {
                let oy = if sn.1 == en.1 { if sn.1 > 0.0 { s.1.max(e.1) + STUB } else { s.1.min(e.1) - STUB } } else { s.1 + sn.1 * STUB };
                let iy = if sn.1 == en.1 { oy } else { e.1 + en.1 * STUB };
                let mx = (s.0 + e.0) / 2.0;
                if sn.1 == en.1 {
                    vec![s, (s.0, oy), (e.0, oy), e]
                } else {
                    vec![s, (s.0, oy), (mx, oy), (mx, iy), (e.0, iy), e]
                }
            }
        }
        (true, false) => {
            // Leave sideways, arrive vertically: one corner if it lines up.
            if (e.0 - s.0) * sn.0 > 0.0 && (s.1 - e.1) * en.1 > 0.0 {
                vec![s, (e.0, s.1), e]
            } else {
                let ox = s.0 + sn.0 * STUB;
                let iy = e.1 + en.1 * STUB;
                vec![s, (ox, s.1), (ox, iy), (e.0, iy), e]
            }
        }
        (false, true) => {
            if (e.1 - s.1) * sn.1 > 0.0 && (s.0 - e.0) * en.0 > 0.0 {
                vec![s, (s.0, e.1), e]
            } else {
                let oy = s.1 + sn.1 * STUB;
                let ix = e.0 + en.0 * STUB;
                vec![s, (s.0, oy), (ix, oy), (ix, e.1), e]
            }
        }
    }
}

fn normalize_fixed(v: f64) -> f64 {
    if (v - 0.5).abs() < 1e-4 { 0.5001 } else { v }
}

/// Works out an arrow's points and bindings between two ends.
#[allow(clippy::too_many_arguments)]
pub fn route(
    draft: &Draft,
    from: &End,
    to: &End,
    style: Route,
    from_side: Option<Side>,
    to_side: Option<Side>,
    via: &[Pt],
    bow: f64,
) -> Result<Routed, String> {
    let a = match from {
        End::Element(id) => Some((id.as_str(), target(draft, id)?)),
        End::At(_) => None,
    };
    let b = match to {
        End::Element(id) => Some((id.as_str(), target(draft, id)?)),
        End::At(_) => None,
    };
    let center = |end: &End, t: &Option<(&str, (Rect, Outline))>| match (end, t) {
        (End::At(p), _) => *p,
        (_, Some((_, (r, _)))) => r.center(),
        _ => (0.0, 0.0),
    };
    let (ca, cb) = (center(from, &a), center(to, &b));
    if style == Route::Elbow {
        let ra = a.map(|(_, (r, _))| r).unwrap_or(Rect::new(ca.0, ca.1, 0.0, 0.0));
        let rb = b.map(|(_, (r, _))| r).unwrap_or(Rect::new(cb.0, cb.1, 0.0, 0.0));
        let sa = from_side.unwrap_or_else(|| geo::facing_side(&ra, &rb));
        let sb = to_side.unwrap_or_else(|| geo::facing_side(&rb, &ra));
        let s = if a.is_some() { geo::side_point(&ra, sa, ELBOW_GAP) } else { ca };
        let e = if b.is_some() { geo::side_point(&rb, sb, ELBOW_GAP) } else { cb };
        let mut points = vec![s];
        if via.is_empty() {
            points.extend(elbow_path(s, sa, e, sb).into_iter().skip(1));
        } else {
            // Through the waypoints, one corner at a time.
            let mut prev = s;
            for &p in via.iter().chain(std::iter::once(&e)) {
                if (prev.0 - p.0).abs() > 0.5 && (prev.1 - p.1).abs() > 0.5 {
                    points.push((p.0, prev.1));
                }
                points.push(p);
                prev = p;
            }
        }
        let points = dedupe(points);
        let fixed = |t: &Option<(&str, (Rect, Outline))>, p: Pt| match t {
            Some((id, (r, _))) => json!({
                "elementId": id,
                "focus": 0,
                "gap": 0,
                "fixedPoint": [
                    normalize_fixed(if r.w > 0.0 { (p.0 - r.x) / r.w } else { 0.5 }),
                    normalize_fixed(if r.h > 0.0 { (p.1 - r.y) / r.h } else { 0.5 }),
                ],
            }),
            None => Value::Null,
        };
        let (first, last) = (points[0], *points.last().unwrap_or(&points[0]));
        return Ok(Routed { start: fixed(&a, first), end: fixed(&b, last), points, elbowed: true, curved: false });
    }

    let mut inner: Vec<Pt> = via.to_vec();
    let curved = style == Route::Curved;
    if curved && inner.is_empty() {
        // One control point off the middle, bowing to the left of travel.
        let (mx, my) = ((ca.0 + cb.0) / 2.0, (ca.1 + cb.1) / 2.0);
        let (dx, dy) = (cb.0 - ca.0, cb.1 - ca.1);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let k = bow * len;
        inner.push((mx - dy / len * k, my + dx / len * k));
    }
    let toward_a = inner.first().copied().unwrap_or(cb);
    let toward_b = inner.last().copied().unwrap_or(ca);
    let side_or_boundary = |t: &Option<(&str, (Rect, Outline))>, side: Option<Side>, toward: Pt, c: Pt| match (t, side) {
        (Some((_, (r, o))), Some(sd)) => geo::side_point_at(r, *o, sd, 0.5, ARROW_GAP),
        (Some((_, (r, o))), None) => geo::boundary_point(r, *o, toward, ARROW_GAP),
        (None, _) => c,
    };
    let s = side_or_boundary(&a, from_side, toward_a, ca);
    let e = side_or_boundary(&b, to_side, toward_b, cb);
    let mut points = vec![s];
    points.extend(inner);
    points.push(e);
    let points = dedupe(points);
    let bind = |t: &Option<(&str, (Rect, Outline))>, adjacent: Pt, edge: Pt| match t {
        Some((id, (r, o))) => {
            let gap = geo::boundary_point(r, *o, edge, 0.0);
            json!({
                "elementId": id,
                "focus": round2(geo::focus(r, *o, adjacent, edge) * 1e4) / 1e4,
                "gap": round2(geo::dist(gap, edge).max(1.0)),
            })
        }
        None => Value::Null,
    };
    let n = points.len();
    let start = bind(&a, points[1.min(n - 1)], points[0]);
    let end = bind(&b, points[n.saturating_sub(2)], points[n - 1]);
    Ok(Routed { points, start, end, elbowed: false, curved: curved || n > 2 && style == Route::Curved })
}

/// A binding of an arrow end at `edge` (coming from `adjacent`) to `target`.
pub fn binding(draft: &Draft, target: &str, adjacent: Pt, edge: Pt, elbowed: bool) -> Value {
    let Some(el) = draft.get(target) else { return Value::Null };
    let (r, o) = (model::rect(el), Outline::of(model::kind(el)));
    if elbowed {
        return json!({
            "elementId": target,
            "focus": 0,
            "gap": 0,
            "fixedPoint": [
                normalize_fixed(if r.w > 0.0 { (edge.0 - r.x) / r.w } else { 0.5 }),
                normalize_fixed(if r.h > 0.0 { (edge.1 - r.y) / r.h } else { 0.5 }),
            ],
        });
    }
    let on = geo::boundary_point(&r, o, edge, 0.0);
    json!({
        "elementId": target,
        "focus": round2(geo::focus(&r, o, adjacent, edge) * 1e4) / 1e4,
        "gap": round2(geo::dist(on, edge).max(1.0)),
    })
}

/// Writes a route into an arrow element.
pub fn apply_route(el: &mut Value, r: &Routed) {
    set_points(el, &r.points);
    el["startBinding"] = r.start.clone();
    el["endBinding"] = r.end.clone();
    el["elbowed"] = r.elbowed.into();
    el["roundness"] = if r.curved { json!({ "type": 2 }) } else { Value::Null };
    if r.elbowed {
        el["fixedSegments"] = json!([]);
        el["startIsSpecial"] = false.into();
        el["endIsSpecial"] = false.into();
    } else if let Some(o) = el.as_object_mut() {
        o.remove("fixedSegments");
        o.remove("startIsSpecial");
        o.remove("endIsSpecial");
    }
}

/// Adds `{id, type}` to an element's `boundElements` (once).
pub fn add_bound(el: &mut Value, id: &str, kind: &str) {
    let mut list = el.get("boundElements").and_then(Value::as_array).cloned().unwrap_or_default();
    if !list.iter().any(|b| b.get("id").and_then(Value::as_str) == Some(id)) {
        list.push(json!({ "id": id, "type": kind }));
    }
    el["boundElements"] = Value::Array(list);
}

pub fn remove_bound(el: &mut Value, id: &str) {
    let Some(list) = el.get("boundElements").and_then(Value::as_array) else { return };
    let kept: Vec<Value> = list.iter().filter(|b| b.get("id").and_then(Value::as_str) != Some(id)).cloned().collect();
    el["boundElements"] = if kept.is_empty() { Value::Null } else { Value::Array(kept) };
}

/// Ends of an existing arrow as `End`s (bound element, or its point).
pub fn arrow_ends(el: &Value) -> (End, End) {
    let pts = model::abs_points(el);
    let first = pts.first().copied().unwrap_or((model::x(el), model::y(el)));
    let last = pts.last().copied().unwrap_or(first);
    let end = |key: &str, p: Pt| model::binding_target(el, key).map(|id| End::Element(id.to_owned())).unwrap_or(End::At(p));
    (end("startBinding", first), end("endBinding", last))
}

/// The routing style an existing arrow was drawn with.
pub fn route_of(el: &Value) -> Route {
    if el.get("elbowed").and_then(Value::as_bool) == Some(true) {
        Route::Elbow
    } else if !el.get("roundness").is_none_or(Value::is_null) && model::rel_points(el).len() > 2 {
        Route::Curved
    } else {
        Route::Straight
    }
}

/// Re-routes an arrow after the shapes it connects moved, keeping its style.
/// Bound ends follow their shapes; free ends stay put. Its label follows.
pub fn reroute(draft: &mut Draft, arrow_id: &str) {
    let Some(el) = draft.get(arrow_id) else { return };
    if !model::is_linear(el) {
        return;
    }
    let (from, to) = arrow_ends(el);
    let style = route_of(el);
    let pts = model::abs_points(el);
    // Free waypoints of a multi-point straight arrow stay where they are.
    let via: Vec<Pt> = if style == Route::Straight && pts.len() > 2 { pts[1..pts.len() - 1].to_vec() } else { Vec::new() };
    let gone = |e: &End| matches!(e, End::Element(id) if !draft.exists(id));
    if gone(&from) || gone(&to) {
        return;
    }
    let Ok(r) = route(draft, &from, &to, style, None, None, &via, 0.18) else { return };
    let label = model::bound_text_id(el);
    draft.update(arrow_id, |a| apply_route(a, &r));
    if let Some(label) = label {
        let arrow = draft.get(arrow_id).cloned();
        if let Some(arrow) = arrow {
            draft.update(&label, |t| place_arrow_label(&arrow, t));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn draft_with(els: Vec<Value>) -> Draft {
        Draft::new(els, Map::new(), None)
    }

    #[test]
    fn labels_are_centred_like_the_editor() {
        let style = Style::default();
        let mut rect = base("rectangle", "r", Rect::new(0.0, 0.0, 200.0, 80.0), &style);
        let mut label = text_element("r-label", 0.0, 0.0, "Hello", &style, Some("r"));
        assert!(fit_label(&mut rect, &mut label).is_none());
        let (lx, lw) = (model::x(&label), model::width(&label));
        assert!((lx + lw / 2.0 - 100.0).abs() < 0.01);
        assert_eq!(model::height(&label), 25.0);
        assert!((model::y(&label) - 27.5).abs() < 0.01);
    }

    #[test]
    fn long_labels_wrap_and_grow_the_container() {
        let style = Style::default();
        let mut rect = base("rectangle", "r", Rect::new(0.0, 0.0, 120.0, 40.0), &style);
        let mut label = text_element("t", 0.0, 0.0, "A rather long label that will not fit", &style, Some("r"));
        let grew = fit_label(&mut rect, &mut label).expect("grew");
        assert!(grew.1 > 40.0);
        assert!(model::text_of(&label).contains('\n'));
    }

    #[test]
    fn arrows_bind_to_both_shapes() {
        let style = Style::default();
        let a = base("rectangle", "a", Rect::new(0.0, 0.0, 100.0, 60.0), &style);
        let b = base("ellipse", "b", Rect::new(300.0, 0.0, 100.0, 60.0), &style);
        let d = draft_with(vec![a, b]);
        let r = route(&d, &End::Element("a".into()), &End::Element("b".into()), Route::Straight, None, None, &[], 0.0).unwrap();
        assert_eq!(r.points.len(), 2);
        assert!((r.points[0].0 - 108.0).abs() < 0.01 && (r.points[1].0 - 292.0).abs() < 0.01);
        assert_eq!(r.start["elementId"], "a");
        assert_eq!(r.end["focus"], 0.0);
        let e = route(&d, &End::Element("a".into()), &End::Element("b".into()), Route::Elbow, Some(Side::Bottom), Some(Side::Bottom), &[], 0.0).unwrap();
        assert!(e.elbowed && e.points.len() >= 4);
        assert!(e.points.windows(2).all(|w| (w[0].0 - w[1].0).abs() < 0.01 || (w[0].1 - w[1].1).abs() < 0.01));
    }

    #[test]
    fn auto_sizes_fit_their_labels() {
        let style = Style::default();
        for kind in ["rectangle", "ellipse", "diamond"] {
            let (w, h) = auto_size(kind, Some("Payment service"), &style, None);
            let mut shape = base(kind, "s", Rect::new(0.0, 0.0, w, h), &style);
            let mut label = text_element("t", 0.0, 0.0, "Payment service", &style, Some("s"));
            assert!(fit_label(&mut shape, &mut label).is_none(), "{kind} {w}x{h} too small");
            assert!(!model::text_of(&label).contains('\n'), "{kind} wrapped");
        }
    }
}
