//! A small JSON reader, enough for GNOME Shell's keyboard layout files
//! (the panel has no JSON crate).

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_array(&self) -> &[Value] {
        match self {
            Value::Array(items) => items,
            _ => &[],
        }
    }
}

pub fn parse(text: &str) -> Result<Value, String> {
    let mut parser = Parser { chars: text.chars().collect(), pos: 0 };
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(format!("trailing data at {}", parser.pos));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Result<char, String> {
        let c = self.peek().ok_or("unexpected end")?;
        self.pos += 1;
        Ok(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, word: &str) -> Result<(), String> {
        for c in word.chars() {
            if self.next()? != c {
                return Err(format!("expected {word} at {}", self.pos));
            }
        }
        Ok(())
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek().ok_or("unexpected end")? {
            '{' => self.object(),
            '[' => self.array(),
            '"' => self.string().map(Value::String),
            't' => self.expect("true").map(|_| Value::Bool(true)),
            'f' => self.expect("false").map(|_| Value::Bool(false)),
            'n' => self.expect("null").map(|_| Value::Null),
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Object(members));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            if self.next()? != ':' {
                return Err(format!("expected : at {}", self.pos));
            }
            members.push((key, self.value()?));
            self.skip_ws();
            match self.next()? {
                ',' => continue,
                '}' => return Ok(Value::Object(members)),
                c => return Err(format!("unexpected {c:?} at {}", self.pos)),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.next()? {
                ',' => continue,
                ']' => return Ok(Value::Array(items)),
                c => return Err(format!("unexpected {c:?} at {}", self.pos)),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut code = 0;
        for _ in 0..4 {
            code = code * 16 + self.next()?.to_digit(16).ok_or("bad \\u escape")?;
        }
        Ok(code)
    }

    fn string(&mut self) -> Result<String, String> {
        if self.next()? != '"' {
            return Err(format!("expected string at {}", self.pos));
        }
        let mut out = String::new();
        loop {
            match self.next()? {
                '"' => return Ok(out),
                '\\' => match self.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let mut code = self.hex4()?;
                        // A UTF-16 surrogate pair.
                        if (0xd800..0xdc00).contains(&code) {
                            self.expect("\\u")?;
                            let low = self.hex4()?;
                            code = 0x10000 + ((code - 0xd800) << 10) + (low.wrapping_sub(0xdc00) & 0x3ff);
                        }
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || "+-.eE".contains(c)) {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse().map(Value::Number).map_err(|_| format!("bad number {text:?} at {start}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_layout_like_json() {
        let v = parse(r#"{"a": [1, 2.5, "xé\"", true, null], "b": {}}"#).unwrap();
        assert_eq!(v.get("a").unwrap().as_array().len(), 5);
        assert_eq!(v.get("a").unwrap().as_array()[2].as_str(), Some("xé\""));
        assert_eq!(v.get("a").unwrap().as_array()[1].as_f64(), Some(2.5));
    }
}
