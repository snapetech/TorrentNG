use crate::error::BencodeError;

const DEFAULT_MAX_DEPTH: usize = 64;
const DEFAULT_MAX_STRING: usize = 16 * 1024 * 1024; // 16 MiB

/// A borrowed bencode value. String values borrow from the input slice.
#[derive(Debug, PartialEq, Clone)]
pub enum BValue<'a> {
    Bytes(&'a [u8]),
    Int(i64),
    List(Vec<BValue<'a>>),
    Dict(Vec<(&'a [u8], BValue<'a>)>),
}

impl<'a> BValue<'a> {
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            BValue::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            BValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    pub fn get(&self, key: &[u8]) -> Option<&BValue<'a>> {
        match self {
            BValue::Dict(pairs) => pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

pub struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    max_depth: usize,
    max_string: usize,
    strict_dict_keys: bool,
}

impl<'a> Decoder<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            max_depth: DEFAULT_MAX_DEPTH,
            max_string: DEFAULT_MAX_STRING,
            strict_dict_keys: true,
        }
    }

    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    pub fn with_max_string(mut self, max: usize) -> Self {
        self.max_string = max;
        self
    }

    pub fn with_strict_dict_keys(mut self, strict: bool) -> Self {
        self.strict_dict_keys = strict;
        self
    }

    pub fn decode(mut self) -> Result<BValue<'a>, BencodeError> {
        let val = self.decode_value(0)?;
        if self.pos != self.input.len() {
            return Err(BencodeError::TrailingData);
        }
        Ok(val)
    }

    /// Decode a value and also return the byte span it occupied.
    pub fn decode_with_span(
        mut self,
    ) -> Result<(BValue<'a>, std::ops::Range<usize>), BencodeError> {
        let start = self.pos;
        let val = self.decode_value(0)?;
        if self.pos != self.input.len() {
            return Err(BencodeError::TrailingData);
        }
        Ok((val, start..self.pos))
    }

    fn peek(&self) -> Result<u8, BencodeError> {
        self.input
            .get(self.pos)
            .copied()
            .ok_or(BencodeError::UnexpectedEof)
    }

    fn consume(&mut self) -> Result<u8, BencodeError> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    fn expect(&mut self, expected: u8) -> Result<(), BencodeError> {
        let b = self.consume()?;
        if b != expected {
            Err(BencodeError::UnexpectedByte(b, self.pos - 1))
        } else {
            Ok(())
        }
    }

    fn decode_value(&mut self, depth: usize) -> Result<BValue<'a>, BencodeError> {
        if depth > self.max_depth {
            return Err(BencodeError::DepthExceeded(self.max_depth));
        }
        match self.peek()? {
            b'i' => self.decode_int(),
            b'l' => self.decode_list(depth),
            b'd' => self.decode_dict(depth),
            b'0'..=b'9' => self.decode_bytes(),
            b => Err(BencodeError::UnexpectedByte(b, self.pos)),
        }
    }

    fn decode_int(&mut self) -> Result<BValue<'a>, BencodeError> {
        self.expect(b'i')?;
        let start = self.pos;
        while self.peek()? != b'e' {
            self.pos += 1;
            if self.pos >= self.input.len() {
                return Err(BencodeError::UnexpectedEof);
            }
        }
        let digits = &self.input[start..self.pos];
        self.expect(b'e')?;

        let s = std::str::from_utf8(digits)
            .map_err(|_| BencodeError::InvalidInteger(format!("{digits:?}")))?;

        // Reject -0 and leading zeros
        if s == "-0" {
            return Err(BencodeError::InvalidInteger("-0".into()));
        }
        if s.len() > 1 && s.starts_with('0') {
            return Err(BencodeError::InvalidInteger(s.into()));
        }
        if s.len() > 2 && s.starts_with("-0") {
            return Err(BencodeError::InvalidInteger(s.into()));
        }

        let n = s
            .parse::<i64>()
            .map_err(|_| BencodeError::InvalidInteger(s.into()))?;

        Ok(BValue::Int(n))
    }

    fn decode_bytes(&mut self) -> Result<BValue<'a>, BencodeError> {
        let len = self.decode_length()?;
        if len > self.max_string {
            return Err(BencodeError::StringTooLong {
                len,
                max: self.max_string,
            });
        }
        let end = self.pos + len;
        if end > self.input.len() {
            return Err(BencodeError::UnexpectedEof);
        }
        let bytes = &self.input[self.pos..end];
        self.pos = end;
        Ok(BValue::Bytes(bytes))
    }

    fn decode_length(&mut self) -> Result<usize, BencodeError> {
        let start = self.pos;
        while self.peek()? != b':' {
            let b = self.input[self.pos];
            if !b.is_ascii_digit() {
                return Err(BencodeError::InvalidStringLength(format!(
                    "non-digit {b:#04x} at {}",
                    self.pos
                )));
            }
            self.pos += 1;
            if self.pos >= self.input.len() {
                return Err(BencodeError::UnexpectedEof);
            }
        }
        let s = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| BencodeError::InvalidStringLength("utf8".into()))?;
        if s.len() > 1 && s.starts_with('0') {
            return Err(BencodeError::InvalidStringLength(format!(
                "leading zero: {s}"
            )));
        }
        let n = s
            .parse::<usize>()
            .map_err(|e| BencodeError::InvalidStringLength(e.to_string()))?;
        self.expect(b':')?;
        Ok(n)
    }

    fn decode_list(&mut self, depth: usize) -> Result<BValue<'a>, BencodeError> {
        self.expect(b'l')?;
        let mut items = Vec::new();
        while self.peek()? != b'e' {
            items.push(self.decode_value(depth + 1)?);
        }
        self.expect(b'e')?;
        Ok(BValue::List(items))
    }

    fn decode_dict(&mut self, depth: usize) -> Result<BValue<'a>, BencodeError> {
        self.expect(b'd')?;
        let mut pairs: Vec<(&'a [u8], BValue<'a>)> = Vec::new();
        let mut last_key: Option<&[u8]> = None;
        while self.peek()? != b'e' {
            let key = match self.decode_bytes()? {
                BValue::Bytes(b) => b,
                _ => unreachable!(),
            };
            if self.strict_dict_keys {
                if let Some(prev) = last_key {
                    if key <= prev {
                        return Err(BencodeError::UnsortedDictKeys);
                    }
                }
            }
            last_key = Some(key);
            let val = self.decode_value(depth + 1)?;
            pairs.push((key, val));
        }
        self.expect(b'e')?;
        Ok(BValue::Dict(pairs))
    }
}

