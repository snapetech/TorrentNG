use crate::decode::BValue;

pub fn encode(val: &BValue<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_into(val, &mut buf);
    buf
}

fn encode_into(val: &BValue<'_>, buf: &mut Vec<u8>) {
    match val {
        BValue::Int(n) => {
            buf.push(b'i');
            buf.extend_from_slice(n.to_string().as_bytes());
            buf.push(b'e');
        }
        BValue::Bytes(b) => {
            buf.extend_from_slice(b.len().to_string().as_bytes());
            buf.push(b':');
            buf.extend_from_slice(b);
        }
        BValue::List(items) => {
            buf.push(b'l');
            for item in items {
                encode_into(item, buf);
            }
            buf.push(b'e');
        }
        BValue::Dict(pairs) => {
            buf.push(b'd');
            for (k, v) in pairs {
                buf.extend_from_slice(k.len().to_string().as_bytes());
                buf.push(b':');
                buf.extend_from_slice(k);
                encode_into(v, buf);
            }
            buf.push(b'e');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_integer() {
        let v = BValue::Int(42);
        assert_eq!(encode(&v), b"i42e");
    }

    #[test]
    fn roundtrip_bytes() {
        let v = BValue::Bytes(b"spam");
        assert_eq!(encode(&v), b"4:spam");
    }

    #[test]
    fn roundtrip_list() {
        let v = BValue::List(vec![BValue::Int(1), BValue::Bytes(b"a")]);
        assert_eq!(encode(&v), b"li1e1:ae");
    }
}
