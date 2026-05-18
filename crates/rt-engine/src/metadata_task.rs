use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::Context;
use futures::{stream::FuturesUnordered, SinkExt, StreamExt};
use rt_bencode::decode;
use rt_metrics::{MemoryClass, MemoryLease, ResourceGovernor};
use rt_peer_wire::{
    codec::PeerCodec,
    extension::{ExtensionHandshake, UtMetadataMessage, EXT_HANDSHAKE_ID},
    handshake::{ExtensionFlags, Handshake},
    message::Message,
};
use rt_tracker::{
    udp::{UdpAnnounceRequest, UdpAnnounceResponse, UdpConnectRequest, UdpConnectResponse},
    AnnounceRequest, AnnounceResponse, InfoHash, TrackerError, TrackerEvent,
};
use sha1::{Digest, Sha1};
use tokio::net::TcpStream;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_util::codec::Framed;
use tracing::{debug, warn};
use url::Url;

use crate::command::EngineCmd;
use crate::peer_id::OUR_PEER_ID;
use crate::torrent_task::TorrentCmd;

const METADATA_PIECE_SIZE: usize = 16 * 1024;
const MAX_METADATA_SIZE: u32 = 16 * 1024 * 1024;
const LOCAL_UT_METADATA_ID: u8 = 1;
const MAX_METADATA_FETCH_CONCURRENCY: usize = 8;
const METADATA_PEER_RETRY_AFTER: Duration = Duration::from_secs(15);
const METADATA_PEER_ATTEMPT_CACHE_MIN: usize = 256;
const METADATA_PEER_ATTEMPT_CACHE_MULTIPLIER: usize = 4;

pub async fn run_metadata_task(
    info_hash: [u8; 20],
    info_hash_hex: String,
    trackers: Vec<String>,
    mut cmd_rx: mpsc::Receiver<TorrentCmd>,
    engine_tx: mpsc::Sender<EngineCmd>,
    resources: ResourceGovernor,
    listen_port: u16,
    max_peers: usize,
    http_timeout_secs: u64,
    udp_timeout_secs: u64,
    paused: bool,
) {
    let mut paused = paused;
    let mut peer_attempts = HashMap::new();
    let mut tracker_tick = interval(Duration::from_secs(60));
    let http_timeout = Duration::from_secs(http_timeout_secs);
    let udp_timeout = Duration::from_secs(udp_timeout_secs);
    let mut tracker_event = TrackerEvent::Started;
    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    TorrentCmd::NewPeers(peers) | TorrentCmd::PriorityPeers(peers) => {
                        if paused {
                            continue;
                        }
                        if try_fetch_from_peers(
                            info_hash,
                            &info_hash_hex,
                            &trackers,
                            peers,
                            max_peers,
                            &mut peer_attempts,
                            &engine_tx,
                            &resources,
                        )
                        .await {
                            return;
                        }
                    }
                    TorrentCmd::AcceptPeer {
                        stream,
                        peer_addr,
                        handshake,
                    } => if paused {
                        drop(stream);
                    } else {
                        match fetch_from_incoming_peer(stream, peer_addr, info_hash, handshake, resources.clone()).await {
                        Ok(info) => {
                            complete_metadata(&engine_tx, &info_hash_hex, &trackers, info).await;
                            return;
                        }
                        Err(e) => {
                            debug!(
                                component = "metadata",
                                operation = "fetch_incoming_peer",
                                torrent = %info_hash_hex,
                                peer = %peer_addr,
                                result = "error",
                                error = %e,
                                "incoming metadata fetch failed"
                            )
                        }
                    }},
                    TorrentCmd::Shutdown => {
                        if !paused {
                            announce_trackers(
                                info_hash,
                                &info_hash_hex,
                                &trackers,
                                listen_port,
                                max_peers,
                                http_timeout,
                                udp_timeout,
                                TrackerEvent::Stopped,
                            )
                            .await;
                        }
                        return;
                    }
                    TorrentCmd::Pause => {
                        if !paused {
                            announce_trackers(
                                info_hash,
                                &info_hash_hex,
                                &trackers,
                                listen_port,
                                max_peers,
                                http_timeout,
                                udp_timeout,
                                TrackerEvent::Stopped,
                            )
                            .await;
                        }
                        paused = true;
                    }
                    TorrentCmd::Resume => {
                        paused = false;
                        tracker_event = TrackerEvent::Started;
                        tracker_tick.reset_immediately();
                    }
                    TorrentCmd::Reannounce => {
                        paused = false;
                        tracker_tick.reset_immediately();
                    }
                    TorrentCmd::GetPeers { reply } => {
                        let _ = reply.send(Vec::new());
                    }
                    TorrentCmd::GetRuntimeStats { reply } => {
                        let _ = reply.send(Default::default());
                    }
                    TorrentCmd::Recheck { .. }
                    | TorrentCmd::CancelJob { .. }
                    | TorrentCmd::ReloadFilePolicy => {}
                }
            }
            _ = tracker_tick.tick(), if !paused && !trackers.is_empty() => {
                let peers = announce_trackers(
                    info_hash,
                    &info_hash_hex,
                    &trackers,
                    listen_port,
                    max_peers,
                    http_timeout,
                    udp_timeout,
                    tracker_event,
                ).await;
                if tracker_event == TrackerEvent::Started {
                    tracker_event = TrackerEvent::Empty;
                }
                if try_fetch_from_peers(
                    info_hash,
                    &info_hash_hex,
                    &trackers,
                    peers,
                    max_peers,
                    &mut peer_attempts,
                    &engine_tx,
                    &resources,
                )
                .await {
                    return;
                }
            }
            else => {
                if !paused {
                    announce_trackers(
                        info_hash,
                        &info_hash_hex,
                        &trackers,
                        listen_port,
                        max_peers,
                        http_timeout,
                        udp_timeout,
                        TrackerEvent::Stopped,
                    )
                    .await;
                }
                return;
            },
        }
    }
}

