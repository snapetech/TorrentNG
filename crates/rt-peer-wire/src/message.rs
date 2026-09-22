/// BEP 3 peer wire messages.
use crate::error::WireError;
use bytes::{BufMut, Bytes, BytesMut};

/// Hard cap: 2 MiB per message (largest legal piece block is 16 KiB, but allow some headroom).
pub const MAX_MESSAGE_LEN: u32 = 2 * 1024 * 1024;

/// Maximum block size a peer may request from us (BEP 3).
pub const MAX_BLOCK_SIZE: u32 = 16 * 1024;

/// BEP 52's fixed hash-request header, excluding the peer-wire message id.
const HASH_EXCHANGE_HEADER_LEN: usize = 32 + (4 * 4);

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
    ///
    /// `data` is `Bytes` rather than `Vec<u8>` so a block read from storage
    /// (already `Bytes`) can be moved/cloned onto the wire with a cheap
    /// refcount bump instead of a full memory copy on the seeding hot path.
    Piece { piece: u32, begin: u32, data: Bytes },
    /// id=8: cancel a pending request.
    Cancel { piece: u32, begin: u32, length: u32 },
    /// id=16: BEP 6 Fast extension — reject a request that cannot be served.
    Reject { piece: u32, begin: u32, length: u32 },
    /// id=14 (0x0E): BEP 6 Fast extension — sender has every piece. Sent in
    /// place of a full `Bitfield` when the local peer is complete and the
    /// remote negotiated the Fast extension in the handshake.
    HaveAll,
    /// id=15 (0x0F): BEP 6 Fast extension — sender has no pieces. Sent in
    /// place of an empty/omitted `Bitfield` when the remote negotiated the
    /// Fast extension in the handshake.
    HaveNone,
    /// id=20: BEP 10 extended message.
    Extended { ext_id: u8, payload: Vec<u8> },
    /// id=21: BEP 52 hash request.
    HashRequest {
        pieces_root: [u8; 32],
        base_layer: u32,
        index: u32,
        length: u32,
        proof_layers: u32,
    },
    /// id=22: BEP 52 hashes response.
    Hashes {
        pieces_root: [u8; 32],
        base_layer: u32,
        index: u32,
        length: u32,
        proof_layers: u32,
        hashes: Vec<[u8; 32]>,
    },
    /// id=23: BEP 52 hash request rejection.
    HashReject {
        pieces_root: [u8; 32],
        base_layer: u32,
        index: u32,
        length: u32,
        proof_layers: u32,
    },
}

