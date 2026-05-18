use std::{
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rand::Rng;
use tokio::{net::UdpSocket, time::timeout};

use crate::{
    error::UtpError,
    packet::{PacketType, UtpPacket},
    state::{InboundAction, UtpConnection, DEFAULT_MTU_PAYLOAD_BYTES},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtpTransportConfig {
    pub handshake_timeout: Duration,
    pub io_timeout: Duration,
    pub max_datagram_len: usize,
    pub max_retransmits: usize,
}

impl Default for UtpTransportConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(15),
            max_datagram_len: DEFAULT_MTU_PAYLOAD_BYTES + crate::HEADER_SIZE + 64,
            max_retransmits: 4,
        }
    }
}

pub struct UtpListener {
    socket: UdpSocket,
    config: UtpTransportConfig,
}

pub struct UtpStream {
    socket: UdpSocket,
    peer: SocketAddr,
    conn: UtpConnection,
    config: UtpTransportConfig,
    last_remote_timestamp_us: u32,
}

impl UtpListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self, UtpError> {
        Self::bind_with_config(addr, UtpTransportConfig::default()).await
    }

    pub async fn bind_with_config(
        addr: SocketAddr,
        config: UtpTransportConfig,
    ) -> Result<Self, UtpError> {
        let socket = UdpSocket::bind(addr)
            .await
            .map_err(|err| UtpError::Io(err.to_string()))?;
        Ok(Self { socket, config })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, UtpError> {
        self.socket
            .local_addr()
            .map_err(|err| UtpError::Io(err.to_string()))
    }

    pub async fn accept(self) -> Result<UtpStream, UtpError> {
        let mut buf = vec![0u8; self.config.max_datagram_len];
        loop {
            let (len, peer) = timeout(
                self.config.handshake_timeout,
                self.socket.recv_from(&mut buf),
            )
            .await
            .map_err(|_| UtpError::Timeout)?
            .map_err(|err| UtpError::Io(err.to_string()))?;
            let packet = UtpPacket::parse(&buf[..len])?;
            if packet.header.packet_type != PacketType::Syn {
                continue;
            }

            self.socket
                .connect(peer)
                .await
                .map_err(|err| UtpError::Io(err.to_string()))?;
            let conn = UtpConnection::accept(&packet.header, random_seq_nr())?;
            let state = conn.build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
            send_packet(&self.socket, &state).await?;
            return Ok(UtpStream {
                socket: self.socket,
                peer,
                conn,
                config: self.config,
                last_remote_timestamp_us: packet.header.timestamp_us,
            });
        }
    }
}

impl UtpStream {
    pub async fn connect(peer: SocketAddr) -> Result<Self, UtpError> {
        Self::connect_with_config(peer, UtpTransportConfig::default()).await
    }

    pub async fn connect_with_config(
        peer: SocketAddr,
        config: UtpTransportConfig,
    ) -> Result<Self, UtpError> {
        let bind_addr = if peer.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind_addr)
            .await
            .map_err(|err| UtpError::Io(err.to_string()))?;
        socket
            .connect(peer)
            .await
            .map_err(|err| UtpError::Io(err.to_string()))?;
        let mut conn = UtpConnection::connect(random_connection_id(), random_seq_nr());
        let syn = conn.build_syn(now_us());
        let mut packet = None;
        for _ in 0..=config.max_retransmits {
            send_packet(&socket, &syn).await?;
            match recv_packet(&socket, config.handshake_timeout, config.max_datagram_len).await {
                Ok(received) => {
                    packet = Some(received);
                    break;
                }
                Err(UtpError::Timeout) => continue,
                Err(err) => return Err(err),
            }
        }
        let packet = packet.ok_or(UtpError::Timeout)?;
        conn.on_inbound(&packet)?;
        if !conn.is_established() {
            return Err(UtpError::InvalidStatePacket {
                state: conn.state(),
                packet_type: packet.header.packet_type,
            });
        }
        Ok(Self {
            socket,
            peer,
            conn,
            config,
            last_remote_timestamp_us: packet.header.timestamp_us,
        })
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    pub fn connection(&self) -> &UtpConnection {
        &self.conn
    }

