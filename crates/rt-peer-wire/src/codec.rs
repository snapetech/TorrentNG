/// tokio-util `Codec` impl for BEP 3 length-prefixed peer messages.
use bytes::{Buf, BufMut, BytesMut};
use rt_metrics::{MemoryClass, MemoryLease, ResourceGovernor};
use tokio_util::codec::{Decoder, Encoder};

use crate::{
    error::WireError,
    message::{Message, MAX_MESSAGE_LEN},
};

/// BEP 3 length-prefixed peer message codec.
///
/// The default codec is useful for protocol-only callers. Engine-owned TCP
/// connections should use [`PeerCodec::with_resources`] so a peer cannot grow
/// tokio-util's retained read buffer to the protocol maximum without passing
/// the engine's `PeerBuffer` admission gate first.
#[derive(Default)]
pub struct PeerCodec {
    resources: Option<ResourceGovernor>,
    memory_leases: Vec<MemoryLease>,
    read_reserved_bytes: u64,
    write_reserved_bytes: u64,
}

impl PeerCodec {
    pub fn with_resources(resources: ResourceGovernor) -> Self {
        Self {
            resources: Some(resources),
            ..Self::default()
        }
    }

    fn reserve_capacity(&mut self, src: &mut BytesMut, required: usize) -> Result<(), WireError> {
        // `BytesMut::reserve` may grow from the current allocation by more
        // than the exact requested length. Charge twice the larger of the
        // current capacity and requested frame size: once for the retained
        // wire buffer and once for Message::parse's owned Bitfield/Extended
        // payload copy.
        let accounted_capacity = src.capacity().max(required);
        if let Some(resources) = self.resources.clone() {
            let required_bytes = u64::try_from(accounted_capacity)
                .ok()
                .and_then(|bytes| bytes.checked_mul(2))
                .ok_or(WireError::PeerBufferLengthOverflow)?;
            if required_bytes > self.read_reserved_bytes {
                let additional = required_bytes - self.read_reserved_bytes;
                let lease = resources
                    .try_acquire(MemoryClass::PeerBuffer, additional)
                    .ok_or(WireError::PeerBufferAllocationDenied(required_bytes))?;
                self.memory_leases.push(lease);
                self.read_reserved_bytes = required_bytes;
            }
        }

        if required > src.capacity() {
            src.reserve(required.saturating_sub(src.len()));
        }
        Ok(())
    }

    fn reserve_write_capacity(
        &mut self,
        dst: &mut BytesMut,
        required: usize,
    ) -> Result<(), WireError> {
        // The write buffer is retained by `Framed` while a slow peer drains
        // it. Charge a conservative growth allowance before BytesMut can
        // allocate; the read and write buffers share one codec but have
        // independent retained-capacity leases.
        let accounted_capacity = dst.capacity().max(required);
        if let Some(resources) = self.resources.clone() {
            let required_bytes = u64::try_from(accounted_capacity)
                .ok()
                .and_then(|bytes| bytes.checked_mul(2))
                .ok_or(WireError::PeerBufferLengthOverflow)?;
            if required_bytes > self.write_reserved_bytes {
                let additional = required_bytes - self.write_reserved_bytes;
                let lease = resources
                    .try_acquire(MemoryClass::PeerBuffer, additional)
                    .ok_or(WireError::PeerBufferAllocationDenied(required_bytes))?;
                self.memory_leases.push(lease);
                self.write_reserved_bytes = required_bytes;
            }
        }
        if required > dst.capacity() {
            dst.reserve(required.saturating_sub(dst.len()));
        }
        Ok(())
    }

    fn reserve_encode_temporary(&self, bytes: usize) -> Result<Option<MemoryLease>, WireError> {
        let Some(resources) = self.resources.clone() else {
            return Ok(None);
        };
        let bytes = u64::try_from(bytes).map_err(|_| WireError::PeerBufferLengthOverflow)?;
        resources
            .try_acquire(MemoryClass::PeerBuffer, bytes)
            .map(Some)
            .ok_or(WireError::PeerBufferAllocationDenied(bytes))
    }
}

impl Decoder for PeerCodec {
    type Item = Message;
    type Error = WireError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            self.reserve_capacity(src, 4)?;
            return Ok(None);
        }
        let len = u32::from_be_bytes(src[..4].try_into().unwrap());
        if len > MAX_MESSAGE_LEN {
            return Err(WireError::MessageTooLarge(len));
        }
        let total = 4 + len as usize;
        if src.len() < total {
            self.reserve_capacity(src, total)?;
            return Ok(None);
        }
        src.advance(4);
        let payload = src.split_to(len as usize);
        let msg = Message::parse(&payload)?;
        Ok(Some(msg))
    }
}