async fn try_fetch_from_peers(
    info_hash: [u8; 20],
    info_hash_hex: &str,
    trackers: &[String],
    peers: Vec<SocketAddr>,
    max_peers: usize,
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    engine_tx: &mpsc::Sender<EngineCmd>,
    resources: &ResourceGovernor,
) -> bool {
    let mut candidates = metadata_fetch_candidates(
        peers,
        peer_attempts,
        Instant::now(),
        metadata_peer_attempt_cache_cap(max_peers),
        metadata_peer_candidate_cap(max_peers),
    );
    let mut in_flight = FuturesUnordered::new();

    while in_flight.len() < MAX_METADATA_FETCH_CONCURRENCY {
        let Some(peer) = candidates.pop_front() else {
            break;
        };
        in_flight.push(metadata_fetch_attempt(peer, info_hash, resources.clone()));
    }

    while let Some((peer, result)) = in_flight.next().await {
        match result {
            Ok(info) => {
                complete_metadata(engine_tx, info_hash_hex, trackers, info).await;
                return true;
            }
            Err(e) => {
                debug!(
                    component = "metadata",
                    operation = "fetch_peer",
                    torrent = %info_hash_hex,
                    peer = %peer,
                    result = "error",
                    error = %e,
                    "metadata fetch failed"
                )
            }
        }

        if let Some(peer) = candidates.pop_front() {
            in_flight.push(metadata_fetch_attempt(peer, info_hash, resources.clone()));
        }
    }

    false
}

fn should_retry_peer(
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    peer: SocketAddr,
    now: Instant,
) -> bool {
    if peer_attempts
        .get(&peer)
        .is_some_and(|last| now.duration_since(*last) < METADATA_PEER_RETRY_AFTER)
    {
        return false;
    }
    peer_attempts.insert(peer, now);
    true
}

fn metadata_peer_attempt_cache_cap(max_peers: usize) -> usize {
    max_peers
        .saturating_mul(METADATA_PEER_ATTEMPT_CACHE_MULTIPLIER)
        .max(METADATA_PEER_ATTEMPT_CACHE_MIN)
}

fn metadata_peer_candidate_cap(max_peers: usize) -> usize {
    max_peers.max(MAX_METADATA_FETCH_CONCURRENCY)
}

fn metadata_fetch_candidates(
    peers: Vec<SocketAddr>,
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    now: Instant,
    attempt_cap: usize,
    candidate_cap: usize,
) -> VecDeque<SocketAddr> {
    peer_attempts.retain(|_, last| now.duration_since(*last) < METADATA_PEER_RETRY_AFTER);
    let mut candidates = VecDeque::new();
    for peer in peers {
        if candidates.len() >= candidate_cap {
            break;
        }
        if !should_retry_peer(peer_attempts, peer, now) {
            continue;
        }
        prune_metadata_peer_attempts(peer_attempts, attempt_cap, peer);
        candidates.push_back(peer);
    }
    candidates
}