    pub async fn send(&mut self, payload: &[u8]) -> Result<(), UtpError> {
        for chunk in payload.chunks(DEFAULT_MTU_PAYLOAD_BYTES) {
            let packet = self.conn.build_data(
                now_us(),
                timestamp_diff_us(self.last_remote_timestamp_us),
                chunk.to_vec(),
            );
            let mut acknowledged = false;
            for _ in 0..=self.config.max_retransmits {
                send_packet(&self.socket, &packet).await?;
                loop {
                    let ack = match recv_packet(
                        &self.socket,
                        self.config.io_timeout,
                        self.config.max_datagram_len,
                    )
                    .await
                    {
                        Ok(ack) => ack,
                        Err(UtpError::Timeout) => {
                            self.conn.on_timeout();
                            break;
                        }
                        Err(err) => return Err(err),
                    };
                    self.last_remote_timestamp_us = ack.header.timestamp_us;
                    self.conn.on_inbound(&ack)?;
                    if ack.header.packet_type == PacketType::State {
                        acknowledged = true;
                        break;
                    }
                }
                if acknowledged {
                    break;
                }
            }
            if !acknowledged {
                return Err(UtpError::Timeout);
            }
        }
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Vec<u8>, UtpError> {
        loop {
            let packet = recv_packet(
                &self.socket,
                self.config.io_timeout,
                self.config.max_datagram_len,
            )
            .await?;
            self.last_remote_timestamp_us = packet.header.timestamp_us;
            match self.conn.on_inbound(&packet)? {
                InboundAction::DeliverPayload => {
                    let ack = self
                        .conn
                        .build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
                    send_packet(&self.socket, &ack).await?;
                    return Ok(packet.payload);
                }
                InboundAction::Close => {
                    let ack = self
                        .conn
                        .build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
                    send_packet(&self.socket, &ack).await?;
                    self.conn.mark_closed();
                    return Ok(Vec::new());
                }
                InboundAction::Reset => return Ok(Vec::new()),
                InboundAction::None | InboundAction::SendState => {}
            }
        }
    }

    pub async fn close(&mut self) -> Result<(), UtpError> {
        let fin = self
            .conn
            .build_fin(now_us(), timestamp_diff_us(self.last_remote_timestamp_us));
        let mut packet = None;
        for _ in 0..=self.config.max_retransmits {
            send_packet(&self.socket, &fin).await?;
            match recv_packet(
                &self.socket,
                self.config.io_timeout,
                self.config.max_datagram_len,
            )
            .await
            {
                Ok(received) => {
                    packet = Some(received);
                    break;
                }
                Err(UtpError::Timeout) => {
                    self.conn.on_timeout();
                    continue;
                }
                Err(err) => return Err(err),
            }
        }
        let packet = packet.ok_or(UtpError::Timeout)?;
        self.conn.on_inbound(&packet)?;
        self.conn.mark_closed();
        Ok(())
    }
}

async fn send_packet(socket: &UdpSocket, packet: &UtpPacket) -> Result<(), UtpError> {
    let bytes = packet.encode()?;
    socket
        .send(&bytes)
        .await
        .map(|_| ())
        .map_err(|err| UtpError::Io(err.to_string()))
}

async fn recv_packet(
    socket: &UdpSocket,
    wait: Duration,
    max_datagram_len: usize,
) -> Result<UtpPacket, UtpError> {
    let mut buf = vec![0u8; max_datagram_len];
    let len = timeout(wait, socket.recv(&mut buf))
        .await
        .map_err(|_| UtpError::Timeout)?
        .map_err(|err| UtpError::Io(err.to_string()))?;
    UtpPacket::parse(&buf[..len])
}

fn random_connection_id() -> u16 {
    rand::thread_rng().gen()
}

fn random_seq_nr() -> u16 {
    rand::thread_rng().gen()
}

fn now_us() -> u32 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    elapsed.as_micros() as u32
}

fn timestamp_diff_us(remote_timestamp_us: u32) -> u32 {
    now_us().wrapping_sub(remote_timestamp_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> UtpTransportConfig {
        UtpTransportConfig {
            handshake_timeout: Duration::from_secs(2),
            io_timeout: Duration::from_secs(2),
            max_datagram_len: 2048,
            max_retransmits: 1,
        }
    }

    #[tokio::test]
    async fn utp_stream_connects_and_exchanges_payload() {
        let listener = UtpListener::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let payload = stream.recv().await.unwrap();
            assert_eq!(payload, b"hello over utp");
            stream.send(b"ack").await.unwrap();
            stream.close().await.unwrap();
        });

        let mut client = UtpStream::connect_with_config(addr, test_config())
            .await
            .unwrap();
        client.send(b"hello over utp").await.unwrap();
        assert_eq!(client.recv().await.unwrap(), b"ack");
        let _ = client.recv().await.unwrap();
        server.await.unwrap();
    }
}