impl Message {
    /// Return the complete length of the encoded message, including its
    /// four-byte length prefix. `None` means the message cannot be represented
    /// as a legal peer-wire frame.
    pub fn encoded_len(&self) -> Option<usize> {
        let message_len = match self {
            Message::KeepAlive => 0,
            Message::Choke
            | Message::Unchoke
            | Message::Interested
            | Message::NotInterested
            | Message::HaveAll
            | Message::HaveNone => 1,
            Message::Have(_) => 5,
            Message::Bitfield(bits) => 1usize.checked_add(bits.len())?,
            Message::Request { .. } | Message::Cancel { .. } | Message::Reject { .. } => 13,
            Message::Piece { data, .. } => 9usize.checked_add(data.len())?,
            Message::Extended { payload, .. } => 2usize.checked_add(payload.len())?,
            Message::HashRequest { index, length, .. }
            | Message::HashReject { index, length, .. } => {
                let _ = (index, length);
                1usize.checked_add(HASH_EXCHANGE_HEADER_LEN)?
            }
            Message::Hashes { hashes, .. } => 1usize
                .checked_add(HASH_EXCHANGE_HEADER_LEN)?
                .checked_add(hashes.len().checked_mul(32)?)?,
        };
        if message_len > MAX_MESSAGE_LEN as usize {
            return None;
        }
        4usize.checked_add(message_len)
    }

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
            Message::Reject {
                piece,
                begin,
                length,
            } => encode_fixed(16, &encode_3u32(*piece, *begin, *length)),
            Message::HaveAll => encode_fixed(14, &[]),
            Message::HaveNone => encode_fixed(15, &[]),
            Message::Extended { ext_id, payload } => {
                let len = (1 + 1 + payload.len()) as u32;
                let mut v = Vec::with_capacity(4 + 1 + 1 + payload.len());
                v.extend_from_slice(&len.to_be_bytes());
                v.push(20);
                v.push(*ext_id);
                v.extend_from_slice(payload);
                v
            }
            Message::HashRequest {
                pieces_root,
                base_layer,
                index,
                length,
                proof_layers,
            } => encode_hash_exchange(
                21,
                pieces_root,
                *base_layer,
                *index,
                *length,
                *proof_layers,
                &[],
            ),
            Message::Hashes {
                pieces_root,
                base_layer,
                index,
                length,
                proof_layers,
                hashes,
            } => {
                let mut hash_bytes = Vec::with_capacity(hashes.len().saturating_mul(32));
                for hash in hashes {
                    hash_bytes.extend_from_slice(hash);
                }
                encode_hash_exchange(
                    22,
                    pieces_root,
                    *base_layer,
                    *index,
                    *length,
                    *proof_layers,
                    &hash_bytes,
                )
            }
            Message::HashReject {
                pieces_root,
                base_layer,
                index,
                length,
                proof_layers,
            } => encode_hash_exchange(
                23,
                pieces_root,
                *base_layer,
                *index,
                *length,
                *proof_layers,
                &[],
            ),
        }
    }

    /// Append the length-prefixed wire frame directly to a reusable buffer.
    ///
    /// The regular [`Message::encode`] API remains convenient for protocol
    /// callers that need an owned `Vec<u8>`. Engine socket paths can use this
    /// method to avoid constructing that temporary vector before copying the
    /// frame into a retained write buffer.
    pub fn encode_into(&self, dst: &mut BytesMut) -> Result<(), WireError> {
        let encoded_len = self
            .encoded_len()
            .ok_or(WireError::MessageTooLarge(u32::MAX))?;
        if dst.remaining_mut() < encoded_len {
            dst.reserve(encoded_len - dst.remaining_mut());
        }

        match self {
            Message::KeepAlive => dst.put_u32(0),
            Message::Choke => put_fixed(dst, 0, &[]),
            Message::Unchoke => put_fixed(dst, 1, &[]),
            Message::Interested => put_fixed(dst, 2, &[]),
            Message::NotInterested => put_fixed(dst, 3, &[]),
            Message::Have(idx) => put_fixed(dst, 4, &idx.to_be_bytes()),
            Message::Bitfield(bits) => {
                dst.put_u32((1 + bits.len()) as u32);
                dst.put_u8(5);
                dst.put_slice(bits);
            }
            Message::Request {
                piece,
                begin,
                length,
            } => put_fixed(dst, 6, &encode_3u32(*piece, *begin, *length)),
            Message::Piece { piece, begin, data } => {
                dst.put_u32((1 + 4 + 4 + data.len()) as u32);
                dst.put_u8(7);
                dst.put_u32(*piece);
                dst.put_u32(*begin);
                dst.put_slice(data);
            }
            Message::Cancel {
                piece,
                begin,
                length,
            } => put_fixed(dst, 8, &encode_3u32(*piece, *begin, *length)),
            Message::Reject {
                piece,
                begin,
                length,
            } => put_fixed(dst, 16, &encode_3u32(*piece, *begin, *length)),
            Message::HaveAll => put_fixed(dst, 14, &[]),
            Message::HaveNone => put_fixed(dst, 15, &[]),
            Message::Extended { ext_id, payload } => {
                dst.put_u32((1 + 1 + payload.len()) as u32);
                dst.put_u8(20);
                dst.put_u8(*ext_id);
                dst.put_slice(payload);
            }
            Message::HashRequest {
                pieces_root,
                base_layer,
                index,
                length,
                proof_layers,
            } => put_hash_exchange(
                dst,
                21,
                &HashExchangeHeader {
                    pieces_root,
                    base_layer: *base_layer,
                    index: *index,
                    length: *length,
                    proof_layers: *proof_layers,
                },
                &[],
            ),
            Message::Hashes {
                pieces_root,
                base_layer,
                index,
                length,
                proof_layers,
                hashes,
            } => {
                let hashes_len = hashes.len().saturating_mul(32);
                let message_len = 1 + HASH_EXCHANGE_HEADER_LEN + hashes_len;
                dst.put_u32(message_len as u32);
                dst.put_u8(22);
                dst.put_slice(pieces_root);
                dst.put_u32(*base_layer);
                dst.put_u32(*index);
                dst.put_u32(*length);
                dst.put_u32(*proof_layers);
                for hash in hashes {
                    dst.put_slice(hash);
                }
            }
            Message::HashReject {
                pieces_root,
                base_layer,
                index,
                length,
                proof_layers,
            } => put_hash_exchange(
                dst,
                23,
                &HashExchangeHeader {
                    pieces_root,
                    base_layer: *base_layer,
                    index: *index,
                    length: *length,
                    proof_layers: *proof_layers,
                },
                &[],
            ),
        }
        Ok(())
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
                let data_len = body.len() - 8;
                if data_len as u32 > MAX_BLOCK_SIZE {
                    return Err(WireError::InvalidMessage(format!(
                        "Piece block {data_len} bytes exceeds MAX_BLOCK_SIZE {MAX_BLOCK_SIZE}"
                    )));
                }
                let data = Bytes::copy_from_slice(&body[8..]);
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
            16 => {
                expect_len(body, 12, "Reject")?;
                let (piece, begin, length) = parse_3u32(body);
                validate_request(length)?;
                Ok(Message::Reject {
                    piece,
                    begin,
                    length,
                })
            }
            14 => {
                expect_len(body, 0, "HaveAll")?;
                Ok(Message::HaveAll)
            }
            15 => {
                expect_len(body, 0, "HaveNone")?;
                Ok(Message::HaveNone)
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
            21 => {
                let (pieces_root, base_layer, index, length, proof_layers, hashes) =
                    parse_hash_exchange(body, false)?;
                debug_assert!(hashes.is_empty());
                Ok(Message::HashRequest {
                    pieces_root,
                    base_layer,
                    index,
                    length,
                    proof_layers,
                })
            }
            22 => {
                let (pieces_root, base_layer, index, length, proof_layers, hashes) =
                    parse_hash_exchange(body, true)?;
                Ok(Message::Hashes {
                    pieces_root,
                    base_layer,
                    index,
                    length,
                    proof_layers,
                    hashes,
                })
            }
            23 => {
                let (pieces_root, base_layer, index, length, proof_layers, hashes) =
                    parse_hash_exchange(body, false)?;
                debug_assert!(hashes.is_empty());
                Ok(Message::HashReject {
                    pieces_root,
                    base_layer,
                    index,
                    length,
                    proof_layers,
                })
            }
            _ => Err(WireError::UnknownMessageId(id)),
        }
    }
}