fn prune_metadata_peer_attempts(
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    attempt_cap: usize,
    protected_peer: SocketAddr,
) {
    while peer_attempts.len() > attempt_cap {
        let Some(oldest_peer) = peer_attempts
            .iter()
            .filter(|(peer, _)| **peer != protected_peer)
            .min_by_key(|(_, attempted_at)| *attempted_at)
            .map(|(peer, _)| *peer)
        else {
            break;
        };
        peer_attempts.remove(&oldest_peer);
    }
}

async fn metadata_fetch_attempt(
    peer: SocketAddr,
    info_hash: [u8; 20],
    resources: ResourceGovernor,
) -> (SocketAddr, anyhow::Result<Vec<u8>>) {
    (
        peer,
        fetch_from_outgoing_peer(peer, info_hash, resources).await,
    )
}

async fn complete_metadata(
    engine_tx: &mpsc::Sender<EngineCmd>,
    info_hash_hex: &str,
    trackers: &[String],
    info: Vec<u8>,
) {
    let raw = build_torrent_from_info(&info, trackers);
    let _ = engine_tx
        .send(EngineCmd::CompleteMagnet {
            info_hash: info_hash_hex.to_owned(),
            raw,
        })
        .await;
}

async fn announce_trackers(
    info_hash: [u8; 20],
    info_hash_hex: &str,
    trackers: &[String],
    listen_port: u16,
    max_peers: usize,
    http_timeout: Duration,
    udp_timeout: Duration,
    event: TrackerEvent,
) -> Vec<SocketAddr> {
    let mut peers = Vec::new();
    let mut seen = HashSet::new();
    let peer_cap = metadata_peer_candidate_cap(max_peers);
    for tracker in trackers {
        match announce_tracker(
            tracker,
            info_hash,
            listen_port,
            max_peers,
            http_timeout,
            udp_timeout,
            event,
        )
        .await
        {
            Ok(resp) => {
                for peer in resp.peers.into_iter().map(|peer| peer.addr) {
                    if peers.len() >= peer_cap {
                        return peers;
                    }
                    if seen.insert(peer) {
                        peers.push(peer);
                    }
                }
            }
            Err(err) => {
                warn!(
                    component = "metadata",
                    operation = "tracker_announce",
                    torrent = %info_hash_hex,
                    tracker = %tracker,
                    result = "error",
                    error = %err,
                    "metadata tracker announce failed"
                );
            }
        }
    }
    peers
}

async fn announce_tracker(
    tracker_url: &str,
    info_hash: [u8; 20],
    listen_port: u16,
    max_peers: usize,
    http_timeout: Duration,
    udp_timeout: Duration,
    event: TrackerEvent,
) -> Result<AnnounceResponse, TrackerError> {
    if tracker_url.starts_with("udp://") {
        announce_udp(
            tracker_url,
            info_hash,
            listen_port,
            max_peers,
            udp_timeout,
            event,
        )
        .await
    } else {
        announce_http(
            tracker_url,
            info_hash,
            listen_port,
            max_peers,
            http_timeout,
            event,
        )
        .await
    }
}

async fn announce_http(
    tracker_url: &str,
    info_hash: [u8; 20],
    listen_port: u16,
    max_peers: usize,
    http_timeout: Duration,
    event: TrackerEvent,
) -> Result<AnnounceResponse, TrackerError> {
    if !tracker_url.starts_with("http://") && !tracker_url.starts_with("https://") {
        return Err(TrackerError::Disabled);
    }
    let req = metadata_announce_request(info_hash, listen_port, max_peers, event);
    let url = req.to_http_query(tracker_url)?;
    let response = reqwest::Client::builder()
        .timeout(http_timeout)
        .user_agent(crate::peer_id::USER_AGENT)
        .build()
        .map_err(|e| TrackerError::Network(e.to_string()))?
        .get(url)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                TrackerError::Timeout
            } else {
                TrackerError::Network(e.to_string())
            }
        })?;
    if !response.status().is_success() {
        return Err(TrackerError::Http {
            status: response.status().as_u16(),
        });
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    AnnounceResponse::parse(&bytes)
}