impl Encoder<Message> for PeerCodec {
    type Error = WireError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded_len = item
            .encoded_len()
            .ok_or(WireError::MessageTooLarge(u32::MAX))?;
        // `Piece` carries the seeding hot-path block payload as `Bytes`.
        // Write the frame directly into `dst` instead of routing through
        // `Message::encode`'s `Vec<u8>` return value: that would force an
        // extra copy of the block (into the temporary `Vec`) on top of the
        // copy already required to land the bytes in the socket write
        // buffer. Building the frame here collapses that down to the one
        // copy that is unavoidable for a length-prefixed contiguous frame,
        // moving `data` straight in rather than cloning it.
        match item {
            Message::Piece { piece, begin, data } => {
                self.reserve_write_capacity(dst, encoded_len)?;
                let len = (encoded_len - 4) as u32;
                dst.put_u32(len);
                dst.put_u8(7);
                dst.put_u32(piece);
                dst.put_u32(begin);
                dst.put(data);
            }
            other => {
                // `Message::encode` returns a temporary Vec while the
                // framed write buffer retains its own copy. Reserve the
                // temporary before constructing it, then release that
                // short-lived lease after the copy is queued.
                let _temporary = self.reserve_encode_temporary(encoded_len)?;
                self.reserve_write_capacity(dst, encoded_len)?;
                let encoded = other.encode();
                dst.put_slice(&encoded);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    fn encode_msg(msg: Message) -> BytesMut {
        let mut buf = BytesMut::new();
        PeerCodec::default().encode(msg, &mut buf).unwrap();
        buf
    }

    #[test]
    fn codec_roundtrip_choke() {
        let mut buf = encode_msg(Message::Choke);
        let msg = PeerCodec::default().decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg, Message::Choke);
        assert!(buf.is_empty());
    }

    #[test]
    fn codec_roundtrip_have() {
        let mut buf = encode_msg(Message::Have(99));
        let msg = PeerCodec::default().decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg, Message::Have(99));
    }

    #[test]
    fn codec_partial_read_returns_none() {
        let mut buf = encode_msg(Message::Have(1));
        // Only feed 3 bytes — not enough for length prefix
        let partial = buf.split_to(3);
        let mut partial_buf = partial;
        let result = PeerCodec::default().decode(&mut partial_buf).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn codec_rejects_oversized_message() {
        let mut buf = BytesMut::new();
        buf.put_u32(MAX_MESSAGE_LEN + 1);
        buf.put_bytes(0, (MAX_MESSAGE_LEN + 1) as usize);
        assert!(PeerCodec::default().decode(&mut buf).is_err());
    }

    #[test]
    fn codec_multiple_messages_in_buffer() {
        let mut buf = BytesMut::new();
        let mut codec = PeerCodec::default();
        codec.encode(Message::Choke, &mut buf).unwrap();
        codec.encode(Message::Interested, &mut buf).unwrap();
        let m1 = codec.decode(&mut buf).unwrap().unwrap();
        let m2 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(m1, Message::Choke);
        assert_eq!(m2, Message::Interested);
    }

    #[test]
    fn codec_roundtrip_piece_bytes() {
        let data = bytes::Bytes::from(vec![0x42u8; 4096]);
        let msg = Message::Piece {
            piece: 7,
            begin: 16384,
            data: data.clone(),
        };
        let mut buf = encode_msg(msg);
        let decoded = PeerCodec::default().decode(&mut buf).unwrap().unwrap();
        assert_eq!(
            decoded,
            Message::Piece {
                piece: 7,
                begin: 16384,
                data,
            }
        );
    }

    #[test]
    fn codec_piece_frame_matches_generic_encode() {
        // The Encoder specializes `Piece` to build the frame directly in
        // `dst` instead of going through `Message::encode`'s `Vec<u8>`
        // path. Confirm the two produce byte-identical frames.
        let data = bytes::Bytes::from(vec![0xAAu8; 1024]);
        let msg = Message::Piece {
            piece: 1,
            begin: 0,
            data: data.clone(),
        };
        let via_codec = encode_msg(msg.clone());
        let via_generic = msg.encode();
        assert_eq!(via_codec.as_ref(), via_generic.as_slice());
    }

    #[test]
    fn codec_roundtrip_have_all_and_have_none() {
        let mut buf = encode_msg(Message::HaveAll);
        assert_eq!(
            PeerCodec::default().decode(&mut buf).unwrap().unwrap(),
            Message::HaveAll
        );
        let mut buf = encode_msg(Message::HaveNone);
        assert_eq!(
            PeerCodec::default().decode(&mut buf).unwrap().unwrap(),
            Message::HaveNone
        );
    }

    #[test]
    fn resource_codec_denies_declared_frame_before_growth() {
        let mut class_caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        class_caps[MemoryClass::PeerBuffer as usize] = 64;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 64,
            class_caps_bytes: class_caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let mut codec = PeerCodec::with_resources(governor.clone());
        let mut buf = BytesMut::with_capacity(4);
        buf.put_u32(MAX_MESSAGE_LEN);

        let error = codec
            .decode(&mut buf)
            .expect_err("frame growth must be denied");
        assert!(matches!(error, WireError::PeerBufferAllocationDenied(_)));
        assert_eq!(buf.capacity(), 4);
        drop(codec);
        assert_eq!(governor.snapshot().total_used_bytes, 0);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].denied_allocations,
            1
        );
    }

    #[test]
    fn resource_codec_denies_outbound_frame_before_growth() {
        let mut class_caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        class_caps[MemoryClass::PeerBuffer as usize] = 64;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 64,
            class_caps_bytes: class_caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let mut codec = PeerCodec::with_resources(governor.clone());
        let mut buf = BytesMut::with_capacity(4);
        let error = codec
            .encode(
                Message::Extended {
                    ext_id: 1,
                    payload: vec![0; 40],
                },
                &mut buf,
            )
            .expect_err("outbound frame growth must be denied");
        assert!(matches!(error, WireError::PeerBufferAllocationDenied(_)));
        assert_eq!(buf.capacity(), 4);
        drop(codec);
        assert_eq!(governor.snapshot().total_used_bytes, 0);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].denied_allocations,
            1
        );
    }
}
