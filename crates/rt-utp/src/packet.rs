/// BEP 29 uTP packet header codec.
///
/// uTP header: 20 bytes fixed.
/// type_ver(1) | extension(1) | connection_id(2) | timestamp_us(4) |
/// timestamp_diff(4) | wnd_size(4) | seq_nr(2) | ack_nr(2)
use crate::error::UtpError;

pub const HEADER_SIZE: usize = 20;

/// uTP packet types (upper 4 bits of type_ver byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Data = 0,
    Fin = 1,
    State = 2,
    Reset = 3,
    Syn = 4,
}

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(PacketType::Data),
            1 => Some(PacketType::Fin),
            2 => Some(PacketType::State),
            3 => Some(PacketType::Reset),
            4 => Some(PacketType::Syn),
            _ => None,
        }
    }
}

/// Decoded uTP packet header.
#[derive(Debug, Clone)]
pub struct UtpHeader {
    pub packet_type: PacketType,
    pub version: u8,
    pub extension: u8,
    pub connection_id: u16,
    pub timestamp_us: u32,
    pub timestamp_diff: u32,
    pub wnd_size: u32,
    pub seq_nr: u16,
    pub ack_nr: u16,
}

impl UtpHeader {
    pub fn parse(buf: &[u8]) -> Result<Self, UtpError> {
        if buf.len() < HEADER_SIZE {
            return Err(UtpError::HeaderTooShort(buf.len()));
        }
        let type_ver = buf[0];
        let version = type_ver & 0x0F;
        if version != 1 {
            return Err(UtpError::UnsupportedVersion(version));
        }
        let ptype_nibble = type_ver >> 4;
        let packet_type =
            PacketType::from_u8(ptype_nibble).ok_or(UtpError::UnknownPacketType(ptype_nibble))?;
        Ok(UtpHeader {
            packet_type,
            version,
            extension: buf[1],
            connection_id: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp_us: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            timestamp_diff: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
            wnd_size: u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]),
            seq_nr: u16::from_be_bytes([buf[16], buf[17]]),
            ack_nr: u16::from_be_bytes([buf[18], buf[19]]),
        })
    }

    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0] = (self.packet_type as u8) << 4 | (self.version & 0x0F);
        buf[1] = self.extension;
        buf[2..4].copy_from_slice(&self.connection_id.to_be_bytes());
        buf[4..8].copy_from_slice(&self.timestamp_us.to_be_bytes());
        buf[8..12].copy_from_slice(&self.timestamp_diff.to_be_bytes());
        buf[12..16].copy_from_slice(&self.wnd_size.to_be_bytes());
        buf[16..18].copy_from_slice(&self.seq_nr.to_be_bytes());
        buf[18..20].copy_from_slice(&self.ack_nr.to_be_bytes());
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syn_header() -> UtpHeader {
        UtpHeader {
            packet_type: PacketType::Syn,
            version: 1,
            extension: 0,
            connection_id: 0x1234,
            timestamp_us: 1_000_000,
            timestamp_diff: 0,
            wnd_size: 65536,
            seq_nr: 1,
            ack_nr: 0,
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let h = syn_header();
        let encoded = h.encode();
        assert_eq!(encoded.len(), HEADER_SIZE);
        let decoded = UtpHeader::parse(&encoded).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Syn);
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.connection_id, 0x1234);
        assert_eq!(decoded.timestamp_us, 1_000_000);
        assert_eq!(decoded.wnd_size, 65536);
        assert_eq!(decoded.seq_nr, 1);
        assert_eq!(decoded.ack_nr, 0);
    }

    #[test]
    fn too_short_errors() {
        assert!(UtpHeader::parse(&[0u8; 10]).is_err());
    }

    #[test]
    fn wrong_version_errors() {
        let mut buf = syn_header().encode();
        buf[0] = 4 << 4; // version=0, lower nibble=0
        assert!(matches!(
            UtpHeader::parse(&buf),
            Err(UtpError::UnsupportedVersion(0))
        ));
    }

    #[test]
    fn unknown_type_errors() {
        let mut buf = syn_header().encode();
        buf[0] = (7 << 4) | 1; // type=7 unknown
        assert!(matches!(
            UtpHeader::parse(&buf),
            Err(UtpError::UnknownPacketType(7))
        ));
    }

    #[test]
    fn all_packet_types_roundtrip() {
        for pt in [
            PacketType::Data,
            PacketType::Fin,
            PacketType::State,
            PacketType::Reset,
            PacketType::Syn,
        ] {
            let mut h = syn_header();
            h.packet_type = pt;
            let decoded = UtpHeader::parse(&h.encode()).unwrap();
            assert_eq!(decoded.packet_type, pt);
        }
    }
}