async fn announce_udp(
    tracker_url: &str,
    info_hash: [u8; 20],
    listen_port: u16,
    max_peers: usize,
    udp_timeout: Duration,
    event: TrackerEvent,
) -> Result<AnnounceResponse, TrackerError> {
    let url = Url::parse(tracker_url).map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
    let host = url
        .host_str()
        .ok_or_else(|| TrackerError::InvalidUrl("missing UDP tracker host".into()))?;
    let port = url
        .port()
        .ok_or_else(|| TrackerError::InvalidUrl("missing UDP tracker port".into()))?;
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let tracker_addr = addrs
        .next()
        .ok_or_else(|| TrackerError::Network(format!("no address for {host}:{port}")))?;

    let bind_addr = if tracker_addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_addr)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    socket
        .connect(tracker_addr)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let connect = UdpConnectRequest::new();
    socket
        .send(&connect.encode())
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let mut buf = vec![0u8; 1500];
    let n = tokio::time::timeout(udp_timeout, socket.recv(&mut buf))
        .await
        .map_err(|_| TrackerError::Timeout)?
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let connect_resp = UdpConnectResponse::parse(&buf[..n])?;
    if connect_resp.transaction_id != connect.transaction_id {
        return Err(TrackerError::Udp("connect transaction id mismatch".into()));
    }

    let announce = UdpAnnounceRequest::new(
        connect_resp.connection_id,
        metadata_announce_request(info_hash, listen_port, max_peers, event),
    );
    let encoded = announce.encode()?;
    socket
        .send(&encoded)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let n = tokio::time::timeout(udp_timeout, socket.recv(&mut buf))
        .await
        .map_err(|_| TrackerError::Timeout)?
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let announce_resp = UdpAnnounceResponse::parse(&buf[..n])?;
    if announce_resp.transaction_id != announce.transaction_id {
        return Err(TrackerError::Udp("announce transaction id mismatch".into()));
    }

    Ok(AnnounceResponse {
        interval: announce_resp.interval,
        min_interval: None,
        peers: announce_resp.peers,
        tracker_id: None,
        warning_message: None,
        complete: Some(announce_resp.seeders),
        incomplete: Some(announce_resp.leechers),
    })
}

fn metadata_announce_request(
    info_hash: [u8; 20],
    listen_port: u16,
    max_peers: usize,
    event: TrackerEvent,
) -> AnnounceRequest {
    AnnounceRequest {
        info_hash: InfoHash::V1(info_hash),
        peer_id: OUR_PEER_ID,
        port: listen_port,
        uploaded: 0,
        downloaded: 0,
        left: 0,
        event,
        compact: true,
        numwant: Some(max_peers as u32),
    }
}

async fn fetch_from_outgoing_peer(
    addr: SocketAddr,
    info_hash: [u8; 20],
    resources: ResourceGovernor,
) -> anyhow::Result<Vec<u8>> {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;
    stream.set_nodelay(true)?;
    let mut framed = Framed::new(stream, PeerCodec);
    write_handshake(&mut framed, info_hash).await?;

    let remote_hs = read_handshake(&mut framed).await?;
    if remote_hs.info_hash != info_hash {
        anyhow::bail!("info_hash mismatch from {addr}");
    }
    fetch_metadata(
        addr,
        framed,
        remote_hs.reserved.supports_extension_protocol(),
        info_hash,
        resources,
    )
    .await
}

async fn fetch_from_incoming_peer(
    stream: TcpStream,
    addr: SocketAddr,
    info_hash: [u8; 20],
    remote_hs: Handshake,
    resources: ResourceGovernor,
) -> anyhow::Result<Vec<u8>> {
    stream.set_nodelay(true)?;
    let mut framed = Framed::new(stream, PeerCodec);
    write_handshake(&mut framed, info_hash).await?;
    fetch_metadata(
        addr,
        framed,
        remote_hs.reserved.supports_extension_protocol(),
        info_hash,
        resources,
    )
    .await
}

