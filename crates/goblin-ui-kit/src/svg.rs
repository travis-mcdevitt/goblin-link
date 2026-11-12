//! A deliberately small SVG reader for 24×24 outline icons (the Lucide subset): `path`,
//! `circle`, `ellipse`, `rect`, `line`, `polyline` and `polygon`, no transforms or styles.

use crate::raster::{Path, Pt};

/// Errors while reading an icon.
#[derive(Debug, PartialEq, Eq)]
pub struct SvgError(pub String);

impl std::fmt::Display for SvgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "svg: {}", self.0)
    }
}

impl std::error::Error for SvgError {}

/// Parses an icon document into a flattened path in viewBox units.
pub fn parse_icon(src: &str) -> Result<Path, SvgError> {
    let mut path = Path::default();
    let mut rest = src;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let end = rest
            .find('>')
            .ok_or_else(|| SvgError("unterminated tag".into()))?;
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        let name_end = tag
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(tag.len());
        let name = &tag[..name_end];
        let attrs = &tag[name_end..];
        let num = |k: &str| -> Result<f32, SvgError> {
            attr(attrs, k)
                .map(|v| v.trim().parse::<f32>())
                .unwrap_or(Ok(0.0))
                .map_err(|_| SvgError(format!("bad number in {k}")))
        };
        match name {
            "path" => {
                let d = attr(attrs, "d").ok_or_else(|| SvgError("path without d".into()))?;
                parse_path_data(d, &mut path)?;
            }
            "circle" => {
                let r = num("r")?;
                path.ellipse(num("cx")?, num("cy")?, r, r);
            }
            "ellipse" => path.ellipse(num("cx")?, num("cy")?, num("rx")?, num("ry")?),
            "rect" => {
                let rx = attr(attrs, "rx").map(|_| num("rx")).transpose()?;
                let ry = attr(attrs, "ry").map(|_| num("ry")).transpose()?;
                let (rx, ry) = match (rx, ry) {
                    (Some(a), Some(b)) => (a, b),
                    (Some(a), None) | (None, Some(a)) => (a, a),
                    (None, None) => (0.0, 0.0),
                };
                path.rrect(num("x")?, num("y")?, num("width")?, num("height")?, rx, ry);
            }
            "line" => {
                path.move_to((num("x1")?, num("y1")?));
                path.line_to((num("x2")?, num("y2")?));
            }
            "polyline" | "polygon" => {
                let pts = attr(attrs, "points").unwrap_or("");
                let mut toks = Tokens::new(pts);
                let mut first = true;
                while let Some(x) = toks.number() {
                    let y = toks
                        .number()
                        .ok_or_else(|| SvgError("odd point list".into()))?;
                    if first {
                        path.move_to((x, y));
                        first = false;
                    } else {
                        path.line_to((x, y));
                    }
                }
                if name == "polygon" && !first {
                    path.close();
                }
            }
            _ => {}
        }
    }
    path.finish();
    Ok(path)
}

