use std::collections::HashMap;

use rt_bencode::{decode, encode, BValue};

use crate::error::WireError;

/// BEP 10 extended handshake message id.
pub const EXT_HANDSHAKE_ID: u8 = 0;

/// BEP 9 metadata extension name.
pub const UT_METADATA: &str = "ut_metadata";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionHandshake {
    pub metadata_size: Option<u32>,
    pub extensions: HashMap<String, u8>,
}

impl ExtensionHandshake {
    pub fn new(metadata_size: Option<u32>) -> Self {
        Self {
            metadata_size,
            extensions: HashMap::new(),
        }
    }

    pub fn with_ut_metadata(mut self, id: u8) -> Self {
        self.extensions.insert(UT_METADATA.to_owned(), id);
        self
    }

    pub fn ut_metadata_id(&self) -> Option<u8> {
        self.extensions.get(UT_METADATA).copied()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut extension_pairs = self
            .extensions
            .iter()
            .map(|(name, id)| (name.as_bytes(), BValue::Int(i64::from(*id))))
            .collect::<Vec<_>>();
        extension_pairs.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut pairs = vec![(b"m".as_slice(), BValue::Dict(extension_pairs))];
        if let Some(size) = self.metadata_size {
            pairs.push((b"metadata_size".as_slice(), BValue::Int(i64::from(size))));
        }
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        encode(&BValue::Dict(pairs))
    }

    pub fn parse(payload: &[u8]) -> Result<Self, WireError> {
        let value = decode(payload).map_err(invalid)?;
        let dict = match value {
            BValue::Dict(pairs) => pairs,
            _ => return Err(invalid("extension handshake must be a dict")),
        };

        let metadata_size = dict_int(&dict, b"metadata_size")
            .map(|value| u32::try_from(value).map_err(|_| invalid("metadata_size out of range")))
            .transpose()?;

        let mut extensions = HashMap::new();
        if let Some(BValue::Dict(pairs)) = dict.iter().find(|(k, _)| *k == b"m").map(|(_, v)| v) {
            for (name, value) in pairs {
                let Some(id) = value.as_int() else {
                    continue;
                };
                if id <= 0 || id > u8::MAX.into() {
                    continue;
                }
                let Ok(name) = std::str::from_utf8(name) else {
                    continue;
                };
                extensions.insert(name.to_owned(), id as u8);
            }
        }

        Ok(Self {
            metadata_size,
            extensions,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UtMetadataMessage {
    Request {
        piece: u32,
    },
    Data {
        piece: u32,
        total_size: u32,
        data: Vec<u8>,
    },
    Reject {
        piece: u32,
    },
}

impl UtMetadataMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut header = match self {
            UtMetadataMessage::Request { piece } => metadata_header(0, *piece, None),
            UtMetadataMessage::Data {
                piece, total_size, ..
            } => metadata_header(1, *piece, Some(*total_size)),
            UtMetadataMessage::Reject { piece } => metadata_header(2, *piece, None),
        };
        if let UtMetadataMessage::Data { data, .. } = self {
            header.extend_from_slice(data);
        }
        header
    }

    pub fn parse(payload: &[u8]) -> Result<Self, WireError> {
        let header_len = bencode_value_len(payload).map_err(invalid)?;
        let header = decode(&payload[..header_len]).map_err(invalid)?;
        let BValue::Dict(pairs) = header else {
            return Err(invalid("ut_metadata header must be a dict"));
        };

        let msg_type =
            dict_int(&pairs, b"msg_type").ok_or_else(|| invalid("ut_metadata msg_type missing"))?;
        let piece =
            dict_int(&pairs, b"piece").ok_or_else(|| invalid("ut_metadata piece missing"))?;
        let piece = u32::try_from(piece).map_err(|_| invalid("ut_metadata piece out of range"))?;

        match msg_type {
            0 => {
                if header_len != payload.len() {
                    return Err(invalid("ut_metadata request has trailing data"));
                }
                Ok(UtMetadataMessage::Request { piece })
            }
            1 => {
                let total_size = dict_int(&pairs, b"total_size")
                    .ok_or_else(|| invalid("ut_metadata total_size missing"))?;
                let total_size = u32::try_from(total_size)
                    .map_err(|_| invalid("ut_metadata total_size out of range"))?;
                Ok(UtMetadataMessage::Data {
                    piece,
                    total_size,
                    data: payload[header_len..].to_vec(),
                })
            }
            2 => {
                if header_len != payload.len() {
                    return Err(invalid("ut_metadata reject has trailing data"));
                }
                Ok(UtMetadataMessage::Reject { piece })
            }
            other => Err(invalid(format!("unknown ut_metadata msg_type {other}"))),
        }
    }
}

fn metadata_header(msg_type: i64, piece: u32, total_size: Option<u32>) -> Vec<u8> {
    let mut pairs = vec![
        (b"msg_type".as_slice(), BValue::Int(msg_type)),
        (b"piece".as_slice(), BValue::Int(i64::from(piece))),
    ];
    if let Some(total_size) = total_size {
        pairs.push((b"total_size".as_slice(), BValue::Int(i64::from(total_size))));
    }
    pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
    encode(&BValue::Dict(pairs))
}

fn dict_int(pairs: &[(&[u8], BValue<'_>)], key: &[u8]) -> Option<i64> {
    pairs
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .and_then(|(_, value)| value.as_int())
}

fn bencode_value_len(input: &[u8]) -> Result<usize, &'static str> {
    let mut pos = 0;
    scan_value(input, &mut pos)?;
    Ok(pos)
}

fn scan_value(input: &[u8], pos: &mut usize) -> Result<(), &'static str> {
    match input.get(*pos).copied() {
        Some(b'i') => scan_int(input, pos),
        Some(b'l') => scan_list(input, pos),
        Some(b'd') => scan_dict(input, pos),
        Some(b'0'..=b'9') => scan_bytes(input, pos),
        Some(_) => Err("invalid bencode value"),
        None => Err("empty bencode payload"),
    }
}

fn scan_int(input: &[u8], pos: &mut usize) -> Result<(), &'static str> {
    *pos += 1;
    while let Some(byte) = input.get(*pos).copied() {
        *pos += 1;
        if byte == b'e' {
            return Ok(());
        }
    }
    Err("unterminated bencode int")
}

fn scan_list(input: &[u8], pos: &mut usize) -> Result<(), &'static str> {
    *pos += 1;
    loop {
        match input.get(*pos).copied() {
            Some(b'e') => {
                *pos += 1;
                return Ok(());
            }
            Some(_) => scan_value(input, pos)?,
            None => return Err("unterminated bencode list"),
        }
    }
}

