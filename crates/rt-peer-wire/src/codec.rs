/// tokio-util `Codec` impl for BEP 3 length-prefixed peer messages.
use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::{
    error::WireError,
    message::{Message, MAX_MESSAGE_LEN},
};

pub struct PeerCodec;

impl Decoder for PeerCodec {
    type Item = Message;
    type Error = WireError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            src.reserve(4);
            return Ok(None);
        }
        let len = u32::from_be_bytes(src[..4].try_into().unwrap());
        if len > MAX_MESSAGE_LEN {
            return Err(WireError::MessageTooLarge(len));
        }
        let total = 4 + len as usize;
        if src.len() < total {
            src.reserve(total - src.len());
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
        let encoded = item.encode();
        dst.put_slice(&encoded);
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
        PeerCodec.encode(msg, &mut buf).unwrap();
        buf
    }

    #[test]
    fn codec_roundtrip_choke() {
        let mut buf = encode_msg(Message::Choke);
        let msg = PeerCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg, Message::Choke);
        assert!(buf.is_empty());
    }

    #[test]
    fn codec_roundtrip_have() {
        let mut buf = encode_msg(Message::Have(99));
        let msg = PeerCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg, Message::Have(99));
    }

    #[test]
    fn codec_partial_read_returns_none() {
        let mut buf = encode_msg(Message::Have(1));
        // Only feed 3 bytes — not enough for length prefix
        let partial = buf.split_to(3);
        let mut partial_buf = partial;
        let result = PeerCodec.decode(&mut partial_buf).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn codec_rejects_oversized_message() {
        let mut buf = BytesMut::new();
        buf.put_u32(MAX_MESSAGE_LEN + 1);
        buf.put_bytes(0, (MAX_MESSAGE_LEN + 1) as usize);
        assert!(PeerCodec.decode(&mut buf).is_err());
    }

    #[test]
    fn codec_multiple_messages_in_buffer() {
        let mut buf = BytesMut::new();
        PeerCodec.encode(Message::Choke, &mut buf).unwrap();
        PeerCodec.encode(Message::Interested, &mut buf).unwrap();
        let m1 = PeerCodec.decode(&mut buf).unwrap().unwrap();
        let m2 = PeerCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(m1, Message::Choke);
        assert_eq!(m2, Message::Interested);
    }
}
