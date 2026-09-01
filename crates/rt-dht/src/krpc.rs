//! BEP 5 KRPC message codec.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use rt_bencode::{encode, BValue, Decoder};

use crate::{KNode, NodeId};

#[derive(Debug, thiserror::Error)]
pub enum KrpcError {
    #[error("bencode error: {0}")]
    Bencode(#[from] rt_bencode::BencodeError),
    #[error("invalid KRPC message: {0}")]
    Invalid(&'static str),
    #[error("unsupported KRPC query: {0}")]
    UnsupportedQuery(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DhtQuery {
    Ping {
        id: NodeId,
    },
    FindNode {
        id: NodeId,
        target: NodeId,
    },
    GetPeers {
        id: NodeId,
        info_hash: [u8; 20],
    },
    AnnouncePeer {
        id: NodeId,
        implied_port: bool,
        info_hash: [u8; 20],
        port: u16,
        token: Vec<u8>,
    },
}

impl DhtQuery {
    pub fn name(&self) -> &'static [u8] {
        match self {
            DhtQuery::Ping { .. } => b"ping",
            DhtQuery::FindNode { .. } => b"find_node",
            DhtQuery::GetPeers { .. } => b"get_peers",
            DhtQuery::AnnouncePeer { .. } => b"announce_peer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtResponse {
    pub id: NodeId,
    pub nodes: Vec<KNode>,
    pub values: Vec<SocketAddr>,
    pub token: Option<Vec<u8>>,
}

impl DhtResponse {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            nodes: Vec::new(),
            values: Vec::new(),
            token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KrpcMessage {
    Query {
        transaction_id: Vec<u8>,
        query: DhtQuery,
    },
    Response {
        transaction_id: Vec<u8>,
        response: DhtResponse,
    },
    Error {
        transaction_id: Vec<u8>,
        error: DhtError,
    },
}

impl KrpcMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            KrpcMessage::Query {
                transaction_id,
                query,
            } => encode_query(transaction_id, query),
            KrpcMessage::Response {
                transaction_id,
                response,
            } => encode_response(transaction_id, response),
            KrpcMessage::Error {
                transaction_id,
                error,
            } => encode_error(transaction_id, error),
        }
    }

    pub fn parse(input: &[u8]) -> Result<Self, KrpcError> {
        let value = Decoder::new(input).with_strict_dict_keys(false).decode()?;
        let t = required_bytes(&value, b"t")?.to_vec();
        let y = required_bytes(&value, b"y")?;
        match y {
            b"q" => parse_query(&value, t),
            b"r" => parse_response(&value, t),
            b"e" => parse_error(&value, t),
            _ => Err(KrpcError::Invalid("unknown message type")),
        }
    }
}

fn encode_query(transaction_id: &[u8], query: &DhtQuery) -> Vec<u8> {
    let id = query_id(query);
    let mut args = vec![(b"id".as_ref(), BValue::Bytes(id.as_bytes()))];
    let target;
    let info_hash;
    let port;
    let implied_port;

    match query {
        DhtQuery::Ping { .. } => {}
        DhtQuery::FindNode { target: id, .. } => {
            target = *id.as_bytes();
            args.push((b"target".as_ref(), BValue::Bytes(&target)));
        }
        DhtQuery::GetPeers {
            info_hash: hash, ..
        } => {
            info_hash = *hash;
            args.push((b"info_hash".as_ref(), BValue::Bytes(&info_hash)));
        }
        DhtQuery::AnnouncePeer {
            implied_port: implied,
            info_hash: hash,
            port: p,
            token: tok,
            ..
        } => {
            implied_port = if *implied { 1 } else { 0 };
            info_hash = *hash;
            port = i64::from(*p);
            args.push((b"implied_port".as_ref(), BValue::Int(implied_port)));
            args.push((b"info_hash".as_ref(), BValue::Bytes(&info_hash)));
            args.push((b"port".as_ref(), BValue::Int(port)));
            args.push((b"token".as_ref(), BValue::Bytes(tok)));
        }
    }

    encode(&BValue::Dict(vec![
        (b"a".as_ref(), BValue::Dict(args)),
        (b"q".as_ref(), BValue::Bytes(query.name())),
        (b"t".as_ref(), BValue::Bytes(transaction_id)),
        (b"y".as_ref(), BValue::Bytes(b"q")),
    ]))
}

fn encode_response(transaction_id: &[u8], response: &DhtResponse) -> Vec<u8> {
    let id = *response.id.as_bytes();
    let nodes = encode_compact_nodes(&response.nodes);
    let compact_values = encode_compact_peer_values(&response.values);
    let mut pairs = vec![(b"id".as_ref(), BValue::Bytes(&id))];
    if !nodes.is_empty() {
        pairs.push((b"nodes".as_ref(), BValue::Bytes(&nodes)));
    }
    if let Some(token) = &response.token {
        pairs.push((b"token".as_ref(), BValue::Bytes(token)));
    }
    if !compact_values.is_empty() {
        let values = compact_values
            .iter()
            .map(|peer| BValue::Bytes(peer.as_slice()))
            .collect();
        pairs.push((b"values".as_ref(), BValue::List(values)));
    }

    encode(&BValue::Dict(vec![
        (b"r".as_ref(), BValue::Dict(pairs)),
        (b"t".as_ref(), BValue::Bytes(transaction_id)),
        (b"y".as_ref(), BValue::Bytes(b"r")),
    ]))
}

fn encode_error(transaction_id: &[u8], error: &DhtError) -> Vec<u8> {
    encode(&BValue::Dict(vec![
        (
            b"e".as_ref(),
            BValue::List(vec![
                BValue::Int(error.code),
                BValue::Bytes(error.message.as_bytes()),
            ]),
        ),
        (b"t".as_ref(), BValue::Bytes(transaction_id)),
        (b"y".as_ref(), BValue::Bytes(b"e")),
    ]))
}

fn query_id(query: &DhtQuery) -> NodeId {
    match query {
        DhtQuery::Ping { id }
        | DhtQuery::FindNode { id, .. }
        | DhtQuery::GetPeers { id, .. }
        | DhtQuery::AnnouncePeer { id, .. } => *id,
    }
}

fn parse_query(value: &BValue<'_>, transaction_id: Vec<u8>) -> Result<KrpcMessage, KrpcError> {
    let q = std::str::from_utf8(required_bytes(value, b"q")?)
        .map_err(|_| KrpcError::Invalid("query name is not utf-8"))?;
    let args = value
        .get(b"a")
        .ok_or(KrpcError::Invalid("missing query args"))?;
    let id = parse_node_id(required_bytes(args, b"id")?)?;
    let query = match q {
        "ping" => DhtQuery::Ping { id },
        "find_node" => DhtQuery::FindNode {
            id,
            target: parse_node_id(required_bytes(args, b"target")?)?,
        },
        "get_peers" => DhtQuery::GetPeers {
            id,
            info_hash: parse_20(required_bytes(args, b"info_hash")?)?,
        },
        "announce_peer" => DhtQuery::AnnouncePeer {
            id,
            implied_port: optional_int(args, b"implied_port").unwrap_or(0) != 0,
            info_hash: parse_20(required_bytes(args, b"info_hash")?)?,
            port: parse_port(required_int(args, b"port")?)?,
            token: required_bytes(args, b"token")?.to_vec(),
        },
        other => return Err(KrpcError::UnsupportedQuery(other.to_owned())),
    };
    Ok(KrpcMessage::Query {
        transaction_id,
        query,
    })
}

fn parse_response(value: &BValue<'_>, transaction_id: Vec<u8>) -> Result<KrpcMessage, KrpcError> {
    let r = value
        .get(b"r")
        .ok_or(KrpcError::Invalid("missing response"))?;
    let id = parse_node_id(required_bytes(r, b"id")?)?;
    let nodes = match r.get(b"nodes").and_then(BValue::as_bytes) {
        Some(bytes) => parse_compact_nodes(bytes)?,
        None => Vec::new(),
    };
    let values = match r.get(b"values") {
        Some(BValue::List(items)) => {
            let mut peers = Vec::new();
            for item in items {
                peers.extend(parse_compact_peers(required_value_bytes(item)?)?);
            }
            peers
        }
        Some(_) => return Err(KrpcError::Invalid("values must be a list")),
        None => Vec::new(),
    };
    let token = r.get(b"token").and_then(BValue::as_bytes).map(Vec::from);

    Ok(KrpcMessage::Response {
        transaction_id,
        response: DhtResponse {
            id,
            nodes,
            values,
            token,
        },
    })
}

fn parse_error(value: &BValue<'_>, transaction_id: Vec<u8>) -> Result<KrpcMessage, KrpcError> {
    let Some(BValue::List(items)) = value.get(b"e") else {
        return Err(KrpcError::Invalid("missing error list"));
    };
    if items.len() != 2 {
        return Err(KrpcError::Invalid("error list must have code and message"));
    }
    let code = items[0]
        .as_int()
        .ok_or(KrpcError::Invalid("error code must be int"))?;
    let message = items[1]
        .as_str()
        .ok_or(KrpcError::Invalid("error message must be utf-8"))?
        .to_owned();
    Ok(KrpcMessage::Error {
        transaction_id,
        error: DhtError { code, message },
    })
}

pub fn encode_compact_nodes(nodes: &[KNode]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nodes.len() * 26);
    for node in nodes {
        out.extend_from_slice(node.id.as_bytes());
        out.extend_from_slice(&node.addr.ip().octets());
        out.extend_from_slice(&node.addr.port().to_be_bytes());
    }
    out
}

pub fn parse_compact_nodes(bytes: &[u8]) -> Result<Vec<KNode>, KrpcError> {
    if !bytes.len().is_multiple_of(26) {
        return Err(KrpcError::Invalid(
            "compact nodes length is not a multiple of 26",
        ));
    }
    Ok(bytes
        .as_chunks::<26>()
        .0
        .iter()
        .map(|chunk| {
            let mut id = [0u8; 20];
            id.copy_from_slice(&chunk[..20]);
            let ip = Ipv4Addr::new(chunk[20], chunk[21], chunk[22], chunk[23]);
            let port = u16::from_be_bytes([chunk[24], chunk[25]]);
            KNode {
                id: NodeId::from_bytes(id),
                addr: SocketAddrV4::new(ip, port),
            }
        })
        .collect())
}

pub fn parse_compact_peers(bytes: &[u8]) -> Result<Vec<SocketAddr>, KrpcError> {
    if bytes.len() == 18 {
        let mut octets = [0u8; 16];
        octets.copy_from_slice(&bytes[..16]);
        return Ok(vec![SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::from(octets),
            u16::from_be_bytes([bytes[16], bytes[17]]),
            0,
            0,
        ))]);
    }
    if bytes.len().is_multiple_of(6) {
        return Ok(bytes
            .as_chunks::<6>()
            .0
            .iter()
            .map(|chunk| {
                SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]),
                    u16::from_be_bytes([chunk[4], chunk[5]]),
                ))
            })
            .collect());
    }
    if bytes.len().is_multiple_of(18) {
        return Ok(bytes
            .as_chunks::<18>()
            .0
            .iter()
            .map(|chunk| {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&chunk[..16]);
                SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::from(octets),
                    u16::from_be_bytes([chunk[16], chunk[17]]),
                    0,
                    0,
                ))
            })
            .collect());
    }
    Err(KrpcError::Invalid(
        "compact peers length is not a multiple of 6 or 18",
    ))
}

