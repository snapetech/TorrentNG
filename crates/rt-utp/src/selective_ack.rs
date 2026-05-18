/// BEP 29 selective ACK extension helpers.
///
/// The extension payload is a bitset for packets after `ack_nr`: bit 0 of the
/// first byte acknowledges `ack_nr + 2`, bit 1 acknowledges `ack_nr + 3`, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectiveAck {
    bits: Vec<u8>,
}

impl SelectiveAck {
    pub const EXTENSION_KIND: u8 = 1;

    pub fn new(bits: Vec<u8>) -> Self {
        Self { bits }
    }

    pub fn empty() -> Self {
        Self { bits: Vec::new() }
    }

    pub fn from_received_offsets(mut offsets: Vec<u16>) -> Self {
        offsets.retain(|offset| *offset >= 2);
        let Some(max) = offsets.iter().copied().max() else {
            return Self::empty();
        };
        let mut bits = vec![0u8; usize::from(max / 8) + 1];
        for offset in offsets {
            let bit_index = offset - 2;
            bits[usize::from(bit_index / 8)] |= 1 << (bit_index % 8);
        }
        while bits.last().is_some_and(|last| *last == 0) {
            bits.pop();
        }
        Self { bits }
    }

    pub fn is_acknowledged(&self, offset_after_ack: u16) -> bool {
        if offset_after_ack < 2 {
            return false;
        }
        let bit_index = offset_after_ack - 2;
        let byte = usize::from(bit_index / 8);
        let bit = bit_index % 8;
        self.bits
            .get(byte)
            .is_some_and(|value| (value & (1 << bit)) != 0)
    }

    pub fn acknowledged_offsets(&self) -> Vec<u16> {
        let mut out = Vec::new();
        for (byte_idx, byte) in self.bits.iter().copied().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    out.push((byte_idx as u16 * 8) + bit + 2);
                }
            }
        }
        out
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selective_ack_offsets_roundtrip_to_bits() {
        let ack = SelectiveAck::from_received_offsets(vec![0, 1, 2, 3, 9, 17]);

        assert!(ack.is_acknowledged(2));
        assert!(ack.is_acknowledged(3));
        assert!(ack.is_acknowledged(9));
        assert!(ack.is_acknowledged(17));
        assert!(!ack.is_acknowledged(1));
        assert!(!ack.is_acknowledged(10));
        assert_eq!(ack.acknowledged_offsets(), vec![2, 3, 9, 17]);
    }

    #[test]
    fn empty_selective_ack_has_no_offsets() {
        let ack = SelectiveAck::from_received_offsets(vec![0, 1]);
        assert!(ack.as_bytes().is_empty());
        assert!(ack.acknowledged_offsets().is_empty());
    }
}
