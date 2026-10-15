//! PNG previews of a scene, so an agent that can see images can check what
//! is on the canvas. Shapes, arrows, text, frames and images are drawn at
//! their exact positions, sizes and colours, without rough.js' sketchiness.

use super::colors;
use super::geometry::{dist, rotate, smooth, union_all, Pt, Rect};
use super::model::{self, opt_num, opt_str, str_of};
use super::text::{self, Font};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::f64::consts::FRAC_PI_2;
use std::rc::Rc;
use tiny_skia as sk;
use ttf_parser::OutlineBuilder;

/// Samples per segment for rounded lines and arrows.
const CURVE_STEPS: usize = 16;

pub struct RenderOptions {
    /// Scene-coordinate area to show; None = fit all given elements (+ padding).
    pub region: Option<Rect>,
    /// Longest side of the output image in pixels (default 1600). Never upscale beyond 2x scene px.
    pub max_size: u32,
    /// Draw a faint coordinate grid with scene-coordinate labels along the top and left edges.
    pub grid: bool,
    /// Padding in scene units around the content when region is None (default 40).
    pub padding: f64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { region: None, max_size: 1600, grid: false, padding: 40.0 }
    }
}

pub struct Rendered {
    pub png: Vec<u8>,
    /// Scene area shown.
    pub region: Rect,
    /// Pixels per scene unit.
    pub scale: f64,
    pub width: u32,
    pub height: u32,
}

/// Renders `elements` (non-deleted, back to front) to a PNG.
/// `files` is the scene's `files` map (fileId -> BinaryFileData) and
/// `background` the canvas colour.
pub fn render(
    elements: &[&Value],
    files: &Map<String, Value>,
    background: &str,
    opts: &RenderOptions,
) -> Result<Rendered, String> {
    let empty_fit = opts.region.is_none() && elements.is_empty();
    let max_size = opts.max_size.clamp(16, 8192);
    let max_scale = if empty_fit { 1.0 } else { 2.0 };
    let fit_scale = |r: &Rect| (max_size as f64 / r.w.max(r.h)).min(max_scale);
    let region = match opts.region {
        Some(r) => r,
        None => {
            // Frame names keep their on-screen size, so they need more room when zoomed out.
            let first = content_region(elements, opts.padding, 1.0);
            let s = fit_scale(&first);
            if s < 1.0 { content_region(elements, opts.padding, 1.0 / s) } else { first }
        }
    };
    let finite = [region.x, region.y, region.w, region.h].iter().all(|v| v.is_finite());
    if !finite || region.w <= 0.0 || region.h <= 0.0 {
        return Err(format!("the region to render must have a positive size (got {region:?})"));
    }
    let scale = fit_scale(&region);
    let width = ((region.w * scale).round() as u32).clamp(1, max_size);
    let height = ((region.h * scale).round() as u32).clamp(1, max_size);
    let region = Rect::new(region.x, region.y, width as f64 / scale, height as f64 / scale);

    let bg = parse_color(background).filter(|c| c.alpha() > 0.0).unwrap_or(sk::Color::WHITE);
    let dark = colors::luminance(background).is_some_and(|l| l < 0.35);
    let mut pm = sk::Pixmap::new(width, height).ok_or("could not allocate the image")?;
    pm.fill(bg);
    let s = scale as f32;
    let mut painter = Painter {
        pm,
        base: sk::Transform::from_row(s, 0.0, 0.0, s, (-region.x * scale) as f32, (-region.y * scale) as f32),
        scale,
        bg,
        files,
        by_id: elements.iter().map(|el| (model::id(el), *el)).collect(),
        clips: HashMap::new(),
    };
    let step = opts.grid.then(|| grid_step(&region, scale));
    if let Some(step) = step {
        painter.grid_lines(&region, step, dark);
    }
    for el in elements {
        painter.element(el);
    }
    if let Some(step) = step {
        painter.grid_labels(&region, step, dark);
    }
    let png = painter.pm.encode_png().map_err(|e| e.to_string())?;
    Ok(Rendered { png, region, scale, width, height })
}

/// Bounds of everything drawn, plus padding. Frame names are `k` times
/// their size at 100% zoom.
fn content_region(elements: &[&Value], padding: f64, k: f64) -> Rect {
    let rects = elements.iter().map(|el| {
        let b = model::bounds(el);
        if model::is_frame(el) {
            b.union(&Rect::new(b.x, b.y - 22.0 * k, b.w, 22.0 * k))
        } else {
            b
        }
    });
    let Some(r) = union_all(rects) else {
        return Rect::new(0.0, 0.0, 800.0, 500.0);
    };
    let r = r.inflate(padding.max(0.0));
    let (cx, cy) = r.center();
    let (w, h) = (r.w.max(1.0), r.h.max(1.0));
    Rect::new(cx - w / 2.0, cy - h / 2.0, w, h)
}

/// A round grid step giving at most about a dozen lines along the longer
/// side, at least ~48 px apart so the labels fit.
fn grid_step(region: &Rect, scale: f64) -> f64 {
    let longest = region.w.max(region.h);
    let mut step = 10.0;
    for i in 0.. {
        step = [1.0, 2.0, 5.0][i % 3] * 10f64.powi(i as i32 / 3 + 1);
        if longest / step <= 12.0 && step * scale >= 48.0 {
            break;
        }
    }
    step
}

/// Where and how one element is drawn.
struct Ctx {
    /// Scene → pixels, with the element's rotation.
    ts: sk::Transform,
    /// The enclosing frame's area, if the element is in one.
    clip: Option<Rc<sk::Mask>>,
    opacity: f32,
}

/// An element's stroke and fill.
struct Style<'e> {
    stroke: Option<sk::Color>,
    fill: Option<sk::Color>,
    fill_style: &'e str,
    /// `strokeWidth` as set.
    sw: f32,
    /// Width actually drawn (dashed and dotted lines are a little bolder).
    width: f32,
    dash: Option<[f32; 2]>,
    dotted: bool,
}

