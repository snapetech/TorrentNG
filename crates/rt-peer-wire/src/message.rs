/// BEP 3 peer wire messages.
use crate::error::WireError;

/// Hard cap: 2 MiB per message (largest legal piece block is 16 KiB, but allow some headroom).
pub const MAX_MESSAGE_LEN: u32 = 2 * 1024 * 1024;

/// Maximum block size a peer may request from us (BEP 3).
pub const MAX_BLOCK_SIZE: u32 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// length=0 keep-alive (no id byte).
    KeepAlive,
    /// id=0
    Choke,
    /// id=1
    Unchoke,
    /// id=2
    Interested,
    /// id=3
    NotInterested,
    /// id=4: piece index.
    Have(u32),
    /// id=5: bitfield (one bit per piece, MSB first).
    Bitfield(Vec<u8>),
    /// id=6: piece, begin, length.
    Request { piece: u32, begin: u32, length: u32 },
    /// id=7: piece index, begin offset, data block.
    Piece {
        piece: u32,
        begin: u32,
        data: Vec<u8>,
    },
    /// id=8: cancel a pending request.
    Cancel { piece: u32, begin: u32, length: u32 },
    /// id=20: BEP 10 extended message.
    Extended { ext_id: u8, payload: Vec<u8> },
}

impl Message {
    /// Encode into length-prefixed wire format.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Message::KeepAlive => vec![0, 0, 0, 0],
            Message::Choke => encode_fixed(0, &[]),
            Message::Unchoke => encode_fixed(1, &[]),
            Message::Interested => encode_fixed(2, &[]),
            Message::NotInterested => encode_fixed(3, &[]),
            Message::Have(idx) => encode_fixed(4, &idx.to_be_bytes()),
            Message::Bitfield(bits) => {
                let len = (1 + bits.len()) as u32;
                let mut v = Vec::with_capacity(4 + 1 + bits.len());
                v.extend_from_slice(&len.to_be_bytes());
                v.push(5);
                v.extend_from_slice(bits);
                v
            }
            Message::Request {
                piece,
                begin,
                length,
            } => encode_fixed(6, &encode_3u32(*piece, *begin, *length)),
            Message::Piece { piece, begin, data } => {
                let len = (1 + 4 + 4 + data.len()) as u32;
                let mut v = Vec::with_capacity(4 + 1 + 8 + data.len());
                v.extend_from_slice(&len.to_be_bytes());
                v.push(7);
                v.extend_from_slice(&piece.to_be_bytes());
                v.extend_from_slice(&begin.to_be_bytes());
                v.extend_from_slice(data);
                v
            }
            Message::Cancel {
                piece,
                begin,
                length,
            } => encode_fixed(8, &encode_3u32(*piece, *begin, *length)),
            Message::Extended { ext_id, payload } => {
                let len = (1 + 1 + payload.len()) as u32;
                let mut v = Vec::with_capacity(4 + 1 + 1 + payload.len());
                v.extend_from_slice(&len.to_be_bytes());
                v.push(20);
                v.push(*ext_id);
                v.extend_from_slice(payload);
                v
            }
        }
    }

    /// Parse a single message from a complete payload (after length prefix has been consumed).
    /// `payload` is the bytes after the 4-byte length field; empty = keepalive.
    pub fn parse(payload: &[u8]) -> Result<Self, WireError> {
        if payload.is_empty() {
            return Ok(Message::KeepAlive);
        }
        let id = payload[0];
        let body = &payload[1..];
        match id {
            0 => {
                expect_len(body, 0, "Choke")?;
                Ok(Message::Choke)
            }
            1 => {
                expect_len(body, 0, "Unchoke")?;
                Ok(Message::Unchoke)
            }
            2 => {
                expect_len(body, 0, "Interested")?;
                Ok(Message::Interested)
            }
            3 => {
                expect_len(body, 0, "NotInterested")?;
                Ok(Message::NotInterested)
            }
            4 => {
                expect_len(body, 4, "Have")?;
                Ok(Message::Have(u32::from_be_bytes(
                    body[..4].try_into().unwrap(),
                )))
            }
            5 => Ok(Message::Bitfield(body.to_vec())),
            6 => {
                expect_len(body, 12, "Request")?;
                let (piece, begin, length) = parse_3u32(body);
                validate_request(length)?;
                Ok(Message::Request {
                    piece,
                    begin,
                    length,
                })
            }
            7 => {
                if body.len() < 8 {
                    return Err(WireError::InvalidMessage("Piece body too short".into()));
                }
                let piece = u32::from_be_bytes(body[..4].try_into().unwrap());
                let begin = u32::from_be_bytes(body[4..8].try_into().unwrap());
                let data = body[8..].to_vec();
                if data.len() as u32 > MAX_BLOCK_SIZE {
                    return Err(WireError::InvalidMessage(format!(
                        "Piece block {} bytes exceeds MAX_BLOCK_SIZE {}",
                        data.len(),
                        MAX_BLOCK_SIZE
                    )));
                }
                Ok(Message::Piece { piece, begin, data })
            }
            8 => {
                expect_len(body, 12, "Cancel")?;
                let (piece, begin, length) = parse_3u32(body);
                Ok(Message::Cancel {
                    piece,
                    begin,
                    length,
                })
            }
            20 => {
                if body.is_empty() {
                    return Err(WireError::InvalidMessage(
                        "Extended body missing extension id".into(),
                    ));
                }
                Ok(Message::Extended {
                    ext_id: body[0],
                    payload: body[1..].to_vec(),
                })
            }
            _ => Err(WireError::UnknownMessageId(id)),
        }
    }
}