/// Decode a bencode value from a byte slice.
pub fn decode(input: &[u8]) -> Result<BValue<'_>, BencodeError> {
    Decoder::new(input).decode()
}

/// Decode and return the value along with the byte span of the `info` key's value,
/// which is needed for exact infohash computation.
pub fn decode_torrent_info_span(
    input: &[u8],
) -> Result<(BValue<'_>, Option<std::ops::Range<usize>>), BencodeError> {
    let mut dec = Decoder::new(input);
    let val = dec.decode_value(0)?;
    if dec.pos != input.len() {
        return Err(BencodeError::TrailingData);
    }

    // Find the byte span of the `info` value in the top-level dict
    let info_span = find_info_span(input);
    Ok((val, info_span))
}

fn find_info_span(input: &[u8]) -> Option<std::ops::Range<usize>> {
    // Walk the top-level dict looking for the "info" key, record value span
    let mut pos = 0;
    if input.get(pos) != Some(&b'd') {
        return None;
    }
    pos += 1;
    while pos < input.len() && input[pos] != b'e' {
        let key_start = pos;
        // parse key length
        let colon = memchr(b':', &input[pos..])?;
        let len_s = std::str::from_utf8(&input[pos..pos + colon]).ok()?;
        let key_len = len_s.parse::<usize>().ok()?;
        pos += colon + 1 + key_len;
        let key = &input[key_start + colon + 1..pos];
        let val_start = pos;
        // skip value
        pos = skip_value(input, pos)?;
        if key == b"info" {
            return Some(val_start..pos);
        }
    }
    None
}

fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

fn skip_value(input: &[u8], mut pos: usize) -> Option<usize> {
    match input.get(pos)? {
        b'i' => {
            pos += 1;
            while pos < input.len() && input[pos] != b'e' {
                pos += 1;
            }
            Some(pos + 1)
        }
        b'l' => {
            pos += 1;
            while pos < input.len() && input[pos] != b'e' {
                pos = skip_value(input, pos)?;
            }
            Some(pos + 1)
        }
        b'd' => {
            pos += 1;
            while pos < input.len() && input[pos] != b'e' {
                pos = skip_value(input, pos)?; // key
                pos = skip_value(input, pos)?; // value
            }
            Some(pos + 1)
        }
        b'0'..=b'9' => {
            let colon = memchr(b':', &input[pos..])?;
            let len_s = std::str::from_utf8(&input[pos..pos + colon]).ok()?;
            let len = len_s.parse::<usize>().ok()?;
            Some(pos + colon + 1 + len)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_integer() {
        assert_eq!(decode(b"i42e").unwrap(), BValue::Int(42));
        assert_eq!(decode(b"i-1e").unwrap(), BValue::Int(-1));
        assert_eq!(decode(b"i0e").unwrap(), BValue::Int(0));
    }

    #[test]
    fn reject_negative_zero() {
        assert!(decode(b"i-0e").is_err());
    }

    #[test]
    fn reject_leading_zero() {
        assert!(decode(b"i03e").is_err());
    }

    #[test]
    fn decode_string() {
        assert_eq!(decode(b"4:spam").unwrap(), BValue::Bytes(b"spam"));
        assert_eq!(decode(b"0:").unwrap(), BValue::Bytes(b""));
    }

    #[test]
    fn decode_list() {
        let v = decode(b"li1ei2ee").unwrap();
        assert_eq!(v, BValue::List(vec![BValue::Int(1), BValue::Int(2)]));
    }

    #[test]
    fn decode_dict() {
        let v = decode(b"d3:bar4:spam3:fooi42ee").unwrap();
        match v {
            BValue::Dict(pairs) => {
                assert_eq!(pairs[0].0, b"bar");
                assert_eq!(pairs[1].0, b"foo");
            }
            _ => panic!("expected dict"),
        }
    }

    #[test]
    fn reject_unsorted_dict_keys() {
        // "foo" < "bar" would be out of order
        assert!(decode(b"d3:fooi1e3:bari2ee").is_err());
    }

    #[test]
    fn reject_trailing_data() {
        assert!(decode(b"i1eX").is_err());
    }

    #[test]
    fn nested_structures() {
        let v = decode(b"ld3:keyi1eee").unwrap();
        assert!(matches!(v, BValue::List(_)));
    }
}