fn scan_dict(input: &[u8], pos: &mut usize) -> Result<(), &'static str> {
    *pos += 1;
    loop {
        match input.get(*pos).copied() {
            Some(b'e') => {
                *pos += 1;
                return Ok(());
            }
            Some(b'0'..=b'9') => {
                scan_bytes(input, pos)?;
                scan_value(input, pos)?;
            }
            Some(_) => return Err("invalid bencode dict key"),
            None => return Err("unterminated bencode dict"),
        }
    }
}

fn scan_bytes(input: &[u8], pos: &mut usize) -> Result<(), &'static str> {
    let start = *pos;
    while let Some(byte) = input.get(*pos).copied() {
        *pos += 1;
        if byte == b':' {
            let len = std::str::from_utf8(&input[start..(*pos - 1)])
                .map_err(|_| "invalid bencode byte length")?
                .parse::<usize>()
                .map_err(|_| "invalid bencode byte length")?;
            let end = pos.checked_add(len).ok_or("bencode byte length overflow")?;
            if end > input.len() {
                return Err("bencode byte string exceeds payload");
            }
            *pos = end;
            return Ok(());
        }
        if !byte.is_ascii_digit() {
            return Err("invalid bencode byte length");
        }
    }
    Err("unterminated bencode byte length")
}

fn invalid(message: impl ToString) -> WireError {
    WireError::InvalidMessage(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_handshake_roundtrip() {
        let handshake = ExtensionHandshake::new(Some(1234)).with_ut_metadata(3);
        let parsed = ExtensionHandshake::parse(&handshake.encode()).unwrap();

        assert_eq!(parsed.metadata_size, Some(1234));
        assert_eq!(parsed.ut_metadata_id(), Some(3));
    }

    #[test]
    fn ut_metadata_request_roundtrip() {
        let message = UtMetadataMessage::Request { piece: 7 };
        assert_eq!(
            UtMetadataMessage::parse(&message.encode()).unwrap(),
            UtMetadataMessage::Request { piece: 7 }
        );
    }

    #[test]
    fn ut_metadata_data_roundtrip() {
        let message = UtMetadataMessage::Data {
            piece: 1,
            total_size: 12,
            data: b"metadata".to_vec(),
        };
        assert_eq!(
            UtMetadataMessage::parse(&message.encode()).unwrap(),
            message
        );
    }

    #[test]
    fn ut_metadata_reject_roundtrip() {
        let message = UtMetadataMessage::Reject { piece: 9 };
        assert_eq!(
            UtMetadataMessage::parse(&message.encode()).unwrap(),
            message
        );
    }
}
