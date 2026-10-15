//! Fractional z-order keys, the same scheme Excalidraw uses for `index`
//! (rocicorp's `fractional-indexing`, base 62). Keys sort as plain strings,
//! and there is always room for a new key between two others.

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const SMALLEST_INTEGER: &str = "A00000000000000000000000000";

fn digit(c: u8) -> usize {
    DIGITS.iter().position(|&d| d == c).unwrap_or(0)
}

fn integer_length(head: u8) -> Option<usize> {
    match head {
        b'a'..=b'z' => Some((head - b'a') as usize + 2),
        b'A'..=b'Z' => Some((b'Z' - head) as usize + 2),
        _ => None,
    }
}

fn integer_part(key: &str) -> Option<&str> {
    let len = integer_length(*key.as_bytes().first()?)?;
    key.get(..len)
}

/// A key Excalidraw would accept: a valid integer part and no trailing zero.
pub fn is_valid(key: &str) -> bool {
    if key == SMALLEST_INTEGER || !key.is_ascii() {
        return false;
    }
    let Some(int) = integer_part(key) else { return false };
    let frac = &key[int.len()..];
    key.bytes().all(|b| DIGITS.contains(&b)) && !frac.ends_with('0')
}

/// A string strictly between `a` and `b` (fraction parts, no integer head).
fn midpoint(a: &str, b: Option<&str>) -> String {
    if let Some(b) = b {
        let (ab, bb) = (a.as_bytes(), b.as_bytes());
        let mut n = 0;
        while n < bb.len() && ab.get(n).copied().unwrap_or(b'0') == bb[n] {
            n += 1;
        }
        if n > 0 {
            return format!("{}{}", &b[..n], midpoint(a.get(n..).unwrap_or(""), Some(&b[n..])));
        }
    }
    let da = a.bytes().next().map_or(0, digit);
    let db = b.and_then(|b| b.bytes().next()).map_or(DIGITS.len(), digit);
    if db - da > 1 {
        let mid = ((da + db) as f64 * 0.5).round() as usize;
        return (DIGITS[mid] as char).to_string();
    }
    match b {
        Some(b) if b.len() > 1 => b[..1].to_owned(),
        _ => format!("{}{}", DIGITS[da] as char, midpoint(a.get(1..).unwrap_or(""), None)),
    }
}

fn increment_integer(x: &str) -> Option<String> {
    let bytes = x.as_bytes();
    let head = bytes[0];
    let mut digs: Vec<u8> = bytes[1..].to_vec();
    let mut carry = true;
    for d in digs.iter_mut().rev() {
        if !carry {
            break;
        }
        let next = digit(*d) + 1;
        if next == DIGITS.len() {
            *d = DIGITS[0];
        } else {
            *d = DIGITS[next];
            carry = false;
        }
    }
    if carry {
        if head == b'Z' {
            return Some("a0".into());
        }
        if head == b'z' {
            return None;
        }
        let h = head + 1;
        if h > b'a' {
            digs.push(DIGITS[0]);
        } else {
            digs.pop();
        }
        return Some(format!("{}{}", h as char, String::from_utf8_lossy(&digs)));
    }
    Some(format!("{}{}", head as char, String::from_utf8_lossy(&digs)))
}

fn decrement_integer(x: &str) -> Option<String> {
    let bytes = x.as_bytes();
    let head = bytes[0];
    let mut digs: Vec<u8> = bytes[1..].to_vec();
    let mut borrow = true;
    for d in digs.iter_mut().rev() {
        if !borrow {
            break;
        }
        let i = digit(*d);
        if i == 0 {
            *d = DIGITS[DIGITS.len() - 1];
        } else {
            *d = DIGITS[i - 1];
            borrow = false;
        }
    }
    if borrow {
        if head == b'a' {
            return Some(format!("Z{}", DIGITS[DIGITS.len() - 1] as char));
        }
        if head == b'A' {
            return None;
        }
        let h = head - 1;
        if h < b'Z' {
            digs.push(DIGITS[DIGITS.len() - 1]);
        } else {
            digs.pop();
        }
        return Some(format!("{}{}", h as char, String::from_utf8_lossy(&digs)));
    }
    Some(format!("{}{}", head as char, String::from_utf8_lossy(&digs)))
}