fn encode_compact_peer_values(peers: &[SocketAddr]) -> Vec<Vec<u8>> {
    peers
        .iter()
        .map(|peer| {
            let mut compact = Vec::with_capacity(match peer.ip() {
                IpAddr::V4(_) => 6,
                IpAddr::V6(_) => 18,
            });
            match peer.ip() {
                IpAddr::V4(ip) => compact.extend_from_slice(&ip.octets()),
                IpAddr::V6(ip) => compact.extend_from_slice(&ip.octets()),
            }
            compact.extend_from_slice(&peer.port().to_be_bytes());
            compact
        })
        .collect()
}

fn required_bytes<'a>(value: &'a BValue<'a>, key: &[u8]) -> Result<&'a [u8], KrpcError> {
    value
        .get(key)
        .and_then(BValue::as_bytes)
        .ok_or(KrpcError::Invalid("missing or invalid bytes field"))
}

fn required_value_bytes<'a>(value: &'a BValue<'a>) -> Result<&'a [u8], KrpcError> {
    value
        .as_bytes()
        .ok_or(KrpcError::Invalid("expected bytes value"))
}

fn required_int(value: &BValue<'_>, key: &[u8]) -> Result<i64, KrpcError> {
    value
        .get(key)
        .and_then(BValue::as_int)
        .ok_or(KrpcError::Invalid("missing or invalid int field"))
}