async fn write_handshake(
    framed: &mut Framed<TcpStream, PeerCodec>,
    info_hash: [u8; 20],
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let hs = Handshake {
        info_hash,
        peer_id: OUR_PEER_ID,
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    framed.get_mut().write_all(&hs.encode()).await?;
    Ok(())
}

async fn read_handshake(framed: &mut Framed<TcpStream, PeerCodec>) -> anyhow::Result<Handshake> {
    use tokio::io::AsyncReadExt;
    let mut hs_buf = [0u8; 68];
    tokio::time::timeout(
        Duration::from_secs(10),
        framed.get_mut().read_exact(&mut hs_buf),
    )
    .await??;
    Ok(Handshake::parse(&hs_buf)?)
}

async fn fetch_metadata(
    addr: SocketAddr,
    mut framed: Framed<TcpStream, PeerCodec>,
    remote_supports_extension: bool,
    expected_info_hash: [u8; 20],
    resources: ResourceGovernor,
) -> anyhow::Result<Vec<u8>> {
    if !remote_supports_extension {
        anyhow::bail!("peer does not support BEP 10");
    }
    framed
        .send(Message::Extended {
            ext_id: EXT_HANDSHAKE_ID,
            payload: ExtensionHandshake::new(None)
                .with_ut_metadata(LOCAL_UT_METADATA_ID)
                .encode(),
        })
        .await?;
    framed.send(Message::Interested).await?;

    let (remote_ext_id, metadata_size) = read_remote_metadata_handshake(addr, &mut framed).await?;
    let _lease = reserve_metadata_fetch_bytes(&resources, metadata_size)?;
    let piece_count = metadata_size.div_ceil(METADATA_PIECE_SIZE as u32);
    let mut pieces = BTreeMap::new();

    for piece in 0..piece_count {
        framed
            .send(Message::Extended {
                ext_id: remote_ext_id,
                payload: UtMetadataMessage::Request { piece }.encode(),
            })
            .await?;
        match read_metadata_piece(
            addr,
            &mut framed,
            LOCAL_UT_METADATA_ID,
            piece,
            metadata_size,
        )
        .await?
        {
            Some(data) => {
                pieces.insert(piece, data);
            }
            None => anyhow::bail!("peer rejected metadata piece {piece}"),
        }
    }

    let mut metadata = Vec::with_capacity(metadata_size as usize);
    for (_, data) in pieces {
        metadata.extend_from_slice(&data);
    }
    metadata.truncate(metadata_size as usize);
    decode(&metadata).context("fetched metadata is not valid bencode")?;
    validate_metadata_info_hash(&metadata, expected_info_hash)?;
    Ok(metadata)
}

fn reserve_metadata_fetch_bytes(
    resources: &ResourceGovernor,
    metadata_size: u32,
) -> anyhow::Result<MemoryLease> {
    let bytes = u64::from(metadata_size).saturating_mul(2);
    resources
        .try_acquire(MemoryClass::Metadata, bytes)
        .ok_or_else(|| anyhow::anyhow!("metadata allocation of {bytes} bytes denied"))
}

fn validate_metadata_info_hash(
    metadata: &[u8],
    expected_info_hash: [u8; 20],
) -> anyhow::Result<()> {
    let mut hasher = Sha1::new();
    hasher.update(metadata);
    let actual: [u8; 20] = hasher.finalize().into();
    if actual != expected_info_hash {
        anyhow::bail!(
            "fetched metadata infohash {} does not match expected {}",
            hex::encode(actual),
            hex::encode(expected_info_hash)
        );
    }
    Ok(())
}

async fn read_remote_metadata_handshake(
    addr: SocketAddr,
    framed: &mut Framed<TcpStream, PeerCodec>,
) -> anyhow::Result<(u8, u32)> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), framed.next())
            .await?
            .ok_or_else(|| anyhow::anyhow!("peer closed before extension handshake"))??;
        if let Message::Extended {
            ext_id: EXT_HANDSHAKE_ID,
            payload,
        } = msg
        {
            let handshake = ExtensionHandshake::parse(&payload)?;
            let remote_ext_id = handshake
                .ut_metadata_id()
                .ok_or_else(|| anyhow::anyhow!("peer does not advertise ut_metadata"))?;
            let metadata_size = handshake
                .metadata_size
                .ok_or_else(|| anyhow::anyhow!("peer did not send metadata_size"))?;
            if metadata_size == 0 || metadata_size > MAX_METADATA_SIZE {
                anyhow::bail!("metadata_size {metadata_size} from {addr} is invalid");
            }
            return Ok((remote_ext_id, metadata_size));
        }
    }
}