impl<'e> Style<'e> {
    fn of(el: &'e Value, opacity: f32) -> Self {
        let sw = opt_num(el, "strokeWidth").unwrap_or(2.0).max(0.0) as f32;
        let style = str_of(el, "strokeStyle");
        let dash = match style {
            "dashed" => Some([8.0, 8.0 + sw]),
            "dotted" => Some([1.5, 6.0 + sw]),
            _ => None,
        };
        let stroke = opt_str(el, "strokeColor").unwrap_or(colors::BLACK);
        Self {
            stroke: paint_color(stroke, opacity, true).filter(|_| sw > 0.0),
            fill: paint_color(str_of(el, "backgroundColor"), opacity, false),
            fill_style: str_of(el, "fillStyle"),
            sw,
            width: if dash.is_some() { sw + 0.5 } else { sw },
            dash,
            dotted: style == "dotted",
        }
    }
}

fn parse_color(s: &str) -> Option<sk::Color> {
    let [r, g, b, a] = colors::rgba(s).or_else(|| named_color(s))?;
    Some(sk::Color::from_rgba8(r, g, b, a))
}

/// A few CSS colour names that show up in hand-written scenes.
fn named_color(s: &str) -> Option<[u8; 4]> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "black" => [0, 0, 0, 255],
        "white" => [255, 255, 255, 255],
        "red" => [255, 0, 0, 255],
        "green" => [0, 128, 0, 255],
        "blue" => [0, 0, 255, 255],
        "yellow" => [255, 255, 0, 255],
        "orange" => [255, 165, 0, 255],
        "purple" => [128, 0, 128, 255],
        "gray" | "grey" => [128, 128, 128, 255],
        _ => return None,
    })
}

/// A colour with the element's opacity, or None when invisible. Strokes
/// fall back to black when the colour can't be read; fills to none.
fn paint_color(s: &str, opacity: f32, stroke: bool) -> Option<sk::Color> {
    let mut c = match parse_color(s) {
        Some(c) => c,
        None if stroke && !s.trim().eq_ignore_ascii_case("transparent") => sk::Color::from_rgba8(0x1e, 0x1e, 0x1e, 255),
        None => return None,
    };
    c.apply_opacity(opacity);
    (c.alpha() > 0.0).then_some(c)
}

fn paint(color: sk::Color) -> sk::Paint<'static> {
    let mut p = sk::Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

fn f(p: Pt) -> (f32, f32) {
    (p.0 as f32, p.1 as f32)
}

fn polyline(points: &[Pt], close: bool) -> Option<sk::Path> {
    let mut pb = sk::PathBuilder::new();
    let (first, rest) = points.split_first()?;
    pb.move_to(first.0 as f32, first.1 as f32);
    for p in rest {
        pb.line_to(p.0 as f32, p.1 as f32);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

fn segment(a: Pt, b: Pt) -> Option<sk::Path> {
    polyline(&[a, b], false)
}

/// A rectangle with corners rounded by `r`, drawn like the editor (quadratic corners).
fn rect_path(r: Rect, radius: f64) -> Option<sk::Path> {
    let (x, y, w, h) = (r.x.min(r.right()), r.y.min(r.bottom()), r.w.abs(), r.h.abs());
    let rad = radius.min(w / 2.0).min(h / 2.0);
    if rad <= 0.0 {
        return sk::Rect::from_xywh(x as f32, y as f32, w as f32, h as f32).map(sk::PathBuilder::from_rect);
    }
    let (x, y, w, h, r) = (x as f32, y as f32, w as f32, h as f32, rad as f32);
    let mut pb = sk::PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Excalidraw's `getCornerRadius` for a side of length `x`.
fn corner_radius(el: &Value, x: f64) -> f64 {
    let Some(roundness) = el.get("roundness").filter(|r| r.is_object()) else {
        return 0.0;
    };
    match roundness.get("type").and_then(Value::as_u64) {
        Some(1 | 2) => x * 0.25,
        Some(3) => {
            let fixed = roundness.get("value").and_then(Value::as_f64).unwrap_or(32.0);
            if x <= fixed / 0.25 { x * 0.25 } else { fixed }
        }
        _ => 0.0,
    }
}

fn has_roundness(el: &Value) -> bool {
    el.get("roundness").is_some_and(|r| !r.is_null())
}

/// The editor's diamond (`getDiamondPoints`), with rounded corners when set.
fn diamond_path(el: &Value) -> Option<sk::Path> {
    let r = model::rect(el);
    let (top, right) = ((r.w / 2.0).floor() + 1.0, (r.h / 2.0).floor() + 1.0);
    let p = |px: f64, py: f64| ((r.x + px) as f32, (r.y + py) as f32);
    let ((tx, ty), (rx, ry), (bx, by), (lx, ly)) = (p(top, 0.0), p(r.w, right), p(top, r.h), p(0.0, right));
    let mut pb = sk::PathBuilder::new();
    if has_roundness(el) {
        let v = corner_radius(el, top.abs()) as f32;
        let h = corner_radius(el, right.abs()) as f32;
        pb.move_to(tx + v, ty + h);
        pb.line_to(rx - v, ry - h);
        pb.cubic_to(rx, ry, rx, ry, rx - v, ry + h);
        pb.line_to(bx + v, by - h);
        pb.cubic_to(bx, by, bx, by, bx - v, by - h);
        pb.line_to(lx + v, ly + h);
        pb.cubic_to(lx, ly, lx, ly, lx + v, ly - h);
        pb.line_to(tx - v, ty + h);
        pb.cubic_to(tx, ty, tx, ty, tx + v, ty + h);
    } else {
        pb.move_to(tx, ty);
        pb.line_to(rx, ry);
        pb.line_to(bx, by);
        pb.line_to(lx, ly);
    }
    pb.close();
    pb.finish()
}

/// Whether a line's ends meet (`isPathALoop`), making it a fillable shape.
fn is_loop(points: &[Pt]) -> bool {
    points.len() >= 3 && dist(points[0], points[points.len() - 1]) <= 8.0
}

/// Draws glyph outlines into a path builder: font units → scene units,
/// y flipped, from a pen position on the baseline.
struct Outline<'p> {
    pb: &'p mut sk::PathBuilder,
    x: f64,
    y: f64,
    sx: f64,
    sy: f64,
}

impl Outline<'_> {
    fn pt(&self, x: f32, y: f32) -> (f32, f32) {
        ((self.x + x as f64 * self.sx) as f32, (self.y - y as f64 * self.sy) as f32)
    }
}

impl OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.pt(x, y);
        self.pb.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.pt(x, y);
        self.pb.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let ((x1, y1), (x, y)) = (self.pt(x1, y1), self.pt(x, y));
        self.pb.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let ((x1, y1), (x2, y2), (x, y)) = (self.pt(x1, y1), self.pt(x2, y2), self.pt(x, y));
        self.pb.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

/// Adds the outlines of one line of text starting at `x` on baseline `y`.
/// Glyphs advance by `Font::advance`, so they land where the editor's
/// measurement puts them.
fn glyph_run(pb: &mut sk::PathBuilder, font: &Font, line: &str, size: f64, mut x: f64, y: f64) {
    for c in line.chars() {
        let advance = font.advance(c, size);
        if let Some((face, glyph)) = font.glyph(c) {
            let s = size / face.units_per_em() as f64;
            let own = face.glyph_hor_advance(glyph).unwrap_or(0) as f64 * s;
            let sx = if own > 0.0 { s * advance / own } else { s };
            face.outline_glyph(glyph, &mut Outline { pb: &mut *pb, x, y, sx, sy: s });
        }
        x += advance;
    }
}

/// `text` cut with an ellipsis to fit `max_width`.
fn truncate(font: &Font, text: &str, size: f64, max_width: f64) -> String {
    if font.line_width(text, size) <= max_width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    for i in (1..chars.len()).rev() {
        let cut: String = chars[..i].iter().collect::<String>() + "...";
        if font.line_width(&cut, size) <= max_width {
            return cut;
        }
    }
    String::new()
}

struct Painter<'a> {
    pm: sk::Pixmap,
    /// Scene → pixels.
    base: sk::Transform,
    scale: f64,
    bg: sk::Color,
    files: &'a Map<String, Value>,
    by_id: HashMap<&'a str, &'a Value>,
    /// Clip masks of frames, by frame id.
    clips: HashMap<String, Option<Rc<sk::Mask>>>,
}

