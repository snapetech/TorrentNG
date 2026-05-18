/// BEP 29 uTP packet codec.
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// A BEP 29 extension entry.
///
/// The fixed header's `extension` byte names the first extension kind. Each
/// extension entry then stores the next extension kind, a one-byte payload
/// length, and that extension's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtpExtension {
    pub kind: u8,
    pub data: Vec<u8>,
}

/// Decoded uTP packet including extension chain and application payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtpPacket {
    pub header: UtpHeader,
    pub extensions: Vec<UtpExtension>,
    pub payload: Vec<u8>,
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

impl UtpPacket {
    pub fn parse(buf: &[u8]) -> Result<Self, UtpError> {
        let header = UtpHeader::parse(buf)?;
        let mut offset = HEADER_SIZE;
        let mut extension = header.extension;
        let mut extensions = Vec::new();

        while extension != 0 {
            let remaining = buf.len().saturating_sub(offset);
            if remaining < 2 {
                return Err(UtpError::ExtensionHeaderTruncated {
                    extension,
                    remaining,
                });
            }
            let next_extension = buf[offset];
            let length = usize::from(buf[offset + 1]);
            offset += 2;
            let remaining = buf.len().saturating_sub(offset);
            if remaining < length {
                return Err(UtpError::ExtensionTruncated {
                    extension,
                    length,
                    remaining,
                });
            }
            extensions.push(UtpExtension {
                kind: extension,
                data: buf[offset..offset + length].to_vec(),
            });
            offset += length;
            extension = next_extension;
        }

        Ok(Self {
            header,
            extensions,
            payload: buf[offset..].to_vec(),
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, UtpError> {
        let mut header = self.header.clone();
        header.extension = self.extensions.first().map(|ext| ext.kind).unwrap_or(0);
        for ext in &self.extensions {
            if ext.data.len() > u8::MAX as usize {
                return Err(UtpError::ExtensionTooLong {
                    extension: ext.kind,
                    length: ext.data.len(),
                });
            }
        }

        let extension_len = self
            .extensions
            .iter()
            .map(|ext| 2 + ext.data.len())
            .sum::<usize>();
        let mut out = Vec::with_capacity(HEADER_SIZE + extension_len + self.payload.len());
        out.extend_from_slice(&header.encode());

        for (idx, ext) in self.extensions.iter().enumerate() {
            let next = self
                .extensions
                .get(idx + 1)
                .map(|ext| ext.kind)
                .unwrap_or(0);
            out.push(next);
            out.push(ext.data.len() as u8);
            out.extend_from_slice(&ext.data);
        }
        out.extend_from_slice(&self.payload);
        Ok(out)
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

    #[test]
    fn packet_without_extensions_roundtrips_payload() {
        let packet = UtpPacket {
            header: syn_header(),
            extensions: Vec::new(),
            payload: b"hello".to_vec(),
        };

        let decoded = UtpPacket::parse(&packet.encode().unwrap()).unwrap();
        assert_eq!(decoded.header, packet.header);
        assert!(decoded.extensions.is_empty());
        assert_eq!(decoded.payload, b"hello");
    }

    #[test]
    fn packet_extension_chain_roundtrips() {
        let packet = UtpPacket {
            header: syn_header(),
            extensions: vec![
                UtpExtension {
                    kind: 1,
                    data: vec![10, 11, 12],
                },
                UtpExtension {
                    kind: 2,
                    data: vec![20, 21],
                },
            ],
            payload: b"payload".to_vec(),
        };

        let encoded = packet.encode().unwrap();
        let decoded = UtpPacket::parse(&encoded).unwrap();
        assert_eq!(decoded.header.extension, 1);
        assert_eq!(decoded.extensions, packet.extensions);
        assert_eq!(decoded.payload, b"payload");
    }

    #[test]
    fn truncated_extension_header_errors() {
        let mut bytes = syn_header().encode().to_vec();
        bytes[1] = 1;
        bytes.push(0);

        assert!(matches!(
            UtpPacket::parse(&bytes),
            Err(UtpError::ExtensionHeaderTruncated {
                extension: 1,
                remaining: 1
            })
        ));
    }

    #[test]
    fn truncated_extension_payload_errors() {
        let mut bytes = syn_header().encode().to_vec();
        bytes[1] = 3;
        bytes.extend_from_slice(&[0, 4, 1, 2]);

        assert!(matches!(
            UtpPacket::parse(&bytes),
            Err(UtpError::ExtensionTruncated {
                extension: 3,
                length: 4,
                remaining: 2
            })
        ));
    }

    #[test]
    fn oversized_extension_encode_errors() {
        let packet = UtpPacket {
            header: syn_header(),
            extensions: vec![UtpExtension {
                kind: 1,
                data: vec![0; 256],
            }],
            payload: Vec::new(),
        };

        assert!(matches!(
            packet.encode(),
            Err(UtpError::ExtensionTooLong {
                extension: 1,
                length: 256
            })
        ));
    }
}
