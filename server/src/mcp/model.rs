//! Read access to raw Excalidraw elements (`serde_json::Value`s as stored in
//! the scene). Elements are kept as JSON so fields this code doesn't know
//! about survive round trips.

use super::geometry::{rotate, Pt, Rect};
use serde_json::Value;

pub fn num(el: &Value, key: &str) -> f64 {
    el.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0)
}

pub fn opt_num(el: &Value, key: &str) -> Option<f64> {
    el.get(key).and_then(Value::as_f64).filter(|v| v.is_finite())
}

pub fn str_of<'a>(el: &'a Value, key: &str) -> &'a str {
    el.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn opt_str<'a>(el: &'a Value, key: &str) -> Option<&'a str> {
    el.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub fn id(el: &Value) -> &str {
    str_of(el, "id")
}

pub fn kind(el: &Value) -> &str {
    str_of(el, "type")
}

pub fn is_deleted(el: &Value) -> bool {
    el.get("isDeleted").and_then(Value::as_bool).unwrap_or(false)
}

pub fn is_linear(el: &Value) -> bool {
    matches!(kind(el), "arrow" | "line")
}

pub fn has_points(el: &Value) -> bool {
    matches!(kind(el), "arrow" | "line" | "freedraw")
}

pub fn is_frame(el: &Value) -> bool {
    matches!(kind(el), "frame" | "magicframe")
}

/// Shapes that can hold a label (`containerId` on a text points at them).
pub fn is_container_type(t: &str) -> bool {
    matches!(t, "rectangle" | "ellipse" | "diamond" | "arrow")
}

/// Shapes an arrow can bind to.
pub fn is_bindable_type(t: &str) -> bool {
    matches!(t, "rectangle" | "ellipse" | "diamond" | "text" | "image" | "frame" | "magicframe" | "embeddable" | "iframe")
}

pub fn x(el: &Value) -> f64 {
    num(el, "x")
}
pub fn y(el: &Value) -> f64 {
    num(el, "y")
}
pub fn width(el: &Value) -> f64 {
    num(el, "width")
}
pub fn height(el: &Value) -> f64 {
    num(el, "height")
}
pub fn angle(el: &Value) -> f64 {
    num(el, "angle")
}

/// The element's own box (position and size, before rotation).
pub fn rect(el: &Value) -> Rect {
    Rect::new(x(el), y(el), width(el), height(el))
}

/// Points of a linear or freedraw element, relative to its `x`/`y`.
pub fn rel_points(el: &Value) -> Vec<Pt> {
    el.get("points")
        .and_then(Value::as_array)
        .map(|pts| {
            pts.iter()
                .filter_map(|p| {
                    let a = p.as_array()?;
                    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Points in scene coordinates (rotation applied).
pub fn abs_points(el: &Value) -> Vec<Pt> {
    let (ox, oy) = (x(el), y(el));
    let pts: Vec<Pt> = rel_points(el).into_iter().map(|(px, py)| (ox + px, oy + py)).collect();
    let a = angle(el);
    if a == 0.0 || pts.is_empty() {
        return pts;
    }
    // Excalidraw rotates linear elements around the centre of their points' box.
    let c = Rect::from_points(pts.iter().copied()).map(|r| r.center()).unwrap_or((ox, oy));
    pts.into_iter().map(|p| rotate(p, c, a)).collect()
}

/// Axis-aligned bounds in scene coordinates, rotation included.
pub fn bounds(el: &Value) -> Rect {
    if has_points(el) {
        if let Some(r) = Rect::from_points(abs_points(el)) {
            return r;
        }
    }
    let r = rect(el);
    let a = angle(el);
    if a == 0.0 {
        return r;
    }
    let c = r.center();
    let corners = [(r.x, r.y), (r.right(), r.y), (r.right(), r.bottom()), (r.x, r.bottom())];
    Rect::from_points(corners.into_iter().map(|p| rotate(p, c, a))).unwrap_or(r)
}

pub fn container_id(el: &Value) -> Option<&str> {
    opt_str(el, "containerId")
}

pub fn frame_id(el: &Value) -> Option<&str> {
    opt_str(el, "frameId")
}

pub fn group_ids(el: &Value) -> Vec<String> {
    el.get("groupIds")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|g| g.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// `boundElements` entries as (id, type).
pub fn bound_elements(el: &Value) -> Vec<(String, String)> {
    el.get("boundElements")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|b| Some((b.get("id")?.as_str()?.to_owned(), b.get("type")?.as_str().unwrap_or("").to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// Id of the label text bound to a container.
pub fn bound_text_id(el: &Value) -> Option<String> {
    bound_elements(el).into_iter().find(|(_, t)| t == "text").map(|(id, _)| id)
}

/// `startBinding` / `endBinding` target ids of an arrow.
pub fn binding_target<'a>(el: &'a Value, key: &str) -> Option<&'a str> {
    el.get(key)?.get("elementId")?.as_str().filter(|s| !s.is_empty())
}

/// The visible text of a text element (wrapped lines included).
pub fn text_of(el: &Value) -> &str {
    str_of(el, "text")
}

/// The text as written, before wrapping.
pub fn original_text(el: &Value) -> &str {
    opt_str(el, "originalText").unwrap_or_else(|| text_of(el))
}

pub fn version(el: &Value) -> i64 {
    el.get("version").and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0)
}

pub fn index(el: &Value) -> Option<&str> {
    opt_str(el, "index")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounds_cover_points_and_rotation() {
        let arrow = json!({ "type": "arrow", "x": 10, "y": 10, "points": [[0, 0], [100, 50]] });
        assert_eq!(bounds(&arrow), Rect::new(10.0, 10.0, 100.0, 50.0));
        let rotated = json!({ "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 100, "angle": std::f64::consts::FRAC_PI_4 });
        let b = bounds(&rotated);
        assert!((b.w - 141.42).abs() < 0.1);
    }
}
