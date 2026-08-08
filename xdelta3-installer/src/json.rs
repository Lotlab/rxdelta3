#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_object_with_escapes() {
        let v = Json::Object(vec![
            ("a".to_string(), Json::String("hello\"\\world".to_string())),
            (
                "b".to_string(),
                Json::Array(vec![Json::String("x".into()), Json::String("y".into())]),
            ),
        ]);
        let s = to_string(&v);
        let back = parse(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn parse_errors() {
        assert!(parse("{").is_err());
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("").is_err());
        assert!(parse("{\"a\":1}").is_err()); // numbers unsupported
    }

    #[test]
    fn get_accessors() {
        let v = parse(r#"{"k":"v","arr":["x"]}"#).unwrap();
        assert_eq!(v.get("k").and_then(Json::as_str), Some("v"));
        assert_eq!(v.get("arr").and_then(Json::as_array).unwrap()[0].as_str(), Some("x"));
        assert_eq!(v.get("nope"), None);
    }
}

use std::fmt;

/// Minimal JSON subset: objects, arrays, strings. Enough for the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Object(Vec<(String, Json)>),
    Array(Vec<Json>),
    String(String),
}

impl Json {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

pub fn parse(input: &str) -> Result<Json, String> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    let v = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(format!("trailing data at byte {}", p.pos));
    }
    Ok(v)
}

pub fn to_string(v: &Json) -> String {
    let mut out = String::new();
    write_json(v, &mut out);
    out
}

fn write_json(v: &Json, out: &mut String) {
    match v {
        Json::Object(entries) => {
            out.push('{');
            for (i, (k, val)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_json(val, out);
            }
            out.push('}');
        }
        Json::Array(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(it, out);
            }
            out.push(']');
        }
        Json::String(s) => write_string(s, out),
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", b as char, self.pos))
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Json::String(self.parse_string()?)),
            Some(other) => Err(format!("unexpected byte {:?} at {}", other as char, self.pos)),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        self.skip_ws();
        let mut out = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(out));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err("expected string key".to_string());
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let val = self.parse_value()?;
            out.push((key, val));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err("expected ',' or '}'".to_string()),
            }
        }
        Ok(Json::Object(out))
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        self.skip_ws();
        let mut out = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(out));
        }
        loop {
            let val = self.parse_value()?;
            out.push(val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err("expected ',' or ']'".to_string()),
            }
        }
        Ok(Json::Array(out))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let b = self.peek().ok_or_else(|| "unterminated string".to_string())?;
            self.pos += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let e = self.peek().ok_or_else(|| "bad escape".to_string())?;
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self.take(4)?;
                            let cp = u32::from_str_radix(&hex, 16)
                                .map_err(|_| "bad \\u escape".to_string())?;
                            out.push(char::from_u32(cp).ok_or_else(|| "bad \\u codepoint".to_string())?);
                        }
                        _ => return Err("bad escape".to_string()),
                    }
                }
                b if b < 0x80 => out.push(b as char),
                _ => return Err("non-ASCII in string; manifest is ASCII-only".to_string()),
            }
        }
        Ok(out)
    }

    fn take(&mut self, n: usize) -> Result<String, String> {
        if self.pos + n > self.bytes.len() {
            return Err("unexpected end of input".to_string());
        }
        let s = String::from_utf8_lossy(&self.bytes[self.pos..self.pos + n]).to_string();
        self.pos += n;
        Ok(s)
    }
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", to_string(self))
    }
}