/// Finds `key="value"` in an attribute string.
fn attr<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    let mut s = attrs;
    while let Some(i) = s.find(key) {
        let before_ok = i == 0 || s.as_bytes()[i - 1].is_ascii_whitespace();
        let after = &s[i + key.len()..];
        let after_trim = after.trim_start();
        if before_ok && after_trim.starts_with('=') {
            let v = after_trim[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let v = &v[1..];
                return v.find(q).map(|e| &v[..e]);
            }
        }
        s = &s[i + key.len()..];
    }
    None
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Tokens<'a> {
    fn new(s: &'a str) -> Self {
        Tokens {
            s: s.as_bytes(),
            i: 0,
        }
    }

    fn skip_sep(&mut self) {
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',')
        {
            self.i += 1;
        }
    }

    fn peek_cmd(&mut self) -> Option<u8> {
        self.skip_sep();
        self.s
            .get(self.i)
            .copied()
            .filter(|c| c.is_ascii_alphabetic() && *c != b'e' && *c != b'E')
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_sep();
        let start = self.i;
        let s = self.s;
        let mut j = self.i;
        if j < s.len() && (s[j] == b'-' || s[j] == b'+') {
            j += 1;
        }
        let mut seen_dot = false;
        let mut digits = 0;
        while j < s.len() && (s[j].is_ascii_digit() || (s[j] == b'.' && !seen_dot)) {
            if s[j] == b'.' {
                seen_dot = true;
            } else {
                digits += 1;
            }
            j += 1;
        }
        if digits == 0 {
            return None;
        }
        if j < s.len() && (s[j] == b'e' || s[j] == b'E') {
            let mut k = j + 1;
            if k < s.len() && (s[k] == b'-' || s[k] == b'+') {
                k += 1;
            }
            if k < s.len() && s[k].is_ascii_digit() {
                while k < s.len() && s[k].is_ascii_digit() {
                    k += 1;
                }
                j = k;
            }
        }
        self.i = j;
        std::str::from_utf8(&s[start..j]).ok()?.parse().ok()
    }

    /// Arc flags may be written without separators (`a1 1 0 011 1`).
    fn flag(&mut self) -> Option<bool> {
        self.skip_sep();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

/// Parses SVG path data into `path`.
pub fn parse_path_data(d: &str, path: &mut Path) -> Result<(), SvgError> {
    let mut t = Tokens::new(d);
    let mut cmd = 0u8;
    let mut cur: Pt = (0.0, 0.0);
    let mut start: Pt = (0.0, 0.0);
    let mut last_ctrl: Option<(u8, Pt)> = None;
    let err = |m: &str| SvgError(format!("path data: {m}"));
    loop {
        if let Some(c) = t.peek_cmd() {
            cmd = c;
            t.i += 1;
        } else if t.i >= t.s.len() {
            break;
        } else if cmd == 0 || cmd.eq_ignore_ascii_case(&b'z') {
            return Err(err("number without command"));
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur } else { (0.0, 0.0) };
        let n = |t: &mut Tokens| t.number().ok_or_else(|| err("expected number"));
        let pt = |t: &mut Tokens, b: Pt| -> Result<Pt, SvgError> {
            let x = n(t)?;
            let y = n(t)?;
            Ok((b.0 + x, b.1 + y))
        };
        let upper = cmd.to_ascii_uppercase();
        match upper {
            b'M' => {
                cur = pt(&mut t, base)?;
                start = cur;
                path.move_to(cur);
                // Subsequent pairs are implicit line-tos.
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                cur = pt(&mut t, base)?;
                path.line_to(cur);
            }
            b'H' => {
                let x = n(&mut t)?;
                cur = (if rel { cur.0 + x } else { x }, cur.1);
                path.line_to(cur);
            }
            b'V' => {
                let y = n(&mut t)?;
                cur = (cur.0, if rel { cur.1 + y } else { y });
                path.line_to(cur);
            }
            b'C' => {
                let c1 = pt(&mut t, base)?;
                let c2 = pt(&mut t, base)?;
                let p = pt(&mut t, base)?;
                path.cubic_to(c1, c2, p);
                last_ctrl = Some((b'C', c2));
                cur = p;
                continue;
            }
            b'S' => {
                let c1 = match last_ctrl {
                    Some((b'C', c)) => (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1),
                    _ => cur,
                };
                let c2 = pt(&mut t, base)?;
                let p = pt(&mut t, base)?;
                path.cubic_to(c1, c2, p);
                last_ctrl = Some((b'C', c2));
                cur = p;
                continue;
            }
            b'Q' => {
                let c = pt(&mut t, base)?;
                let p = pt(&mut t, base)?;
                path.quad_to(c, p);
                last_ctrl = Some((b'Q', c));
                cur = p;
                continue;
            }
            b'T' => {
                let c = match last_ctrl {
                    Some((b'Q', c)) => (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1),
                    _ => cur,
                };
                let p = pt(&mut t, base)?;
                path.quad_to(c, p);
                last_ctrl = Some((b'Q', c));
                cur = p;
                continue;
            }
            b'A' => {
                let rx = n(&mut t)?;
                let ry = n(&mut t)?;
                let rot = n(&mut t)?;
                let large = t.flag().ok_or_else(|| err("arc flag"))?;
                let sweep = t.flag().ok_or_else(|| err("arc flag"))?;
                let p = pt(&mut t, base)?;
                path.arc_to(rx, ry, rot, large, sweep, p);
                cur = p;
            }
            b'Z' => {
                path.close();
                cur = start;
            }
            _ => return Err(err("unknown command")),
        }
        last_ctrl = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_numbers_and_flags() {
        let mut p = Path::default();
        parse_path_data("M8.3 10a.7.7 0 0 1-.626-1.079L11.4 3z", &mut p).unwrap();
        assert_eq!(p.subs.len(), 1);
        assert!(p.subs[0].closed);
        let mut q = Path::default();
        parse_path_data("M2 2a1 1 0 011 1h5v-2", &mut q).unwrap();
        let l = q.last();
        assert!((l.0 - 8.0).abs() < 1e-4 && (l.1 - 1.0).abs() < 1e-4);
    }

    #[test]
    fn parses_elements() {
        let svg = r#"<svg viewBox="0 0 24 24"><rect width="18" height="18" x="3" y="3" rx="2" />
            <path d="m16 9-5.5 5.5L8 12" /><circle cx="12" cy="12" r="3"/><line x1="1" x2="2" y1="3" y2="4"/></svg>"#;
        let p = parse_icon(svg).unwrap();
        assert_eq!(p.subs.len(), 4);
        assert!(p.subs[0].closed);
        assert!(!p.subs[1].closed);
        assert!(p.subs[2].closed);
    }

    #[test]
    fn attr_does_not_match_suffix() {
        assert_eq!(attr(r#" cx="1" x="5""#, "x"), Some("5"));
    }
}
