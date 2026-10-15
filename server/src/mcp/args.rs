//! Forgiving access to tool arguments. Models write `right_of`, `rightOf` or
//! `right-of`, numbers as strings, single values where lists are expected;
//! all of that is accepted rather than failing the call.

use super::geometry::Pt;
use serde_json::{Map, Value};

fn norm(key: &str) -> String {
    key.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

#[derive(Clone, Copy)]
pub struct Args<'a>(pub &'a Map<String, Value>);

pub static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);

impl<'a> Args<'a> {
    pub fn of(v: &'a Value) -> Args<'a> {
        Args(v.as_object().unwrap_or(&EMPTY))
    }

    /// The value under any spelling of `key`.
    pub fn get(&self, key: &str) -> Option<&'a Value> {
        if let Some(v) = self.0.get(key) {
            return Some(v).filter(|v| !v.is_null());
        }
        let want = norm(key);
        self.0.iter().find(|(k, _)| norm(k) == want).map(|(_, v)| v).filter(|v| !v.is_null())
    }

    /// The first of several names that is present.
    pub fn any(&self, keys: &[&str]) -> Option<&'a Value> {
        keys.iter().find_map(|k| self.get(k))
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn str(&self, key: &str) -> Option<&'a str> {
        self.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
    }

    /// A string, also accepting numbers and booleans written as values.
    pub fn text(&self, key: &str) -> Option<String> {
        match self.get(key)? {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn num(&self, key: &str) -> Option<f64> {
        as_num(self.get(key)?)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            Value::Bool(b) => Some(*b),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" | "on" => Some(true),
                "false" | "no" | "0" | "off" => Some(false),
                _ => None,
            },
            Value::Number(n) => Some(n.as_f64() != Some(0.0)),
            _ => None,
        }
    }

    pub fn point(&self, key: &str) -> Option<Pt> {
        as_point(self.get(key)?)
    }

    /// A list of strings (a single string counts as a list of one).
    pub fn strings(&self, key: &str) -> Option<Vec<String>> {
        match self.get(key)? {
            Value::String(s) => Some(vec![s.trim().to_owned()]),
            Value::Array(a) => Some(a.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_owned())).collect()),
            _ => None,
        }
    }

    pub fn list(&self, key: &str) -> Option<&'a Vec<Value>> {
        self.get(key).and_then(Value::as_array)
    }

    pub fn obj(&self, key: &str) -> Option<Args<'a>> {
        self.get(key).and_then(Value::as_object).map(Args)
    }
}

pub fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches("px").trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

/// `[x, y]`, `{x, y}` or `"x,y"`.
pub fn as_point(v: &Value) -> Option<Pt> {
    match v {
        Value::Array(a) if a.len() >= 2 => Some((as_num(&a[0])?, as_num(&a[1])?)),
        Value::Object(o) => {
            let a = Args(o);
            Some((a.num("x")?, a.num("y")?))
        }
        Value::String(s) => {
            let (x, y) = s.trim().trim_matches(|c| c == '(' || c == ')' || c == '[' || c == ']').split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        }
        _ => None,
    }
}

pub fn as_points(v: &Value) -> Option<Vec<Pt>> {
    v.as_array()?.iter().map(as_point).collect()
}

/// A readable id from a label: "API Gateway" -> "api-gateway".
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 28 {
            break;
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_in_any_spelling() {
        let v = json!({ "rightOf": "a", "font_size": "24", "center": [1, 2], "ids": "x" });
        let a = Args::of(&v);
        assert_eq!(a.str("right_of"), Some("a"));
        assert_eq!(a.num("fontSize"), Some(24.0));
        assert_eq!(a.point("center"), Some((1.0, 2.0)));
        assert_eq!(a.strings("ids").unwrap(), ["x"]);
        assert_eq!(slug("API Gateway (v2)"), "api-gateway-v2");
    }
}