async fn read_metadata_piece(
    _addr: SocketAddr,
    framed: &mut Framed<TcpStream, PeerCodec>,
    local_ext_id: u8,
    expected_piece: u32,
    expected_total_size: u32,
) -> anyhow::Result<Option<Vec<u8>>> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), framed.next())
            .await?
            .ok_or_else(|| anyhow::anyhow!("peer closed during metadata transfer"))??;
        let Message::Extended { ext_id, payload } = msg else {
            continue;
        };
        if ext_id == EXT_HANDSHAKE_ID {
            continue;
        }
        if ext_id != local_ext_id {
            continue;
        }
        match UtMetadataMessage::parse(&payload)? {
            UtMetadataMessage::Data {
                piece,
                total_size,
                data,
            } if piece == expected_piece => {
                validate_metadata_piece(
                    expected_piece,
                    expected_total_size,
                    total_size,
                    data.len(),
                )?;
                return Ok(Some(data));
            }
            UtMetadataMessage::Reject { piece } if piece == expected_piece => return Ok(None),
            _ => {}
        }
    }
}

fn validate_metadata_piece(
    piece: u32,
    expected_total_size: u32,
    total_size: u32,
    data_len: usize,
) -> anyhow::Result<()> {
    if total_size != expected_total_size {
        anyhow::bail!(
            "metadata piece {piece} total_size {total_size} does not match expected {expected_total_size}"
        );
    }
    let start = piece as usize * METADATA_PIECE_SIZE;
    if start >= expected_total_size as usize {
        anyhow::bail!("metadata piece {piece} starts past metadata size");
    }
    let expected_len = (expected_total_size as usize - start).min(METADATA_PIECE_SIZE);
    if data_len != expected_len {
        anyhow::bail!(
            "metadata piece {piece} length {data_len} does not match expected {expected_len}"
        );
    }
    Ok(())
}

fn build_torrent_from_info(info: &[u8], trackers: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(b'd');
    if let Some(first) = trackers.first() {
        write_bytes_key(&mut out, b"announce");
        write_bytes(&mut out, first.as_bytes());
        write_bytes_key(&mut out, b"announce-list");
        out.push(b'l');
        for tracker in trackers {
            out.push(b'l');
            write_bytes(&mut out, tracker.as_bytes());
            out.push(b'e');
        }
        out.push(b'e');
    }
    write_bytes_key(&mut out, b"info");
    out.extend_from_slice(info);
    out.push(b'e');
    out
}

