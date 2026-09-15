/// BEP 3 peer handshake: pstrlen(1) + pstr(19) + reserved(8) + info_hash(20) + peer_id(20) = 68 bytes.
use crate::error::WireError;

pub const HANDSHAKE_LEN: usize = 68;
const PROTOCOL: &[u8] = b"BitTorrent protocol";
const PROTOCOL_LEN: u8 = 19;

/// Extension flags carried in the 8 reserved bytes.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtensionFlags(pub [u8; 8]);

impl ExtensionFlags {
    /// BEP 10 extension protocol (byte 5, bit 4 from right = 0x10).
    pub fn supports_extension_protocol(self) -> bool {
        self.0[5] & 0x10 != 0
    }

    /// BEP 6 Fast extension (byte 7, bit 0x04).
    pub fn supports_fast_extension(self) -> bool {
        self.0[7] & 0x04 != 0
    }

    /// BEP 52 v2 peer support. The v2 bit is the fourth-most-significant bit
    /// of the final reserved byte (`0x10`).
    pub fn supports_v2(self) -> bool {
        self.0[7] & 0x10 != 0
    }

    pub fn with_extension_protocol() -> Self {
        let mut flags = [0u8; 8];
        flags[5] |= 0x10;
        ExtensionFlags(flags)
    }

    /// BEP 10 extension protocol plus BEP 6 Fast extension. TorrentNG
    /// advertises both in every handshake since it implements both (Fast
    /// extension support is currently scoped to `HaveAll`/`HaveNone`).
    pub fn with_extension_protocol_and_fast() -> Self {
        let mut flags = Self::with_extension_protocol();
        flags.0[7] |= 0x04;
        flags
    }

    /// Advertise BEP 10, BEP 6 Fast, and BEP 52 v2 support.
    pub fn with_extension_protocol_and_fast_and_v2() -> Self {
        let mut flags = Self::with_extension_protocol_and_fast();
        flags.0[7] |= 0x10;
        flags
    }

    /// Add BEP 52 support to the standard extension/fast capability set.
    pub fn with_v2_support() -> Self {
        Self::with_extension_protocol_and_fast_and_v2()
    }
}

/// Outgoing handshake.
#[derive(Debug, Clone)]
pub struct Handshake {
    pub reserved: ExtensionFlags,
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Handshake {
            reserved: ExtensionFlags::default(),
            info_hash,
            peer_id,
        }
    }

    pub fn encode(&self) -> [u8; HANDSHAKE_LEN] {
        let mut buf = [0u8; HANDSHAKE_LEN];
        buf[0] = PROTOCOL_LEN;
        buf[1..20].copy_from_slice(PROTOCOL);
        buf[20..28].copy_from_slice(&self.reserved.0);
        buf[28..48].copy_from_slice(&self.info_hash);
        buf[48..68].copy_from_slice(&self.peer_id);
        buf
    }

    pub fn parse(buf: &[u8; HANDSHAKE_LEN]) -> Result<Self, WireError> {
        if buf[0] != PROTOCOL_LEN || &buf[1..20] != PROTOCOL {
            return Err(WireError::ProtocolMismatch);
        }
        let mut reserved = [0u8; 8];
        reserved.copy_from_slice(&buf[20..28]);
        let mut info_hash = [0u8; 20];
        info_hash.copy_from_slice(&buf[28..48]);
        let mut peer_id = [0u8; 20];
        peer_id.copy_from_slice(&buf[48..68]);
        Ok(Handshake {
            reserved: ExtensionFlags(reserved),
            info_hash,
            peer_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let ih = [0xABu8; 20];
        let pid = [0x2Du8; 20];
        let hs = Handshake::new(ih, pid);
        let encoded = hs.encode();
        assert_eq!(encoded.len(), HANDSHAKE_LEN);
        let parsed = Handshake::parse(&encoded).unwrap();
        assert_eq!(parsed.info_hash, ih);
        assert_eq!(parsed.peer_id, pid);
    }

    #[test]
    fn wrong_protocol_rejected() {
        let mut buf = [0u8; HANDSHAKE_LEN];
        buf[0] = 19;
        buf[1..20].copy_from_slice(b"WrongProtocol!!!!!!"); // 19 bytes
        assert!(Handshake::parse(&buf).is_err());
    }

    #[test]
    fn extension_flag_roundtrip() {
        let flags = ExtensionFlags::with_extension_protocol();
        assert!(flags.supports_extension_protocol());
        let zero = ExtensionFlags::default();
        assert!(!zero.supports_extension_protocol());
    }

    #[test]
    fn fast_extension_flag_roundtrip() {
        let flags = ExtensionFlags::with_extension_protocol_and_fast();
        assert!(flags.supports_extension_protocol());
        assert!(flags.supports_fast_extension());

        // Plain BEP 10 support (no Fast extension bit) must not be
        // misdetected as Fast-extension support.
        let ext_only = ExtensionFlags::with_extension_protocol();
        assert!(!ext_only.supports_fast_extension());

        let zero = ExtensionFlags::default();
        assert!(!zero.supports_fast_extension());
    }

    #[test]
    fn fast_extension_flag_survives_handshake_wire_roundtrip() {
        let hs = Handshake {
            reserved: ExtensionFlags::with_extension_protocol_and_fast(),
            info_hash: [0x11u8; 20],
            peer_id: [0x22u8; 20],
        };
        let encoded = hs.encode();
        let parsed = Handshake::parse(&encoded).unwrap();
        assert!(parsed.reserved.supports_extension_protocol());
        assert!(parsed.reserved.supports_fast_extension());
    }

    #[test]
    fn v2_extension_flag_roundtrip() {
        let flags = ExtensionFlags::with_v2_support();
        assert!(flags.supports_extension_protocol());
        assert!(flags.supports_fast_extension());
        assert!(flags.supports_v2());

        let ext_only = ExtensionFlags::with_extension_protocol();
        assert!(!ext_only.supports_v2());

        let hs = Handshake {
            reserved: flags,
            info_hash: [0x31; 20],
            peer_id: [0x32; 20],
        };
        let parsed = Handshake::parse(&hs.encode()).unwrap();
        assert!(parsed.reserved.supports_v2());
    }
}