fn validate_request(length: u32) -> Result<(), WireError> {
    if length == 0 || length > MAX_BLOCK_SIZE {
        return Err(WireError::InvalidMessage(format!(
            "request length {length} not in 1..={MAX_BLOCK_SIZE}"
        )));
    }
    Ok(())
}

fn encode_fixed(id: u8, body: &[u8]) -> Vec<u8> {
    let len = (1 + body.len()) as u32;
    let mut v = Vec::with_capacity(4 + 1 + body.len());
    v.extend_from_slice(&len.to_be_bytes());
    v.push(id);
    v.extend_from_slice(body);
    v
}

fn encode_3u32(a: u32, b: u32, c: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&a.to_be_bytes());
    buf[4..8].copy_from_slice(&b.to_be_bytes());
    buf[8..12].copy_from_slice(&c.to_be_bytes());
    buf
}

fn parse_3u32(b: &[u8]) -> (u32, u32, u32) {
    (
        u32::from_be_bytes(b[0..4].try_into().unwrap()),
        u32::from_be_bytes(b[4..8].try_into().unwrap()),
        u32::from_be_bytes(b[8..12].try_into().unwrap()),
    )
}

fn expect_len(body: &[u8], expected: usize, name: &str) -> Result<(), WireError> {
    if body.len() != expected {
        return Err(WireError::InvalidMessage(format!(
            "{name} expected {expected} body bytes, got {}",
            body.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: Message) -> Message {
        let encoded = msg.encode();
        // Skip 4-byte length prefix
        Message::parse(&encoded[4..]).unwrap()
    }

    #[test]
    fn keepalive_roundtrip() {
        let encoded = Message::KeepAlive.encode();
        assert_eq!(encoded, [0, 0, 0, 0]);
        assert_eq!(Message::parse(&[]).unwrap(), Message::KeepAlive);
    }

    #[test]
    fn choke_roundtrip() {
        assert_eq!(roundtrip(Message::Choke), Message::Choke);
    }

    #[test]
    fn unchoke_roundtrip() {
        assert_eq!(roundtrip(Message::Unchoke), Message::Unchoke);
    }

    #[test]
    fn interested_roundtrip() {
        assert_eq!(roundtrip(Message::Interested), Message::Interested);
    }

    #[test]
    fn not_interested_roundtrip() {
        assert_eq!(roundtrip(Message::NotInterested), Message::NotInterested);
    }

    #[test]
    fn have_roundtrip() {
        assert_eq!(roundtrip(Message::Have(42)), Message::Have(42));
    }

    #[test]
    fn bitfield_roundtrip() {
        let bits = vec![0xFF, 0xA0, 0x00];
        let msg = Message::Bitfield(bits.clone());
        assert_eq!(roundtrip(msg), Message::Bitfield(bits));
    }

    #[test]
    fn request_roundtrip() {
        let msg = Message::Request {
            piece: 5,
            begin: 0,
            length: 16384,
        };
        assert_eq!(roundtrip(msg.clone()), msg);
    }

    #[test]
    fn request_rejects_oversized_block() {
        // MAX_BLOCK_SIZE + 1
        let payload = {
            let mut v = vec![6u8]; // id = request
            v.extend_from_slice(&0u32.to_be_bytes()); // piece
            v.extend_from_slice(&0u32.to_be_bytes()); // begin
            v.extend_from_slice(&(MAX_BLOCK_SIZE + 1).to_be_bytes());
            v
        };
        assert!(Message::parse(&payload).is_err());
    }

    #[test]
    fn request_rejects_zero_length() {
        let payload = {
            let mut v = vec![6u8];
            v.extend_from_slice(&0u32.to_be_bytes());
            v.extend_from_slice(&0u32.to_be_bytes());
            v.extend_from_slice(&0u32.to_be_bytes()); // length = 0
            v
        };
        assert!(Message::parse(&payload).is_err());
    }

    #[test]
    fn piece_roundtrip() {
        let data = vec![0xABu8; 1024];
        let msg = Message::Piece {
            piece: 3,
            begin: 16384,
            data: data.clone(),
        };
        assert_eq!(
            roundtrip(msg),
            Message::Piece {
                piece: 3,
                begin: 16384,
                data
            }
        );
    }

    #[test]
    fn piece_rejects_oversized_block() {
        let mut v = vec![7u8];
        v.extend_from_slice(&0u32.to_be_bytes()); // piece
        v.extend_from_slice(&0u32.to_be_bytes()); // begin
        v.extend(vec![0u8; MAX_BLOCK_SIZE as usize + 1]);
        assert!(Message::parse(&v).is_err());
    }

    #[test]
    fn cancel_roundtrip() {
        let msg = Message::Cancel {
            piece: 1,
            begin: 0,
            length: 16384,
        };
        assert_eq!(roundtrip(msg.clone()), msg);
    }

    #[test]
    fn extended_roundtrip() {
        let msg = Message::Extended {
            ext_id: 3,
            payload: b"d1:md11:ut_metadatai1eee".to_vec(),
        };
        assert_eq!(roundtrip(msg.clone()), msg);
        let encoded = msg.encode();
        let len = u32::from_be_bytes(encoded[..4].try_into().unwrap());
        assert_eq!(len as usize, encoded.len() - 4);
        assert_eq!(encoded[4], 20);
        assert_eq!(encoded[5], 3);
    }

    #[test]
    fn extended_rejects_missing_extension_id() {
        assert!(Message::parse(&[20u8]).is_err());
    }

    #[test]
    fn unknown_id_rejected() {
        assert!(Message::parse(&[100u8]).is_err());
    }

    #[test]
    fn message_length_prefix_correct() {
        let encoded = Message::Request {
            piece: 0,
            begin: 0,
            length: 16384,
        }
        .encode();
        // length prefix = 1 (id) + 12 (body) = 13
        let len = u32::from_be_bytes(encoded[..4].try_into().unwrap());
        assert_eq!(len, 13);
        assert_eq!(encoded.len(), 17);
    }
}