fn encode_hash_exchange(
    id: u8,
    pieces_root: &[u8; 32],
    base_layer: u32,
    index: u32,
    length: u32,
    proof_layers: u32,
    hashes: &[u8],
) -> Vec<u8> {
    let message_len = 1usize
        .saturating_add(HASH_EXCHANGE_HEADER_LEN)
        .saturating_add(hashes.len());
    let mut out = Vec::with_capacity(4usize.saturating_add(message_len));
    out.extend_from_slice(&(message_len as u32).to_be_bytes());
    out.push(id);
    out.extend_from_slice(pieces_root);
    out.extend_from_slice(&base_layer.to_be_bytes());
    out.extend_from_slice(&index.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&proof_layers.to_be_bytes());
    out.extend_from_slice(hashes);
    out
}

type HashExchange = ([u8; 32], u32, u32, u32, u32, Vec<[u8; 32]>);

fn parse_hash_exchange(body: &[u8], with_hashes: bool) -> Result<HashExchange, WireError> {
    if body.len() < HASH_EXCHANGE_HEADER_LEN {
        return Err(WireError::InvalidMessage(format!(
            "BEP 52 hash message expected at least {HASH_EXCHANGE_HEADER_LEN} body bytes, got {}",
            body.len()
        )));
    }
    if !with_hashes && body.len() != HASH_EXCHANGE_HEADER_LEN {
        return Err(WireError::InvalidMessage(format!(
            "BEP 52 hash request/reject expected {HASH_EXCHANGE_HEADER_LEN} body bytes, got {}",
            body.len()
        )));
    }
    let hash_bytes = &body[HASH_EXCHANGE_HEADER_LEN..];
    if !hash_bytes.len().is_multiple_of(32) {
        return Err(WireError::InvalidMessage(
            "BEP 52 hashes payload is not a multiple of 32 bytes".into(),
        ));
    }
    let mut pieces_root = [0u8; 32];
    pieces_root.copy_from_slice(&body[..32]);
    let base_layer = u32::from_be_bytes(body[32..36].try_into().unwrap());
    let index = u32::from_be_bytes(body[36..40].try_into().unwrap());
    let length = u32::from_be_bytes(body[40..44].try_into().unwrap());
    let proof_layers = u32::from_be_bytes(body[44..48].try_into().unwrap());
    let (hash_chunks, remainder) = hash_bytes.as_chunks::<32>();
    debug_assert!(remainder.is_empty());
    let hashes = hash_chunks.to_vec();
    Ok((pieces_root, base_layer, index, length, proof_layers, hashes))
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

fn put_fixed(dst: &mut BytesMut, id: u8, body: &[u8]) {
    dst.put_u32((1 + body.len()) as u32);
    dst.put_u8(id);
    dst.put_slice(body);
}

struct HashExchangeHeader<'a> {
    pieces_root: &'a [u8; 32],
    base_layer: u32,
    index: u32,
    length: u32,
    proof_layers: u32,
}