impl Painter<'_> {
    fn fill(&mut self, path: &sk::Path, color: sk::Color, cx: &Ctx) {
        self.pm.fill_path(path, &paint(color), sk::FillRule::Winding, cx.ts, cx.clip.as_deref());
    }

    fn stroke(&mut self, path: &sk::Path, color: sk::Color, width: f32, dash: Option<[f32; 2]>, cx: &Ctx) {
        if width <= 0.0 {
            return;
        }
        let mut stroke =
            sk::Stroke { width, line_cap: sk::LineCap::Round, line_join: sk::LineJoin::Round, ..Default::default() };
        if let Some([on, off]) = dash {
            stroke.dash = sk::StrokeDash::new(vec![on, off], 0.0);
        }
        self.pm.stroke_path(path, &paint(color), &stroke, cx.ts, cx.clip.as_deref());
    }

    /// Fills a closed shape in the element's fill style.
    fn fill_shape(&mut self, path: &sk::Path, st: &Style, cx: &Ctx) {
        let Some(color) = st.fill else { return };
        match st.fill_style {
            "solid" => self.fill(path, color, cx),
            "cross-hatch" => self.hatch(path, color, st.sw, true, cx),
            _ => self.hatch(path, color, st.sw, false, cx),
        }
    }

    /// rough.js-style hachure: parallel lines at -41° (`hachureAngle`),
    /// `strokeWidth * 4` apart and half as thick, clipped to the shape.
    fn hatch(&mut self, shape: &sk::Path, color: sk::Color, sw: f32, cross: bool, cx: &Ctx) {
        let mask = match &cx.clip {
            Some(clip) => {
                let mut m = (**clip).clone();
                m.intersect_path(shape, sk::FillRule::Winding, true, cx.ts);
                Some(m)
            }
            None => sk::Mask::new(self.pm.width(), self.pm.height()).map(|mut m| {
                m.fill_path(shape, sk::FillRule::Winding, true, cx.ts);
                m
            }),
        };
        let Some(mask) = mask else { return };
        let px = 1.0 / self.scale;
        let sw = sw.max(1.0) as f64;
        let gap = (sw * 4.0).max(3.0 * px);
        let b = shape.bounds();
        let (cxp, cyp) = ((b.left() + b.right()) as f64 / 2.0, (b.top() + b.bottom()) as f64 / 2.0);
        let reach = ((b.width() as f64).hypot(b.height() as f64) / 2.0) + gap;
        let a = 49f64.to_radians();
        let mut dirs = vec![(a.cos(), -a.sin())];
        if cross {
            dirs.push((a.sin(), a.cos()));
        }
        let mut pb = sk::PathBuilder::new();
        for (dx, dy) in dirs {
            let (nx, ny) = (-dy, dx);
            let n = (reach / gap).ceil() as i64;
            for k in -n..=n {
                let o = k as f64 * gap;
                let (ox, oy) = (cxp + nx * o, cyp + ny * o);
                pb.move_to((ox - dx * reach) as f32, (oy - dy * reach) as f32);
                pb.line_to((ox + dx * reach) as f32, (oy + dy * reach) as f32);
            }
        }
        let Some(lines) = pb.finish() else { return };
        let stroke = sk::Stroke { width: (sw / 2.0).max(0.6 * px) as f32, ..Default::default() };
        self.pm.stroke_path(&lines, &paint(color), &stroke, cx.ts, Some(&mask));
    }

    /// Fill, then outline.
    fn shape(&mut self, path: Option<sk::Path>, st: &Style, cx: &Ctx) {
        let Some(path) = path else { return };
        self.fill_shape(&path, st, cx);
        if let Some(color) = st.stroke {
            self.stroke(&path, color, st.width, st.dash, cx);
        }
    }

    /// The clip mask for children of frame `id`.
    fn frame_clip(&mut self, id: &str) -> Option<Rc<sk::Mask>> {
        if let Some(m) = self.clips.get(id) {
            return m.clone();
        }
        let mask = self.by_id.get(id).copied().filter(|f| model::is_frame(f)).and_then(|frame| {
            let path = rect_path(model::rect(frame), 0.0)?;
            let mut m = sk::Mask::new(self.pm.width(), self.pm.height())?;
            m.fill_path(&path, sk::FillRule::Winding, true, self.base);
            Some(Rc::new(m))
        });
        self.clips.insert(id.to_owned(), mask.clone());
        mask
    }

    fn element(&mut self, el: &Value) {
        let opacity = (opt_num(el, "opacity").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0) as f32;
        if opacity <= 0.0 {
            return;
        }
        let center = if model::has_points(el) {
            let (x, y) = (model::x(el), model::y(el));
            Rect::from_points(model::rel_points(el).into_iter().map(|(px, py)| (x + px, y + py)))
                .map(|r| r.center())
                .unwrap_or((x, y))
        } else {
            model::rect(el).center()
        };
        let angle = model::angle(el);
        let mut ts = self.base;
        if angle != 0.0 {
            ts = ts.pre_concat(sk::Transform::from_rotate_at(
                angle.to_degrees() as f32,
                center.0 as f32,
                center.1 as f32,
            ));
        }
        let clip = model::frame_id(el).and_then(|id| self.frame_clip(id));
        let cx = Ctx { ts, clip, opacity };
        match model::kind(el) {
            "rectangle" => {
                let r = model::rect(el);
                let path = rect_path(r, corner_radius(el, r.w.abs().min(r.h.abs())));
                self.shape(path, &Style::of(el, opacity), &cx);
            }
            "ellipse" => {
                let r = model::rect(el);
                let oval = sk::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32);
                self.shape(oval.and_then(sk::PathBuilder::from_oval), &Style::of(el, opacity), &cx);
            }
            "diamond" => self.shape(diamond_path(el), &Style::of(el, opacity), &cx),
            "line" | "arrow" => self.linear(el, &cx),
            "freedraw" => self.freedraw(el, &cx),
            "text" => self.text(el, &cx),
            "frame" | "magicframe" => self.frame(el, &cx),
            "image" => self.image(el, &cx),
            "embeddable" | "iframe" => self.embed(el, &cx),
            _ => {}
        }
    }

    fn linear(&mut self, el: &Value, cx: &Ctx) {
        let (ox, oy) = (model::x(el), model::y(el));
        let pts: Vec<Pt> = model::rel_points(el).into_iter().map(|(x, y)| (ox + x, oy + y)).collect();
        if pts.len() < 2 {
            return;
        }
        let st = Style::of(el, cx.opacity);
        let elbowed = el.get("elbowed").and_then(Value::as_bool).unwrap_or(false);
        let curved = !elbowed && pts.len() >= 3 && has_roundness(el);
        let path_pts = if curved { smooth(&pts, CURVE_STEPS) } else { pts.clone() };
        if model::kind(el) == "line" && is_loop(&pts) {
            if let Some(poly) = polyline(&path_pts, true) {
                self.fill_shape(&poly, &st, cx);
            }
        }
        let Some(color) = st.stroke else { return };
        if let Some(path) = polyline(&path_pts, false) {
            self.stroke(&path, color, st.width, st.dash, cx);
        }
        for (key, at_end) in [("startArrowhead", false), ("endArrowhead", true)] {
            if let Some(head) = opt_str(el, key) {
                self.arrowhead(head, &pts, &path_pts, at_end, curved, &st, color, cx);
            }
        }
    }

    /// Arrowheads as the editor computes them (`getArrowheadPoints`).
    #[allow(clippy::too_many_arguments)]
    fn arrowhead(
        &mut self,
        head: &str,
        pts: &[Pt],
        path_pts: &[Pt],
        at_end: bool,
        curved: bool,
        st: &Style,
        color: sk::Color,
        cx: &Ctx,
    ) {
        let n = pts.len();
        let (tip, prev) = if at_end { (pts[n - 1], pts[n - 2]) } else { (pts[0], pts[1]) };
        // Direction: from a point 30% back along the last curve segment.
        let from = if curved {
            let k = (CURVE_STEPS as f64 * 0.3).round() as usize;
            if at_end { path_pts[path_pts.len() - 1 - k] } else { path_pts[k] }
        } else {
            prev
        };
        let d = dist(tip, from);
        let length = dist(tip, prev);
        if d < 1e-9 || length < 1e-9 {
            return;
        }
        let (nx, ny) = ((tip.0 - from.0) / d, (tip.1 - from.1) / d);
        let diamond = matches!(head, "diamond" | "diamond_outline");
        let size = match head {
            "arrow" => 25.0,
            "diamond" | "diamond_outline" => 12.0,
            "crowfoot_many" | "crowfoot_one" | "crowfoot_one_or_many" => 20.0,
            _ => 15.0,
        };
        let min_size = f64::min(size, length * if diamond { 0.25 } else { 0.5 });
        let back = (tip.0 - nx * min_size, tip.1 - ny * min_size);
        let angle = match head {
            "bar" => 90f64,
            "arrow" => 20.0,
            _ => 25.0,
        }
        .to_radians();
        let side = |sign: f64| rotate(back, tip, sign * angle);
        let outline_fill = {
            let mut c = self.bg;
            c.apply_opacity(cx.opacity);
            c
        };
        let solid = |me: &mut Self, path: Option<sk::Path>, fill: sk::Color| {
            if let Some(path) = path {
                me.fill(&path, fill, cx);
                me.stroke(&path, color, st.sw, None, cx);
            }
        };
        match head {
            "dot" | "circle" | "circle_outline" => {
                let r = ((min_size + st.sw as f64 - 2.0) / 2.0).max(0.5);
                let fill = if head == "circle_outline" { outline_fill } else { color };
                solid(self, sk::PathBuilder::from_circle(tip.0 as f32, tip.1 as f32, r as f32), fill);
            }
            "triangle" | "triangle_outline" => {
                let fill = if head == "triangle_outline" { outline_fill } else { color };
                solid(self, polyline(&[tip, side(-1.0), side(1.0)], true), fill);
            }
            "diamond" | "diamond_outline" => {
                let far = (tip.0 + (prev.0 - tip.0) / length * min_size * 2.0, tip.1 + (prev.1 - tip.1) / length * min_size * 2.0);
                let fill = if head == "diamond_outline" { outline_fill } else { color };
                solid(self, polyline(&[tip, side(-1.0), far, side(1.0)], true), fill);
            }
            "crowfoot_one" => {
                if let Some(p) = segment(side(-1.0), side(1.0)) {
                    self.stroke(&p, color, st.sw, None, cx);
                }
            }
            _ => {
                // arrow, bar and crow's feet are open strokes.
                let lines = if head.starts_with("crowfoot") {
                    let foot = |sign: f64| rotate(tip, back, sign * angle);
                    let mut l = vec![(foot(-1.0), back), (foot(1.0), back)];
                    if head == "crowfoot_one_or_many" {
                        l.push((side(-1.0), side(1.0)));
                    }
                    l
                } else {
                    vec![(side(-1.0), tip), (side(1.0), tip)]
                };
                let dash = st.dotted.then_some([1.5, 4.0 + st.sw]);
                for (a, b) in lines {
                    if let Some(p) = segment(a, b) {
                        self.stroke(&p, color, st.width, dash, cx);
                    }
                }
            }
        }
    }

    /// perfect-freehand strokes (size `strokeWidth * 4.25`, thinning 0.6,
    /// sine easing), drawn at their average width.
    fn freedraw(&mut self, el: &Value, cx: &Ctx) {
        let (ox, oy) = (model::x(el), model::y(el));
        let pts: Vec<Pt> = model::rel_points(el).into_iter().map(|(x, y)| (ox + x, oy + y)).collect();
        if pts.is_empty() {
            return;
        }
        let st = Style::of(el, cx.opacity);
        if is_loop(&pts) {
            if let Some(poly) = polyline(&pts, true) {
                self.fill_shape(&poly, &st, cx);
            }
        }
        let Some(color) = st.stroke else { return };
        let size = st.sw as f64 * 4.25;
        let simulate = el.get("simulatePressure").and_then(Value::as_bool).unwrap_or(true);
        let given: Vec<f64> = el
            .get("pressures")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        let pressures: Vec<f64> = if !simulate && !given.is_empty() {
            given
        } else {
            // perfect-freehand's simulated pressure: faster strokes are thinner.
            let mut p = 0.5;
            std::iter::once(p)
                .chain(pts.windows(2).map(|w| {
                    let s = (dist(w[0], w[1]) / size.max(1e-9)).min(1.0);
                    p = (p + ((1.0 - s) - p) * (s * 0.275)).min(1.0);
                    p
                }))
                .collect()
        };
        let radius = |p: f64| size * (FRAC_PI_2 * (0.5 - 0.6 * (0.5 - p.clamp(0.0, 1.0)))).sin();
        let width = pressures.iter().map(|&p| radius(p) * 2.0).sum::<f64>() / pressures.len() as f64;
        let mut path_pts = smooth(&pts, 4);
        if path_pts.len() == 1 {
            path_pts.push((pts[0].0 + 0.01, pts[0].1));
        }
        if let Some(path) = polyline(&path_pts, false) {
            self.stroke(&path, color, width as f32, None, cx);
        }
    }

    fn text(&mut self, el: &Value, cx: &Ctx) {
        let r = model::rect(el);
        // Arrow labels cut the arrow line behind them.
        let on_arrow = model::container_id(el).and_then(|id| self.by_id.get(id)).is_some_and(|c| model::kind(c) == "arrow");
        if on_arrow {
            if let Some(knockout) = rect_path(r, 0.0) {
                let bg = self.bg;
                self.fill(&knockout, bg, cx);
            }
        }
        let Some(color) = paint_color(str_of(el, "strokeColor"), cx.opacity, true) else { return };
        let size = opt_num(el, "fontSize").unwrap_or(20.0);
        if size <= 0.0 {
            return;
        }
        let mut font = Font::new(opt_num(el, "fontFamily").map(|f| f as u32).unwrap_or(text::EXCALIFONT));
        if let Some(lh) = opt_num(el, "lineHeight").filter(|v| *v > 0.0) {
            font.line_height = lh;
        }
        let align = str_of(el, "textAlign");
        let mut pb = sk::PathBuilder::new();
        for (i, line) in model::text_of(el).split('\n').enumerate() {
            let lw = font.line_width(line, size);
            let x = match align {
                "center" => r.x + (r.w - lw) / 2.0,
                "right" => r.x + r.w - lw,
                _ => r.x,
            };
            let baseline = r.y + i as f64 * font.line_height_px(size) + font.baseline_offset(size);
            glyph_run(&mut pb, &font, line, size, x, baseline);
        }
        if let Some(path) = pb.finish() {
            self.fill(&path, color, cx);
        }
    }

    /// One line of text with its box's top-left at `(x, y)`.
    fn label(&mut self, text: &str, family: u32, size: f64, (x, y): Pt, color: sk::Color, cx: &Ctx) {
        let font = Font::new(family);
        let mut pb = sk::PathBuilder::new();
        glyph_run(&mut pb, &font, text, size, x, y + font.baseline_offset(size));
        if let Some(path) = pb.finish() {
            self.fill(&path, color, cx);
        }
    }

    /// Frames keep a constant on-screen outline and name, like the editor;
    /// the name is laid out as in exports (Helvetica 14, 3 px above).
    fn frame(&mut self, el: &Value, cx: &Ctx) {
        let r = model::rect(el);
        let k = 1.0 / self.scale.min(1.0);
        let gray = |v: u8| {
            let mut c = sk::Color::from_rgba8(v, v, v, 255);
            c.apply_opacity(cx.opacity);
            c
        };
        if let Some(path) = rect_path(r, 8.0 * k) {
            self.stroke(&path, gray(0xbb), (2.0 * k) as f32, None, cx);
        }
        let default = if model::kind(el) == "magicframe" { "AI Frame" } else { "Frame" };
        let name = opt_str(el, "name").unwrap_or(default);
        let (family, size) = (2, 14.0 * k);
        let font = Font::new(family);
        let name = truncate(&font, name, size, r.w);
        let top = r.y - 3.0 * k - font.line_height_px(size);
        self.label(&name, family, size, (r.x, top), gray(0x99), cx);
    }

    fn image(&mut self, el: &Value, cx: &Ctx) {
        let r = model::rect(el);
        let data = opt_str(el, "fileId").and_then(|id| self.files.get(id)).map(|file| str_of(file, "dataURL"));
        let Some(img) = data.and_then(decode_png_data_url) else {
            return self.placeholder(r, "image", cx);
        };
        // `scale` flips the image around its centre.
        let flip = el.get("scale").and_then(Value::as_array).map(|s| {
            let v = |i: usize| s.get(i).and_then(Value::as_f64).unwrap_or(1.0).signum() as f32;
            (v(0), v(1))
        });
        let mut ts = cx.ts;
        if let Some((fx, fy)) = flip.filter(|&(fx, fy)| fx < 0.0 || fy < 0.0) {
            let (mx, my) = f(r.center());
            ts = ts.pre_translate(mx, my).pre_scale(fx, fy).pre_translate(-mx, -my);
        }
        // `crop` is in the image's natural pixels.
        let (iw, ih) = (img.width() as f64, img.height() as f64);
        let crop = el.get("crop").filter(|c| c.is_object());
        let natural = |key: &str, fallback: f64| crop.and_then(|c| opt_num(c, key)).filter(|v| *v > 0.0).unwrap_or(fallback);
        let (nw, nh) = (natural("naturalWidth", iw), natural("naturalHeight", ih));
        let (cx0, cy0) = (crop.map_or(0.0, |c| model::num(c, "x")), crop.map_or(0.0, |c| model::num(c, "y")));
        let (cw, ch) = (natural("width", nw), natural("height", nh));
        let (sx, sy) = (r.w / cw * nw / iw, r.h / ch * nh / ih);
        let pattern_ts = sk::Transform::from_row(
            sx as f32,
            0.0,
            0.0,
            sy as f32,
            (r.x - cx0 * r.w / cw) as f32,
            (r.y - cy0 * r.h / ch) as f32,
        );
        let shader = sk::Pattern::new(img.as_ref(), sk::SpreadMode::Pad, sk::FilterQuality::Bicubic, cx.opacity, pattern_ts);
        let p = sk::Paint { shader, anti_alias: true, ..Default::default() };
        if let Some(rect) = sk::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32) {
            self.pm.fill_rect(rect, &p, ts, cx.clip.as_deref());
        }
    }

    /// A gray box with a cross and a caption, for content that can't be shown.
    fn placeholder(&mut self, r: Rect, caption: &str, cx: &Ctx) {
        let color = |hex: &str| paint_color(hex, cx.opacity, false);
        let (Some(fill), Some(line), Some(ink)) = (color("#e9ecef"), color("#adb5bd"), color("#868e96")) else { return };
        let Some(path) = rect_path(r, 0.0) else { return };
        let w = (1.5 / self.scale) as f32;
        self.fill(&path, fill, cx);
        self.stroke(&path, line, w, None, cx);
        for (a, b) in [((r.x, r.y), (r.right(), r.bottom())), ((r.right(), r.y), (r.x, r.bottom()))] {
            if let Some(p) = segment(a, b) {
                self.stroke(&p, line, w, None, cx);
            }
        }
        self.caption(r, caption, ink, cx);
    }

    /// Centred one-line text inside `r`, on a light backing.
    fn caption(&mut self, r: Rect, text: &str, color: sk::Color, cx: &Ctx) {
        let font = Font::new(text::NUNITO);
        // About a fifth of the box, but readable (>= 10 px) when zoomed out.
        let min = 10.0 / self.scale;
        let size = (r.h.abs() / 5.0).clamp(min, min.max(20.0)).min(r.h.abs() * 0.8);
        let text = truncate(&font, text, size, r.w.abs() * 0.9);
        if text.is_empty() || size <= 0.0 {
            return;
        }
        let (w, h) = (font.line_width(&text, size), font.line_height_px(size));
        let (mx, my) = r.center();
        if let Some(backing) = rect_path(Rect::new(mx - w / 2.0 - 4.0, my - h / 2.0, w + 8.0, h), 0.0) {
            let mut c = self.bg;
            c.apply_opacity(cx.opacity * 0.85);
            self.fill(&backing, c, cx);
        }
        self.label(&text, text::NUNITO, size, (mx - w / 2.0, my - h / 2.0), color, cx);
    }

    fn embed(&mut self, el: &Value, cx: &Ctx) {
        let r = model::rect(el);
        let mut st = Style::of(el, cx.opacity);
        if st.fill.is_none() {
            st.fill = paint_color("#f1f3f5", cx.opacity, false);
            st.fill_style = "solid";
        }
        if st.stroke.is_none() {
            st.stroke = paint_color("#ced4da", cx.opacity, false);
            st.width = st.sw.max(1.0);
        }
        self.shape(rect_path(r, corner_radius(el, r.w.abs().min(r.h.abs()))), &st, cx);
        let link = opt_str(el, "link").unwrap_or(if model::kind(el) == "iframe" { "iframe" } else { "embed" });
        if let Some(ink) = paint_color("#495057", cx.opacity, false) {
            self.caption(r, link, ink, cx);
        }
    }

    fn grid_lines(&mut self, region: &Rect, step: f64, dark: bool) {
        let tone = if dark { 255 } else { 0 };
        let faint = sk::Color::from_rgba8(tone, tone, tone, 15);
        let axis = sk::Color::from_rgba8(tone, tone, tone, 36);
        let (w, h) = (self.pm.width() as f32, self.pm.height() as f32);
        let stroke = sk::Stroke { width: 1.0, ..Default::default() };
        let id = sk::Transform::identity();
        for (v, vertical) in grid_values(region, step) {
            let px = if vertical { (v - region.x) * self.scale } else { (v - region.y) * self.scale };
            let px = px.round() as f32 + 0.5;
            let path = if vertical { segment((px as f64, 0.0), (px as f64, h as f64)) } else { segment((0.0, px as f64), (w as f64, px as f64)) };
            if let Some(path) = path {
                let c = if v == 0.0 { axis } else { faint };
                self.pm.stroke_path(&path, &paint(c), &stroke, id, None);
            }
        }
    }

    /// Scene coordinates along the top and left edges, in 11 px type.
    fn grid_labels(&mut self, region: &Rect, step: f64, dark: bool) {
        let ink = if dark { sk::Color::from_rgba8(0xad, 0xb5, 0xbd, 255) } else { sk::Color::from_rgba8(0x86, 0x8e, 0x96, 255) };
        let mut backing = self.bg;
        backing.apply_opacity(0.8);
        let font = Font::new(text::NUNITO);
        let size = 11.0;
        let lh = font.line_height_px(size);
        let cx = Ctx { ts: sk::Transform::identity(), clip: None, opacity: 1.0 };
        for (v, vertical) in grid_values(region, step) {
            let label = format!("{}", v as i64);
            let w = font.line_width(&label, size);
            let pos = if vertical {
                let px = ((v - region.x) * self.scale).round() + 3.0;
                (px, 1.0)
            } else {
                let py = ((v - region.y) * self.scale).round() + 1.0;
                if py < lh + 2.0 {
                    continue;
                }
                (3.0, py)
            };
            if let Some(b) = rect_path(Rect::new(pos.0 - 2.0, pos.1, w + 4.0, lh), 0.0) {
                self.fill(&b, backing, &cx);
            }
            self.label(&label, text::NUNITO, size, pos, ink, &cx);
        }
    }
}

