//! Minimal JSON support.
//!
//! The agent only needs to *emit* one fixed payload shape and *read back* a
//! handful of fields from the panel's responses, so pulling in `serde` would be
//! overkill. This module therefore provides:
//!
//! * `escape_into` / `num_to_string` — helpers used by the payload writer.
//! * `parse` — a small recursive-descent parser producing [`Json`].
//!
//! It is intentionally strict-but-forgiving: it accepts any well-formed JSON
//! document (including UTF-8 strings with escapes and surrogate pairs) and does
//! not enforce schema-level constraints.

use std::fmt;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Look up a key on an object value.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(map) => map.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Walk a chain of object keys, e.g. `["error", "message"]`.
    pub fn get_path(&self, path: &[&str]) -> Option<&Json> {
        let mut cur = self;
        for key in path {
            cur = cur.get(key)?;
        }
        Some(cur)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            // Some servers serialise integers as strings; be forgiving here.
            Json::Str(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        let n = self.as_f64()?;
        if !n.is_finite() || n < 0.0 {
            return None;
        }
        Some(n.round() as u64)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            Json::Num(n) => Some(*n != 0.0),
            Json::Str(s) => match s.as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Append a JSON string *body* (no surrounding quotes) with all required
/// escapes applied.
pub fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

/// Render a float the way the API expects: integers without a decimal point,
/// finite values otherwise at full precision.
pub fn num_to_string(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{}", v);
        s
    }
}

/// Build a JSON object incrementally, tracking comma placement.
pub struct ObjBuilder {
    buf: String,
    first: bool,
}

impl ObjBuilder {
    pub fn new() -> Self {
        ObjBuilder {
            buf: String::new(),
            first: true,
        }
    }

    fn sep(&mut self, key: &str) {
        if !self.first {
            self.buf.push(',');
        }
        self.first = false;
        self.buf.push('"');
        escape_into(&mut self.buf, key);
        self.buf.push('"');
        self.buf.push(':');
    }

    pub fn string(&mut self, key: &str, val: &str) {
        self.sep(key);
        self.buf.push('"');
        escape_into(&mut self.buf, val);
        self.buf.push('"');
    }

    pub fn opt_string(&mut self, key: &str, val: &Option<String>) {
        if let Some(v) = val {
            self.string(key, v);
        }
    }

    pub fn int(&mut self, key: &str, val: i64) {
        self.sep(key);
        self.buf.push_str(&val.to_string());
    }

    pub fn opt_int(&mut self, key: &str, val: Option<i64>) {
        if let Some(v) = val {
            self.int(key, v);
        }
    }

    pub fn float(&mut self, key: &str, val: f64) {
        self.sep(key);
        self.buf.push_str(&num_to_string(val));
    }

    pub fn opt_float(&mut self, key: &str, val: Option<f64>) {
        if let Some(v) = val {
            self.float(key, v);
        }
    }

    /// Insert a pre-serialised JSON fragment (already valid JSON).
    pub fn raw(&mut self, key: &str, raw: &str) {
        self.sep(key);
        self.buf.push_str(raw);
    }

    pub fn is_empty(&self) -> bool {
        self.first
    }

    /// Finish building and return the object *body* (without braces).
    pub fn into_body(self) -> String {
        self.buf
    }

    /// Finish building and return a complete `{...}` document.
    pub fn into_object(self) -> String {
        let mut out = String::with_capacity(self.buf.len() + 2);
        out.push('{');
        out.push_str(&self.buf);
        out.push('}');
        out
    }
}

impl Default for ObjBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a complete JSON document.
pub fn parse(input: &str) -> Result<Json, String> {
    let bytes = input.as_bytes();
    let mut p = Parser { b: bytes, i: 0 };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    if p.i != p.b.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, want: u8) -> Result<(), String> {
        match self.bump() {
            Some(c) if c == want => Ok(()),
            Some(c) => Err(format!(
                "expected '{}' at byte {}, found '{}'",
                want as char,
                self.i - 1,
                c as char
            )),
            None => Err(format!("unexpected end of input, expected '{}'", want as char)),
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            None => Err("unexpected end of input".to_string()),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Json::Str(self.parse_string()?)),
            Some(b't') => {
                self.parse_literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.parse_literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.parse_literal("null")?;
                Ok(Json::Null)
            }
            Some(_) => self.parse_number(),
        }
    }

    fn parse_literal(&mut self, lit: &str) -> Result<(), String> {
        let bytes = lit.as_bytes();
        if self.b.len() < self.i + bytes.len() {
            return Err("truncated literal".to_string());
        }
        if &self.b[self.i..self.i + bytes.len()] != bytes {
            return Err(format!("invalid literal at byte {}", self.i));
        }
        self.i += bytes.len();
        Ok(())
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut map: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(map));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let val = self.parse_value()?;
            map.push((key, val));
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => break,
                Some(c) => return Err(format!("expected ',' or '}}' at byte {}, found '{}'", self.i - 1, c as char)),
                None => return Err("unexpected end of input in object".to_string()),
            }
        }
        Ok(Json::Obj(map))
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut arr: Vec<Json> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(arr));
        }
        loop {
            let val = self.parse_value()?;
            arr.push(val);
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => break,
                Some(c) => return Err(format!("expected ',' or ']' at byte {}, found '{}'", self.i - 1, c as char)),
                None => return Err("unexpected end of input in array".to_string()),
            }
        }
        Ok(Json::Arr(arr))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = match self.bump() {
                Some(c) => c,
                None => return Err("unterminated string".to_string()),
            };
            match c {
                b'"' => break,
                b'\\' => {
                    let esc = self
                        .bump()
                        .ok_or_else(|| "unterminated escape".to_string())?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&cp) {
                                // High surrogate: expect a following low surrogate.
                                if self.peek() == Some(b'\\') {
                                    self.i += 1;
                                    if self.peek() == Some(b'u') {
                                        self.i += 1;
                                        let low = self.parse_hex4()?;
                                        if (0xDC00..=0xDFFF).contains(&low) {
                                            let full = 0x10000
                                                + ((cp - 0xD800) << 10)
                                                + (low - 0xDC00);
                                            if let Some(ch) = char::from_u32(full) {
                                                out.push(ch);
                                            }
                                        } else {
                                            out.push('\u{FFFD}');
                                        }
                                    } else {
                                        out.push('\u{FFFD}');
                                    }
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                            } else {
                                out.push('\u{FFFD}');
                            }
                        }
                        other => {
                            return Err(format!("invalid escape '\\{}'", other as char));
                        }
                    }
                }
                c if c < 0x80 => out.push(c as char),
                _ => {
                    // Multi-byte UTF-8: re-decode from the full slice starting at
                    // the byte we just consumed.
                    let start = self.i - 1;
                    let width = utf8_width(self.b[start]);
                    if width == 0 || start + width > self.b.len() {
                        return Err(format!("invalid utf-8 at byte {}", start));
                    }
                    match std::str::from_utf8(&self.b[start..start + width]) {
                        Ok(s) => {
                            out.push_str(s);
                            self.i = start + width;
                        }
                        Err(_) => return Err(format!("invalid utf-8 at byte {}", start)),
                    }
                }
            }
        }
        Ok(out)
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return Err("truncated \\u escape".to_string());
        }
        let mut v: u32 = 0;
        for k in 0..4 {
            let c = self.b[self.i + k];
            let d = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a' + 10) as u32,
                b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return Err(format!("invalid hex digit at byte {}", self.i + k)),
            };
            v = (v << 4) | d;
        }
        self.i += 4;
        Ok(v)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') || self.peek() == Some(b'+') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || c == b'+' || c == b'-'
            {
                self.i += 1;
            } else {
                break;
            }
        }
        if start == self.i {
            return Err(format!("invalid value at byte {}", start));
        }
        let text = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| "invalid number bytes".to_string())?;
        let n = text
            .parse::<f64>()
            .map_err(|_| format!("invalid number '{}'", text))?;
        Ok(Json::Num(n))
    }
}

fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_document() {
        let doc = r#"{"ok":true,"error":{"code":"invalid_api_key","message":"bad"},"n":12.5,"arr":[1,2,3],"s":"a\nb","z":null}"#;
        let v = parse(doc).expect("should parse");
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(
            v.get_path(&["error", "code"]).and_then(|x| x.as_str()),
            Some("invalid_api_key")
        );
        assert_eq!(v.get("n").and_then(|x| x.as_f64()), Some(12.5));
        assert_eq!(v.get("s").and_then(|x| x.as_str()), Some("a\nb"));
        assert_eq!(v.get("z"), Some(&Json::Null));
    }

    #[test]
    fn reads_interval() {
        let v = parse(r#"{"report_interval":60,"next_report_at":"x"}"#).unwrap();
        assert_eq!(v.get("report_interval").and_then(|x| x.as_u64()), Some(60));
    }

    #[test]
    fn unicode_escapes() {
        let v = parse(r#""\u4e2d\u6587""#).unwrap();
        assert_eq!(v.as_str(), Some("中文"));
    }

    #[test]
    fn escapes_strings() {
        let mut s = String::new();
        escape_into(&mut s, "a\"b\\c\n");
        assert_eq!(s, "a\\\"b\\\\c\\n");
    }

    #[test]
    fn builds_object() {
        let mut o = ObjBuilder::new();
        o.int("a", 1);
        o.string("b", "x");
        o.opt_int("c", None);
        assert_eq!(o.into_object(), r#"{"a":1,"b":"x"}"#);
    }
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(b) => write!(f, "{}", b),
            Json::Num(n) => f.write_str(&num_to_string(*n)),
            Json::Str(s) => {
                let mut t = String::new();
                t.push('"');
                escape_into(&mut t, s);
                t.push('"');
                f.write_str(&t)
            }
            Json::Arr(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}", item)?;
                }
                f.write_str("]")
            }
            Json::Obj(map) => {
                f.write_str("{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    let mut t = String::new();
                    t.push('"');
                    escape_into(&mut t, k);
                    t.push('"');
                    f.write_str(&t)?;
                    f.write_str(":")?;
                    write!(f, "{}", v)?;
                }
                f.write_str("}")
            }
        }
    }
}