fn optional_int(value: &BValue<'_>, key: &[u8]) -> Option<i64> {
    value.get(key).and_then(BValue::as_int)
}

fn parse_node_id(bytes: &[u8]) -> Result<NodeId, KrpcError> {
    Ok(NodeId::from_bytes(parse_20(bytes)?))
}

fn parse_20(bytes: &[u8]) -> Result<[u8; 20], KrpcError> {
    bytes
        .try_into()
        .map_err(|_| KrpcError::Invalid("expected 20 bytes"))
}

fn parse_port(port: i64) -> Result<u16, KrpcError> {
    u16::try_from(port).map_err(|_| KrpcError::Invalid("invalid port"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> NodeId {
        NodeId::from_bytes([byte; 20])
    }

    #[test]
    fn ping_query_roundtrip() {
        let msg = KrpcMessage::Query {
            transaction_id: b"aa".to_vec(),
            query: DhtQuery::Ping { id: id(1) },
        };
        assert_eq!(KrpcMessage::parse(&msg.encode()).unwrap(), msg);
    }

    #[test]
    fn find_node_query_roundtrip() {
        let msg = KrpcMessage::Query {
            transaction_id: b"fn".to_vec(),
            query: DhtQuery::FindNode {
                id: id(1),
                target: id(2),
            },
        };
        assert_eq!(KrpcMessage::parse(&msg.encode()).unwrap(), msg);
    }

    #[test]
    fn get_peers_query_roundtrip() {
        let msg = KrpcMessage::Query {
            transaction_id: b"gp".to_vec(),
            query: DhtQuery::GetPeers {
                id: id(1),
                info_hash: [9u8; 20],
            },
        };
        assert_eq!(KrpcMessage::parse(&msg.encode()).unwrap(), msg);
    }

    #[test]
    fn announce_peer_query_roundtrip() {
        let msg = KrpcMessage::Query {
            transaction_id: b"ap".to_vec(),
            query: DhtQuery::AnnouncePeer {
                id: id(1),
                implied_port: true,
                info_hash: [9u8; 20],
                port: 6881,
                token: b"tok".to_vec(),
            },
        };
        assert_eq!(KrpcMessage::parse(&msg.encode()).unwrap(), msg);
    }

    #[test]
    fn response_roundtrip_with_nodes_values_and_token() {
        let mut response = DhtResponse::new(id(4));
        response.nodes.push(KNode {
            id: id(5),
            addr: "127.0.0.1:6881".parse().unwrap(),
        });
        response.values.push("10.0.0.2:51413".parse().unwrap());
        response.values.push("[2001:db8::1]:51413".parse().unwrap());
        response.token = Some(b"token".to_vec());
        let msg = KrpcMessage::Response {
            transaction_id: b"rr".to_vec(),
            response,
        };
        assert_eq!(KrpcMessage::parse(&msg.encode()).unwrap(), msg);
    }

    #[test]
    fn error_roundtrip() {
        let msg = KrpcMessage::Error {
            transaction_id: b"ee".to_vec(),
            error: DhtError {
                code: 203,
                message: "protocol error".into(),
            },
        };
        assert_eq!(KrpcMessage::parse(&msg.encode()).unwrap(), msg);
    }

    #[test]
    fn rejects_bad_compact_lengths() {
        assert!(parse_compact_nodes(&[0; 25]).is_err());
        assert!(parse_compact_peers(&[0; 5]).is_err());
    }

    #[test]
    fn compact_peer_values_roundtrip_ipv4_and_ipv6() {
        let peers = vec![
            "10.0.0.2:51413".parse().unwrap(),
            "[2001:db8::1]:51413".parse().unwrap(),
        ];
        let compact = encode_compact_peer_values(&peers);

        assert_eq!(parse_compact_peers(&compact[0]).unwrap(), vec![peers[0]]);
        assert_eq!(parse_compact_peers(&compact[1]).unwrap(), vec![peers[1]]);
    }
}