/// Grid line positions: (scene coordinate, whether the line is vertical).
fn grid_values(region: &Rect, step: f64) -> Vec<(f64, bool)> {
    let mut out = Vec::new();
    for (start, len, vertical) in [(region.x, region.w, true), (region.y, region.h, false)] {
        let mut v = (start / step).ceil() * step;
        while v <= start + len {
            out.push((v, vertical));
            v += step;
        }
    }
    out
}

/// Decodes a `data:image/png;base64,…` URL.
fn decode_png_data_url(url: &str) -> Option<sk::Pixmap> {
    let (header, data) = url.split_once(',')?;
    if !header.starts_with("data:image/png") || !header.ends_with(";base64") {
        return None;
    }
    sk::Pixmap::decode_png(&base64_decode(data)?).ok()
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A text element sized the way the editor would size it.
    fn text_el(id: &str, s: &str, (x, y): Pt, size: f64, family: u32, align: &str, container: Option<&str>) -> Value {
        let font = Font::new(family);
        json!({
            "id": id, "type": "text", "x": x, "y": y, "angle": 0,
            "width": font.text_width(s, size), "height": font.text_height(s, size),
            "text": s, "originalText": s, "fontSize": size, "fontFamily": family, "lineHeight": font.line_height,
            "textAlign": align, "verticalAlign": "middle", "containerId": container,
            "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "opacity": 100,
        })
    }

    /// A label centred in a container's box.
    fn label_in(id: &str, s: &str, container: &Value, size: f64) -> Value {
        let font = Font::new(text::EXCALIFONT);
        let r = model::rect(container);
        let (w, h) = (font.text_width(s, size), font.text_height(s, size));
        let at = (r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0);
        text_el(id, s, at, size, text::EXCALIFONT, "center", Some(model::id(container)))
    }

    fn with(mut el: Value, extra: Value) -> Value {
        for (k, v) in extra.as_object().unwrap() {
            el[k] = v.clone();
        }
        el
    }

    fn shape(id: &str, kind: &str, (x, y, w, h): (f64, f64, f64, f64), extra: Value) -> Value {
        let el = json!({
            "id": id, "type": kind, "x": x, "y": y, "width": w, "height": h, "angle": 0,
            "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "fillStyle": "solid",
            "strokeWidth": 2, "strokeStyle": "solid", "roughness": 1, "opacity": 100, "roundness": null,
        });
        with(el, extra)
    }

    fn arrow(id: &str, points: &[Pt], extra: Value) -> Value {
        let (x0, y0) = points[0];
        let rel: Vec<Value> = points.iter().map(|(x, y)| json!([x - x0, y - y0])).collect();
        let b = Rect::from_points(points.iter().copied()).unwrap();
        let el = shape(id, "arrow", (x0, y0, b.w, b.h), json!({ "points": rel, "endArrowhead": "arrow", "startArrowhead": null }));
        with(el, extra)
    }

    fn tiny_png() -> String {
        let mut pm = sk::Pixmap::new(4, 2).unwrap();
        pm.fill(sk::Color::from_rgba8(250, 82, 82, 255));
        let png = pm.encode_png().unwrap();
        const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in png.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
            for i in 0..4 {
                out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        format!("data:image/png;base64,{out}")
    }

    fn scene() -> (Vec<Value>, Map<String, Value>) {
        let api = shape("api", "rectangle", (100.0, 100.0, 200.0, 100.0), json!({
            "backgroundColor": "#a5d8ff", "strokeColor": "#1971c2", "roundness": { "type": 3 }, "frameId": "f1",
        }));
        let db = shape("db", "ellipse", (440.0, 95.0, 180.0, 110.0), json!({
            "backgroundColor": "#b2f2bb", "strokeColor": "#2f9e44", "fillStyle": "hachure", "frameId": "f1",
        }));
        let q = shape("q", "diamond", (120.0, 280.0, 160.0, 120.0), json!({
            "backgroundColor": "#ffec99", "fillStyle": "cross-hatch", "roundness": { "type": 2 }, "frameId": "f1",
        }));
        let sql = arrow("sql", &[(305.0, 150.0), (435.0, 150.0)], json!({ "frameId": "f1" }));
        let sql_box = json!({ "id": "sql", "x": 345.0, "y": 140.0, "width": 50.0, "height": 20.0 });
        let wave: Vec<Value> = (0..40).map(|i| json!([i as f64 * 3.0, (i as f64 / 4.0).sin() * 18.0])).collect();
        let mut els = vec![
            shape("f1", "frame", (60.0, 60.0, 620.0, 380.0), json!({ "name": "Architecture", "strokeColor": "#bbb" })),
            api.clone(),
            label_in("t1", "API server", &api, 20.0),
            db.clone(),
            label_in("t2", "Database", &db, 20.0),
            sql,
            label_in("t3", "SQL", &sql_box, 16.0),
            q.clone(),
            label_in("t4", "Cached?", &q, 20.0),
            arrow("curve", &[(280.0, 340.0), (420.0, 300.0), (530.0, 215.0)], json!({
                "roundness": { "type": 2 }, "strokeStyle": "dashed", "startArrowhead": "dot", "endArrowhead": "triangle",
                "strokeColor": "#e03131", "frameId": "f1",
            })),
            // Sticks out of the frame: clipped at the frame's edge.
            shape("clipped", "rectangle", (600.0, 360.0, 160.0, 60.0), json!({ "backgroundColor": "#ffc9c9", "frameId": "f1" })),
            text_el("title", "Request flow\nleft aligned, Nunito", (760.0, 60.0), 24.0, text::NUNITO, "left", None),
            text_el("note", "right aligned\nComic Shanns", (760.0, 150.0), 18.0, text::COMIC_SHANNS, "right", None),
            shape("rot", "rectangle", (780.0, 240.0, 140.0, 70.0), json!({
                "angle": 0.35, "backgroundColor": "#d0bfff", "strokeColor": "#6741d9", "roundness": { "type": 3 }, "opacity": 60,
            })),
            shape("loop", "line", (960.0, 80.0, 100.0, 100.0), json!({
                "points": [[0, 0], [100, 30], [60, 100], [0, 0]], "backgroundColor": "#ffd8a8", "strokeColor": "#e8590c",
            })),
            shape("pen", "freedraw", (960.0, 230.0, 120.0, 40.0), json!({
                "points": wave, "pressures": [], "simulatePressure": true, "strokeWidth": 1,
            })),
            shape("img", "image", (960.0, 320.0, 120.0, 60.0), json!({ "fileId": "file1", "scale": [1, 1] })),
            shape("gif", "image", (1100.0, 320.0, 100.0, 60.0), json!({ "fileId": "nope" })),
            shape("web", "embeddable", (1100.0, 80.0, 160.0, 100.0), json!({ "link": "https://example.com", "strokeColor": "transparent" })),
            shape("dots", "rectangle", (1100.0, 220.0, 160.0, 60.0), json!({ "strokeStyle": "dotted" })),
        ];
        let heads = [
            "bar", "diamond", "circle_outline", "crowfoot_many", "crowfoot_one_or_many", "triangle_outline", "crowfoot_one",
            "diamond_outline",
        ];
        for (i, h) in heads.iter().enumerate() {
            let y = 480.0 + i as f64 * 30.0;
            els.push(arrow(&format!("h{i}"), &[(80.0, y), (260.0, y)], json!({ "startArrowhead": h, "endArrowhead": h })));
            els.push(text_el(&format!("hl{i}"), h, (280.0, y - 10.0), 16.0, text::EXCALIFONT, "left", None));
        }
        let elbow = [(500.0, 480.0), (600.0, 480.0), (600.0, 600.0), (700.0, 600.0)];
        els.push(arrow("elbow", &elbow, json!({ "elbowed": true, "strokeColor": "#1971c2" })));
        let mut files = Map::new();
        files.insert("file1".into(), json!({ "id": "file1", "mimeType": "image/png", "dataURL": tiny_png() }));
        (els, files)
    }

    fn pixel(r: &Rendered, p: Pt) -> [u8; 4] {
        let pm = sk::Pixmap::decode_png(&r.png).unwrap();
        let x = ((p.0 - r.region.x) * r.scale) as u32;
        let y = ((p.1 - r.region.y) * r.scale) as u32;
        let c = pm.pixel(x, y).unwrap().demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    fn render_scene() -> Rendered {
        let (els, files) = scene();
        let refs: Vec<&Value> = els.iter().collect();
        render(&refs, &files, "#ffffff", &RenderOptions { grid: true, ..Default::default() }).unwrap()
    }

    #[test]
    fn renders_a_scene() {
        let r = render_scene();
        assert_eq!(&r.png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(r.width.max(r.height) <= 1600 && r.width > r.height);
        let pm = sk::Pixmap::decode_png(&r.png).unwrap();
        assert_eq!((pm.width(), pm.height()), (r.width, r.height));
        // Inside the solid rectangle, away from its label.
        assert_eq!(pixel(&r, (115.0, 115.0)), [0xa5, 0xd8, 0xff, 255]);
        // The image is drawn; the frame clips its child.
        assert_eq!(pixel(&r, (1020.0, 350.0)), [250, 82, 82, 255]);
        assert_eq!(pixel(&r, (720.0, 410.0)), [255, 255, 255, 255]);
        assert_eq!(pixel(&r, (650.0, 410.0)), [0xff, 0xc9, 0xc9, 255]);
        // Text is drawn in its colour somewhere on its first line.
        let font = Font::new(text::NUNITO);
        let dark = (760..900).any(|x| pixel(&r, (x as f64, 60.0 + font.baseline_offset(24.0) - 5.0))[0] < 80);
        assert!(dark);
    }

    #[test]
    fn honours_region_and_scale_limits() {
        let el = shape("r", "rectangle", (0.0, 0.0, 50.0, 20.0), json!({ "backgroundColor": "#000000" }));
        let r = render(&[&el], &Map::new(), "#ffffff", &RenderOptions::default()).unwrap();
        assert_eq!(r.scale, 2.0);
        assert_eq!((r.width, r.height), (260, 200));
        let region = Rect::new(-1000.0, -500.0, 4000.0, 2000.0);
        let opts = RenderOptions { region: Some(region), max_size: 800, ..Default::default() };
        let r = render(&[&el], &Map::new(), "#ffffff", &opts).unwrap();
        assert_eq!((r.width, r.height, r.scale), (800, 400, 0.2));
        assert_eq!(pixel(&r, (25.0, 10.0)), [0, 0, 0, 255]);
        let flat = RenderOptions { region: Some(Rect::new(0.0, 0.0, 0.0, 5.0)), ..Default::default() };
        assert!(render(&[&el], &Map::new(), "#fff", &flat).is_err());
    }

    #[test]
    fn empty_scene_still_renders() {
        let r = render(&[], &Map::new(), "#ffffff", &RenderOptions { grid: true, ..Default::default() }).unwrap();
        assert_eq!((r.width, r.height), (800, 500));
        assert_eq!(r.region, Rect::new(0.0, 0.0, 800.0, 500.0));
        assert_eq!(grid_step(&r.region, r.scale), 100.0);
    }

    #[test]
    fn decodes_base64() {
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64_decode("YQ").unwrap(), b"a");
        assert!(base64_decode("a$b").is_none());
        assert!(decode_png_data_url(&tiny_png()).is_some());
        assert!(decode_png_data_url("data:image/jpeg;base64,AAAA").is_none());
    }

    /// Writes the test scene to `$RENDER_PREVIEW` for eyeballing.
    #[test]
    #[ignore]
    fn write_preview() {
        let path = std::env::var("RENDER_PREVIEW").unwrap();
        std::fs::write(&path, render_scene().png).unwrap();
        let (els, files) = scene();
        let refs: Vec<&Value> = els.iter().collect();
        let small = RenderOptions { max_size: 500, grid: true, ..Default::default() };
        std::fs::write(path.replace(".png", "-small.png"), render(&refs, &files, "#ffffff", &small).unwrap().png).unwrap();
        let zoom = RenderOptions { region: Some(Rect::new(80.0, 80.0, 400.0, 150.0)), grid: true, ..Default::default() };
        std::fs::write(path.replace(".png", "-zoom.png"), render(&refs, &files, "#121212", &zoom).unwrap().png).unwrap();
    }
}