fn write_bytes_key(out: &mut Vec<u8>, key: &[u8]) {
    write_bytes(out, key);
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn validates_metadata_piece_lengths() {
        validate_metadata_piece(0, 20_000, 20_000, METADATA_PIECE_SIZE).unwrap();
        validate_metadata_piece(1, 20_000, 20_000, 20_000 - METADATA_PIECE_SIZE).unwrap();

        assert!(validate_metadata_piece(0, 20_000, 19_999, METADATA_PIECE_SIZE).is_err());
        assert!(validate_metadata_piece(1, 20_000, 20_000, METADATA_PIECE_SIZE).is_err());
        assert!(validate_metadata_piece(2, 20_000, 20_000, 1).is_err());
    }

    #[test]
    fn validates_metadata_info_hash() {
        let info = b"d4:name4:teste";
        let mut hasher = Sha1::new();
        hasher.update(info);
        let expected: [u8; 20] = hasher.finalize().into();

        validate_metadata_info_hash(info, expected).unwrap();
        assert!(validate_metadata_info_hash(info, [0; 20]).is_err());
    }

    #[test]
    fn metadata_peer_retry_has_cooldown() {
        let peer: SocketAddr = "127.0.0.1:6881".parse().unwrap();
        let now = Instant::now();
        let mut attempts = HashMap::new();

        assert!(should_retry_peer(&mut attempts, peer, now));
        assert!(!should_retry_peer(
            &mut attempts,
            peer,
            now + Duration::from_secs(10)
        ));
        assert!(should_retry_peer(
            &mut attempts,
            peer,
            now + METADATA_PEER_RETRY_AFTER + Duration::from_secs(1)
        ));
    }

    #[test]
    fn metadata_fetch_candidates_are_bounded_and_prune_retry_history() {
        let now = Instant::now();
        let mut attempts = HashMap::from([
            (
                "127.0.0.1:6881".parse().unwrap(),
                now - METADATA_PEER_RETRY_AFTER - Duration::from_secs(1),
            ),
            ("127.0.0.2:6881".parse().unwrap(), now),
        ]);
        let peers = vec![
            "127.0.0.1:6881".parse().unwrap(),
            "127.0.0.2:6881".parse().unwrap(),
            "127.0.0.3:6881".parse().unwrap(),
            "127.0.0.4:6881".parse().unwrap(),
        ];

        let candidates = metadata_fetch_candidates(peers, &mut attempts, now, 2, 2);

        assert_eq!(candidates.len(), 2);
        assert!(candidates.contains(&"127.0.0.1:6881".parse().unwrap()));
        assert!(candidates.contains(&"127.0.0.3:6881".parse().unwrap()));
        assert_eq!(attempts.len(), 2);
        assert!(attempts.contains_key(&"127.0.0.3:6881".parse().unwrap()));
    }

    #[test]
    fn metadata_attempt_cache_cap_scales_with_peer_limit() {
        assert_eq!(
            metadata_peer_attempt_cache_cap(1),
            METADATA_PEER_ATTEMPT_CACHE_MIN
        );
        assert_eq!(metadata_peer_attempt_cache_cap(100), 400);
        assert_eq!(
            metadata_peer_candidate_cap(1),
            MAX_METADATA_FETCH_CONCURRENCY
        );
        assert_eq!(metadata_peer_candidate_cap(100), 100);
    }

    #[test]
    fn metadata_fetch_reservation_uses_metadata_governor_class() {
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::Metadata as usize] = 128;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 128,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_metadata_fetch_bytes(&governor, 64).unwrap();
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            128
        );
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            0
        );
        assert!(reserve_metadata_fetch_bytes(&governor, 65).is_err());
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].denied_allocations,
            1
        );
    }

    #[tokio::test]
    async fn dht_only_peer_candidates_can_complete_magnet_metadata() {
        let info =
            b"d6:lengthi4e4:name4:test12:piece lengthi16384e6:pieces20:abcdefghijklmnopqrste";
        let mut hasher = Sha1::new();
        hasher.update(info);
        let info_hash: [u8; 20] = hasher.finalize().into();
        let info_hash_hex = hex::encode(info_hash);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let info_for_peer = info.to_vec();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut handshake = [0_u8; 68];
            stream.read_exact(&mut handshake).await.unwrap();
            let remote = Handshake::parse(&handshake).unwrap();
            let response = Handshake {
                info_hash: remote.info_hash,
                peer_id: [b'P'; 20],
                reserved: ExtensionFlags::with_extension_protocol(),
            };
            stream.write_all(&response.encode()).await.unwrap();

            let mut framed = Framed::new(stream, PeerCodec);
            framed
                .send(Message::Extended {
                    ext_id: EXT_HANDSHAKE_ID,
                    payload: ExtensionHandshake::new(Some(info_for_peer.len() as u32))
                        .with_ut_metadata(7)
                        .encode(),
                })
                .await
                .unwrap();
            while let Some(message) = framed.next().await {
                let Message::Extended { ext_id: 7, payload } = message.unwrap() else {
                    continue;
                };
                let UtMetadataMessage::Request { piece } =
                    UtMetadataMessage::parse(&payload).unwrap()
                else {
                    continue;
                };
                let start = piece as usize * METADATA_PIECE_SIZE;
                let end = (start + METADATA_PIECE_SIZE).min(info_for_peer.len());
                framed
                    .send(Message::Extended {
                        ext_id: LOCAL_UT_METADATA_ID,
                        payload: UtMetadataMessage::Data {
                            piece,
                            total_size: info_for_peer.len() as u32,
                            data: info_for_peer[start..end].to_vec(),
                        }
                        .encode(),
                    })
                    .await
                    .unwrap();
                break;
            }
        });
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::Metadata as usize] = 1024 * 1024;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 1024 * 1024,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let (engine_tx, mut engine_rx) = mpsc::channel(1);
        let mut attempts = HashMap::new();

        assert!(
            try_fetch_from_peers(
                info_hash,
                &info_hash_hex,
                &[],
                vec![peer_addr],
                8,
                &mut attempts,
                &engine_tx,
                &governor,
            )
            .await
        );
        let cmd = engine_rx.recv().await.unwrap();
        match cmd {
            EngineCmd::CompleteMagnet { info_hash, raw } => {
                assert_eq!(info_hash, info_hash_hex);
                let parsed = rt_metainfo::parse_torrent(&raw).unwrap();
                assert_eq!(parsed.name(), "test");
            }
            other => panic!("unexpected engine command: {other:?}"),
        }
        peer.await.unwrap();
    }
}
