//! Plane geometry for shapes and arrows: bounds, outlines, where an arrow
//! meets a shape, and Excalidraw's binding "focus".

pub type Pt = (f64, f64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn from_points(points: impl IntoIterator<Item = Pt>) -> Option<Self> {
        let mut it = points.into_iter();
        let (x0, y0) = it.next()?;
        let (mut a, mut b, mut c, mut d) = (x0, y0, x0, y0);
        for (x, y) in it {
            a = a.min(x);
            b = b.min(y);
            c = c.max(x);
            d = d.max(y);
        }
        Some(Self::new(a, b, c - a, d - b))
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn center(&self) -> Pt {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn inflate(&self, d: f64) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
    /// Overlap area's width and height (zero or negative when apart).
    pub fn overlap(&self, o: &Rect) -> (f64, f64) {
        (self.right().min(o.right()) - self.x.max(o.x), self.bottom().min(o.bottom()) - self.y.max(o.y))
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        let (w, h) = self.overlap(o);
        w > 0.0 && h > 0.0
    }
    pub fn contains_rect(&self, o: &Rect, slack: f64) -> bool {
        o.x >= self.x - slack && o.y >= self.y - slack && o.right() <= self.right() + slack && o.bottom() <= self.bottom() + slack
    }
    pub fn contains(&self, p: Pt) -> bool {
        p.0 >= self.x && p.0 <= self.right() && p.1 >= self.y && p.1 <= self.bottom()
    }
}

pub fn union_all(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    rects.into_iter().reduce(|a, b| a.union(&b))
}

pub fn dist(a: Pt, b: Pt) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

pub fn rotate(p: Pt, center: Pt, angle: f64) -> Pt {
    if angle == 0.0 {
        return p;
    }
    let (s, c) = angle.sin_cos();
    let (dx, dy) = (p.0 - center.0, p.1 - center.1);
    (center.0 + dx * c - dy * s, center.1 + dx * s + dy * c)
}

/// The kinds of outline an arrow can bind to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outline {
    Rect,
    Ellipse,
    Diamond,
}

impl Outline {
    pub fn of(element_type: &str) -> Outline {
        match element_type {
            "ellipse" => Outline::Ellipse,
            "diamond" => Outline::Diamond,
            _ => Outline::Rect,
        }
    }
}

/// Where the ray from the shape's centre towards `toward` leaves the shape,
/// pushed out by `gap`.
pub fn boundary_point(shape: &Rect, outline: Outline, toward: Pt, gap: f64) -> Pt {
    let (cx, cy) = shape.center();
    let (dx, dy) = (toward.0 - cx, toward.1 - cy);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        return (cx, cy);
    }
    let (ux, uy) = (dx / len, dy / len);
    let (a, b) = (shape.w / 2.0, shape.h / 2.0);
    let t = match outline {
        Outline::Rect => {
            let tx = if ux.abs() > 1e-12 { a / ux.abs() } else { f64::INFINITY };
            let ty = if uy.abs() > 1e-12 { b / uy.abs() } else { f64::INFINITY };
            tx.min(ty)
        }
        Outline::Ellipse => {
            if a < 1e-9 || b < 1e-9 {
                0.0
            } else {
                1.0 / ((ux / a).powi(2) + (uy / b).powi(2)).sqrt()
            }
        }
        Outline::Diamond => {
            if a < 1e-9 || b < 1e-9 {
                0.0
            } else {
                1.0 / (ux.abs() / a + uy.abs() / b)
            }
        }
    };
    (cx + ux * (t + gap), cy + uy * (t + gap))
}

/// The midpoint of one side, pushed out by `gap`.
pub fn side_point(shape: &Rect, side: Side, gap: f64) -> Pt {
    let (cx, cy) = shape.center();
    match side {
        Side::Top => (cx, shape.y - gap),
        Side::Bottom => (cx, shape.bottom() + gap),
        Side::Left => (shape.x - gap, cy),
        Side::Right => (shape.right() + gap, cy),
    }
}