fn put_hash_exchange(dst: &mut BytesMut, id: u8, header: &HashExchangeHeader<'_>, hashes: &[u8]) {
    let message_len = 1 + HASH_EXCHANGE_HEADER_LEN + hashes.len();
    dst.put_u32(message_len as u32);
    dst.put_u8(id);
    dst.put_slice(header.pieces_root);
    dst.put_u32(header.base_layer);
    dst.put_u32(header.index);
    dst.put_u32(header.length);
    dst.put_u32(header.proof_layers);
    dst.put_slice(hashes);
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
    fn encode_into_matches_owned_encoding() {
        let messages = [
            Message::KeepAlive,
            Message::Have(42),
            Message::Bitfield(vec![0xFF, 0xA0, 0x00]),
            Message::Piece {
                piece: 3,
                begin: 16_384,
                data: Bytes::from(vec![0xAB; 1024]),
            },
            Message::Extended {
                ext_id: 3,
                payload: b"d1:md11:ut_metadatai1eee".to_vec(),
            },
            Message::Hashes {
                pieces_root: [0x22; 32],
                base_layer: 1,
                index: 0,
                length: 2,
                proof_layers: 0,
                hashes: vec![[0x33; 32], [0x44; 32]],
            },
        ];

        for message in messages {
            let expected = message.encode();
            let mut actual = BytesMut::new();
            message.encode_into(&mut actual).unwrap();
            assert_eq!(actual.as_ref(), expected.as_slice());
        }
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
        let data = Bytes::from(vec![0xABu8; 1024]);
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
    fn piece_data_is_bytes_and_clones_cheaply() {
        // The whole point of carrying `Bytes` instead of `Vec<u8>` on the
        // wire type is that a clone is a refcount bump over the same
        // backing allocation, not a memory copy.
        let data = Bytes::from(vec![0x11u8; 4096]);
        let msg = Message::Piece {
            piece: 0,
            begin: 0,
            data: data.clone(),
        };
        let Message::Piece { data: msg_data, .. } = msg else {
            unreachable!()
        };
        assert_eq!(data.as_ptr(), msg_data.as_ptr());
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
    fn reject_roundtrip() {
        let msg = Message::Reject {
            piece: 2,
            begin: 16384,
            length: 8192,
        };
        assert_eq!(roundtrip(msg.clone()), msg);
        assert_eq!(&msg.encode()[4..5], &[16]);
    }

    #[test]
    fn reject_rejects_invalid_length() {
        let mut payload = vec![16u8];
        payload.extend_from_slice(&encode_3u32(0, 0, MAX_BLOCK_SIZE + 1));
        assert!(Message::parse(&payload).is_err());
    }

    #[test]
    fn have_all_roundtrip() {
        assert_eq!(roundtrip(Message::HaveAll), Message::HaveAll);
        let encoded = Message::HaveAll.encode();
        // length prefix = 1 (id byte only, zero payload)
        let len = u32::from_be_bytes(encoded[..4].try_into().unwrap());
        assert_eq!(len, 1);
        assert_eq!(encoded[4], 14);
        assert_eq!(encoded.len(), 5);
    }

    #[test]
    fn have_none_roundtrip() {
        assert_eq!(roundtrip(Message::HaveNone), Message::HaveNone);
        let encoded = Message::HaveNone.encode();
        let len = u32::from_be_bytes(encoded[..4].try_into().unwrap());
        assert_eq!(len, 1);
        assert_eq!(encoded[4], 15);
        assert_eq!(encoded.len(), 5);
    }

    #[test]
    fn have_all_and_have_none_reject_nonempty_body() {
        assert!(Message::parse(&[14u8, 0u8]).is_err());
        assert!(Message::parse(&[15u8, 0u8]).is_err());
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
    fn bep52_hash_request_roundtrip() {
        let message = Message::HashRequest {
            pieces_root: [0x11; 32],
            base_layer: 4,
            index: 8,
            length: 4,
            proof_layers: 2,
        };
        assert_eq!(
            message.encoded_len(),
            Some(4 + 1 + HASH_EXCHANGE_HEADER_LEN)
        );
        assert_eq!(roundtrip(message.clone()), message);
        assert_eq!(&message.encode()[4..5], &[21]);
    }

    #[test]
    fn bep52_hashes_roundtrip() {
        let message = Message::Hashes {
            pieces_root: [0x22; 32],
            base_layer: 1,
            index: 0,
            length: 2,
            proof_layers: 0,
            hashes: vec![[0x33; 32], [0x44; 32], [0x55; 32]],
        };
        assert_eq!(roundtrip(message.clone()), message);
        assert_eq!(&message.encode()[4..5], &[22]);
    }

    #[test]
    fn bep52_hash_reject_roundtrip() {
        let message = Message::HashReject {
            pieces_root: [0x66; 32],
            base_layer: 0,
            index: 16,
            length: 16,
            proof_layers: 3,
        };
        assert_eq!(roundtrip(message.clone()), message);
        assert_eq!(&message.encode()[4..5], &[23]);
    }

    #[test]
    fn bep52_hash_coordinates_are_deferred_to_torrent_validation() {
        for (index, length) in [(0u32, 0u32), (0, 1), (0, 3), (1, 2), (4, 8)] {
            let mut payload = vec![21u8];
            payload.extend_from_slice(&[0; 32]);
            payload.extend_from_slice(&0u32.to_be_bytes());
            payload.extend_from_slice(&index.to_be_bytes());
            payload.extend_from_slice(&length.to_be_bytes());
            payload.extend_from_slice(&0u32.to_be_bytes());
            assert!(matches!(
                Message::parse(&payload),
                Ok(Message::HashRequest { index: parsed_index, length: parsed_length, .. })
                    if parsed_index == index && parsed_length == length
            ));
        }
    }

    #[test]
    fn bep52_hash_response_rejects_partial_hash() {
        let mut payload = vec![22u8];
        payload.extend_from_slice(&[0; 32]);
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&2u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.push(1);
        assert!(Message::parse(&payload).is_err());
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