/// A key that sorts after `a` and before `b` (either may be open-ended).
/// Invalid neighbours are treated as open ends.
pub fn key_between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.filter(|k| is_valid(k));
    let b = b.filter(|k| is_valid(k));
    let (a, b) = match (a, b) {
        (Some(x), Some(y)) if x >= y => (Some(x), None),
        other => other,
    };
    match (a, b) {
        (None, None) => "a0".into(),
        (None, Some(b)) => {
            let ib = integer_part(b).unwrap_or("a0");
            let fb = &b[ib.len()..];
            if ib == SMALLEST_INTEGER {
                return format!("{ib}{}", midpoint("", Some(fb)));
            }
            if ib < b {
                return ib.to_owned();
            }
            decrement_integer(ib).unwrap_or_else(|| format!("{ib}{}", midpoint("", Some(fb))))
        }
        (Some(a), None) => {
            let ia = integer_part(a).unwrap_or("a0");
            let fa = &a[ia.len()..];
            increment_integer(ia).unwrap_or_else(|| format!("{ia}{}", midpoint(fa, None)))
        }
        (Some(a), Some(b)) => {
            let ia = integer_part(a).unwrap_or("a0");
            let fa = &a[ia.len()..];
            let ib = integer_part(b).unwrap_or("a0");
            let fb = &b[ib.len()..];
            if ia == ib {
                return format!("{ia}{}", midpoint(fa, Some(fb)));
            }
            match increment_integer(ia) {
                Some(i) if i.as_str() < b => i,
                _ => format!("{ia}{}", midpoint(fa, None)),
            }
        }
    }
}

/// `n` ascending keys strictly between `a` and `b`.
pub fn keys_between(a: Option<&str>, b: Option<&str>, n: usize) -> Vec<String> {
    let mut keys = Vec::with_capacity(n);
    let mut prev = a.map(str::to_owned);
    if b.is_none() {
        for _ in 0..n {
            let k = key_between(prev.as_deref(), None);
            prev = Some(k.clone());
            keys.push(k);
        }
        return keys;
    }
    // Bounded on the right: split the range recursively so keys stay short.
    fn fill(a: Option<&str>, b: Option<&str>, n: usize, out: &mut Vec<String>) {
        if n == 0 {
            return;
        }
        let mid = key_between(a, b);
        let left = n / 2;
        fill(a, Some(&mid), left, out);
        out.push(mid.clone());
        fill(Some(&mid), b, n - left - 1, out);
    }
    fill(prev.as_deref(), b, n, &mut keys);
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_reference_implementation() {
        assert_eq!(key_between(None, None), "a0");
        assert_eq!(key_between(Some("a0"), None), "a1");
        assert_eq!(key_between(Some("a1"), None), "a2");
        assert_eq!(key_between(Some("a0"), Some("a1")), "a0V");
        assert_eq!(key_between(Some("a1"), Some("a2")), "a1V");
        assert_eq!(key_between(Some("a0V"), Some("a1")), "a0l");
        assert_eq!(key_between(Some("Zz"), Some("a0")), "ZzV");
        assert_eq!(key_between(Some("Zz"), Some("a1")), "a0");
        assert_eq!(key_between(None, Some("Y00")), "Xzzz");
        assert_eq!(key_between(Some("bzz"), None), "c000");
        assert_eq!(key_between(Some("a0"), Some("a0V")), "a0G");
        assert_eq!(key_between(Some("a0"), Some("a0G")), "a08");
        assert_eq!(key_between(None, Some("a0")), "Zz");
        assert_eq!(key_between(Some("az"), None), "b00");
    }

    #[test]
    fn many_keys_stay_ordered() {
        let keys = keys_between(Some("a0"), Some("a1"), 50);
        assert_eq!(keys.len(), 50);
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        assert!(keys[0].as_str() > "a0" && keys[49].as_str() < "a1");
        assert!(keys.iter().all(|k| is_valid(k)));
        let open = keys_between(Some("a5"), None, 100);
        assert!(open.windows(2).all(|w| w[0] < w[1]) && open[0].as_str() > "a5");
    }
}