/// A point on a side, `t` in 0..1 along it, pushed out by `gap`. For
/// ellipses and diamonds the point follows the outline.
pub fn side_point_at(shape: &Rect, outline: Outline, side: Side, t: f64, gap: f64) -> Pt {
    if outline != Outline::Rect || (t - 0.5).abs() < 1e-6 {
        if outline == Outline::Rect {
            return side_point(shape, side, gap);
        }
        // Aim through the spot on the bounding box side, then onto the outline.
        let target = match side {
            Side::Top => (shape.x + shape.w * t, shape.y - shape.h),
            Side::Bottom => (shape.x + shape.w * t, shape.bottom() + shape.h),
            Side::Left => (shape.x - shape.w, shape.y + shape.h * t),
            Side::Right => (shape.right() + shape.w, shape.y + shape.h * t),
        };
        return boundary_point(shape, outline, target, gap);
    }
    match side {
        Side::Top => (shape.x + shape.w * t, shape.y - gap),
        Side::Bottom => (shape.x + shape.w * t, shape.bottom() + gap),
        Side::Left => (shape.x - gap, shape.y + shape.h * t),
        Side::Right => (shape.right() + gap, shape.y + shape.h * t),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    pub fn parse(s: &str) -> Option<Side> {
        match s.trim().to_ascii_lowercase().as_str() {
            "top" | "up" | "n" | "north" => Some(Side::Top),
            "bottom" | "down" | "s" | "south" => Some(Side::Bottom),
            "left" | "w" | "west" => Some(Side::Left),
            "right" | "e" | "east" => Some(Side::Right),
            _ => None,
        }
    }
    pub fn horizontal(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }
    /// Unit vector pointing out of the side.
    pub fn normal(self) -> Pt {
        match self {
            Side::Top => (0.0, -1.0),
            Side::Bottom => (0.0, 1.0),
            Side::Left => (-1.0, 0.0),
            Side::Right => (1.0, 0.0),
        }
    }
    pub fn opposite(self) -> Side {
        match self {
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// The side of `a` that faces `b`.
pub fn facing_side(a: &Rect, b: &Rect) -> Side {
    let (ac, bc) = (a.center(), b.center());
    let (dx, dy) = (bc.0 - ac.0, bc.1 - ac.1);
    // Gaps between the boxes decide first; centres break ties.
    let gap_x = (b.x - a.right()).max(a.x - b.right());
    let gap_y = (b.y - a.bottom()).max(a.y - b.bottom());
    let horizontal = if gap_x > 0.0 || gap_y > 0.0 { gap_x >= gap_y } else { dx.abs() * a.h.max(1.0) >= dy.abs() * a.w.max(1.0) };
    if horizontal {
        if dx >= 0.0 { Side::Right } else { Side::Left }
    } else if dy >= 0.0 {
        Side::Bottom
    } else {
        Side::Top
    }
}

fn cross(a: Pt, b: Pt) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

/// Intersection of segments p1-p2 and p3-p4.
pub fn segment_intersection(p1: Pt, p2: Pt, p3: Pt, p4: Pt) -> Option<Pt> {
    let r = (p2.0 - p1.0, p2.1 - p1.1);
    let s = (p4.0 - p3.0, p4.1 - p3.1);
    let denom = cross(r, s);
    if denom.abs() < 1e-12 {
        return None;
    }
    let qp = (p3.0 - p1.0, p3.1 - p1.1);
    let t = cross(qp, s) / denom;
    let u = cross(qp, r) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then(|| (p1.0 + t * r.0, p1.1 + t * r.1))
}

/// Excalidraw's `determineFocusDistance` for an unrotated shape: where the
/// line from `adjacent` through `edge` passes relative to the centre, as a
/// signed fraction (0 = through the centre).
pub fn focus(shape: &Rect, outline: Outline, adjacent: Pt, edge: Pt) -> f64 {
    let c = shape.center();
    if dist(adjacent, edge) < 1e-9 {
        return 0.0;
    }
    let turn = cross((edge.0 - adjacent.0, edge.1 - adjacent.1), (edge.0 - c.0, edge.1 - c.1));
    if turn.abs() < 1e-9 {
        return 0.0;
    }
    let sign = -turn.signum();
    let (x, y, w, h) = (shape.x, shape.y, shape.w, shape.h);
    let (dx, dy) = (edge.0 - adjacent.0, edge.1 - adjacent.1);
    let len = (dx * dx + dy * dy).sqrt();
    let reach = (w * 2.0).max(h * 2.0);
    let far = (edge.0 + dx / len * reach, edge.1 + dy / len * reach);
    let (interceptees, axes) = if outline == Outline::Diamond {
        (
            [((x + w / 2.0, y - h), (x + w / 2.0, y + h * 2.0)), ((x - w, y + h / 2.0), (x + w * 2.0, y + h / 2.0))],
            [h, w],
        )
    } else {
        let diag = (w * w + h * h).sqrt();
        ([((x - w, y - h), (x + w * 2.0, y + h * 2.0)), ((x + w * 2.0, y - h), (x - w, y + h * 2.0))], [diag, diag])
    };
    let mut hits: Vec<(Pt, f64)> = interceptees
        .iter()
        .zip(axes)
        .filter_map(|(seg, axis)| segment_intersection(edge, far, seg.0, seg.1).map(|p| (p, axis)))
        .collect();
    hits.sort_by(|a, b| dist(a.0, edge).total_cmp(&dist(b.0, edge)));
    let mut ratios: Vec<f64> = hits.iter().map(|(p, axis)| sign * dist(c, *p) / (axis / 2.0)).collect();
    ratios.sort_by(|a, b| a.abs().total_cmp(&b.abs()));
    ratios.first().copied().unwrap_or(0.0).clamp(-1.0, 1.0)
}

/// Whether segment a-b passes through the interior of `r` (shrunk by `inset`).
pub fn segment_hits_rect(a: Pt, b: Pt, r: &Rect, inset: f64) -> bool {
    let r = r.inflate(-inset);
    if r.w <= 0.0 || r.h <= 0.0 {
        return false;
    }
    if r.contains(a) || r.contains(b) {
        return true;
    }
    let corners = [(r.x, r.y), (r.right(), r.y), (r.right(), r.bottom()), (r.x, r.bottom())];
    (0..4).any(|i| segment_intersection(a, b, corners[i], corners[(i + 1) % 4]).is_some())
}

/// Points along a Catmull-Rom curve through `points`, the way Excalidraw
/// draws rounded lines.
pub fn smooth(points: &[Pt], steps: usize) -> Vec<Pt> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(points.len() * steps);
    for i in 0..points.len() - 1 {
        let p0 = if i == 0 { points[0] } else { points[i - 1] };
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = if i + 2 < points.len() { points[i + 2] } else { points[i + 1] };
        for s in 0..steps {
            let t = s as f64 / steps as f64;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f64, b: f64, c: f64, d: f64| {
                0.5 * (2.0 * b + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (-a + 3.0 * b - 3.0 * c + d) * t3)
            };
            out.push((f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1)));
        }
    }
    out.push(*points.last().unwrap());
    out
}

/// Midpoint along a polyline, by length.
pub fn polyline_midpoint(points: &[Pt]) -> Pt {
    let total: f64 = points.windows(2).map(|w| dist(w[0], w[1])).sum();
    let mut left = total / 2.0;
    for w in points.windows(2) {
        let d = dist(w[0], w[1]);
        if d >= left && d > 0.0 {
            let t = left / d;
            return (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
        }
        left -= d;
    }
    points.first().copied().unwrap_or((0.0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_points_land_on_outlines() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        assert_eq!(boundary_point(&r, Outline::Rect, (300.0, 25.0), 0.0), (100.0, 25.0));
        let (x, y) = boundary_point(&r, Outline::Ellipse, (50.0, -100.0), 0.0);
        assert!((x - 50.0).abs() < 1e-6 && y.abs() < 1e-6);
        let (x, _) = boundary_point(&r, Outline::Diamond, (500.0, 25.0), 4.0);
        assert!((x - 104.0).abs() < 1e-6);
    }

    #[test]
    fn focus_is_zero_through_the_centre() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(focus(&r, Outline::Rect, (50.0, -100.0), (50.0, -5.0)).abs() < 1e-9);
        let off = focus(&r, Outline::Rect, (80.0, -100.0), (80.0, -5.0));
        assert!(off.abs() > 0.1 && off.abs() < 1.0);
    }

    #[test]
    fn segments_and_rects() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(segment_hits_rect((-5.0, 5.0), (15.0, 5.0), &r, 0.0));
        assert!(!segment_hits_rect((-5.0, 15.0), (15.0, 15.0), &r, 0.0));
    }
}
