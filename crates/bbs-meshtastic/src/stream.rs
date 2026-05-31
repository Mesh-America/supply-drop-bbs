//! Meshtastic serial/TCP stream client.

use std::{io, net::SocketAddr, time::Duration};

use prost::Message;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::{interval, sleep, MissedTickBehavior},
};
use tokio_serial::SerialPortBuilderExt;
use tracing::{debug, info, warn};

use crate::proto::{disconnect, heartbeat, want_config, FromRadio, ToRadio};

const START1: u8 = 0x94;
const START2: u8 = 0xc3;
const MAX_PROTO_LEN: usize = 512;

#[derive(Debug, Clone)]
pub struct TcpConfig {
    pub addr: SocketAddr,
    pub reconnect_delay_initial: Duration,
    pub reconnect_delay_max: Duration,
}

#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
    pub reconnect_delay_initial: Duration,
    pub reconnect_delay_max: Duration,
}

#[derive(Debug)]
pub enum ClientEvent {
    Connected,
    Disconnected { will_retry: bool },
    FromRadio(FromRadio),
}

pub struct MeshtasticClient {
    cmd_tx: mpsc::Sender<ToRadio>,
    event_rx: mpsc::Receiver<ClientEvent>,
}

impl MeshtasticClient {
    pub fn connect_tcp(config: TcpConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(64);
        tokio::spawn(run_tcp_worker(config, cmd_rx, event_tx));
        Self { cmd_tx, event_rx }
    }

    pub fn connect_serial(config: SerialConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(64);
        tokio::spawn(run_serial_worker(config, cmd_rx, event_tx));
        Self { cmd_tx, event_rx }
    }

    pub fn sender(&self) -> mpsc::Sender<ToRadio> {
        self.cmd_tx.clone()
    }

    pub async fn recv(&mut self) -> Option<ClientEvent> {
        self.event_rx.recv().await
    }
}

enum SessionOutcome {
    Shutdown,
    IoError(io::Error),
}

async fn run_tcp_worker(
    config: TcpConfig,
    mut cmd_rx: mpsc::Receiver<ToRadio>,
    event_tx: mpsc::Sender<ClientEvent>,
) {
    let mut backoff = config.reconnect_delay_initial;
    loop {
        match attempt_tcp_session(&config, &mut cmd_rx, &event_tx).await {
            SessionOutcome::Shutdown => break,
            SessionOutcome::IoError(e) => {
                warn!("meshtastic/tcp: session error: {e}");
                let _ = event_tx
                    .send(ClientEvent::Disconnected { will_retry: true })
                    .await;
                sleep(backoff).await;
                backoff = (backoff * 2).min(config.reconnect_delay_max);
            }
        }
    }
    let _ = event_tx
        .send(ClientEvent::Disconnected { will_retry: false })
        .await;
}

async fn attempt_tcp_session(
    config: &TcpConfig,
    cmd_rx: &mut mpsc::Receiver<ToRadio>,
    event_tx: &mpsc::Sender<ClientEvent>,
) -> SessionOutcome {
    let stream = match TcpStream::connect(config.addr).await {
        Ok(s) => s,
        Err(e) => return SessionOutcome::IoError(e),
    };
    let _ = stream.set_nodelay(true);
    info!(addr = %config.addr, "meshtastic/tcp: connected");
    let (mut reader, mut writer) = stream.into_split();
    run_session(&mut reader, &mut writer, cmd_rx, event_tx).await
}

async fn run_serial_worker(
    config: SerialConfig,
    mut cmd_rx: mpsc::Receiver<ToRadio>,
    event_tx: mpsc::Sender<ClientEvent>,
) {
    let mut backoff = config.reconnect_delay_initial;
    loop {
        match attempt_serial_session(&config, &mut cmd_rx, &event_tx).await {
            SessionOutcome::Shutdown => break,
            SessionOutcome::IoError(e) => {
                warn!("meshtastic/serial: session error: {e}");
                let _ = event_tx
                    .send(ClientEvent::Disconnected { will_retry: true })
                    .await;
                sleep(backoff).await;
                backoff = (backoff * 2).min(config.reconnect_delay_max);
            }
        }
    }
    let _ = event_tx
        .send(ClientEvent::Disconnected { will_retry: false })
        .await;
}

async fn attempt_serial_session(
    config: &SerialConfig,
    cmd_rx: &mut mpsc::Receiver<ToRadio>,
    event_tx: &mpsc::Sender<ClientEvent>,
) -> SessionOutcome {
    let stream = match tokio_serial::new(&config.port, config.baud_rate).open_native_async() {
        Ok(s) => s,
        Err(e) => {
            return SessionOutcome::IoError(io::Error::other(format!(
                "could not open serial port {}: {e}",
                config.port
            )));
        }
    };
    info!(port = %config.port, baud = config.baud_rate, "meshtastic/serial: opened");
    // CP210x and CH34x chips assert DTR on port open which can trigger a
    // brief Heltec/T-Beam reset. Give the firmware time to boot before
    // sending WantConfig, otherwise the handshake packet is lost and the
    // radio sits silent until reconnect.
    sleep(Duration::from_secs(2)).await;
    let (mut reader, mut writer) = tokio::io::split(stream);
    run_session(&mut reader, &mut writer, cmd_rx, event_tx).await
}

async fn run_session<R, W>(
    reader: &mut R,
    writer: &mut W,
    cmd_rx: &mut mpsc::Receiver<ToRadio>,
    event_tx: &mpsc::Sender<ClientEvent>,
) -> SessionOutcome
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if event_tx.send(ClientEvent::Connected).await.is_err() {
        return SessionOutcome::Shutdown;
    }

    if let Err(e) = write_to_radio(writer, &want_config(1)).await {
        return SessionOutcome::IoError(e);
    }

    let mut heartbeats = interval(Duration::from_secs(30));
    heartbeats.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut heartbeat_nonce = 1u32;

    loop {
        tokio::select! {
            frame = read_from_radio(reader) => match frame {
                Ok(frame) => {
                    debug!(frame = %describe_from_radio(&frame), "meshtastic: rx FromRadio");
                    if event_tx.send(ClientEvent::FromRadio(frame)).await.is_err() {
                        return SessionOutcome::Shutdown;
                    }
                }
                Err(e) => return SessionOutcome::IoError(e),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(cmd) => {
                    debug!("meshtastic: tx ToRadio");
                    if let Err(e) = write_to_radio(writer, &cmd).await {
                        return SessionOutcome::IoError(e);
                    }
                }
                None => {
                    let _ = write_to_radio(writer, &disconnect()).await;
                    return SessionOutcome::Shutdown;
                }
            },
            _ = heartbeats.tick() => {
                let hb = heartbeat(heartbeat_nonce);
                heartbeat_nonce = heartbeat_nonce.wrapping_add(1);
                if let Err(e) = write_to_radio(writer, &hb).await {
                    return SessionOutcome::IoError(e);
                }
            }
        }
    }
}

pub fn wrap_to_radio(msg: &ToRadio) -> Vec<u8> {
    let payload = msg.encode_to_vec();
    let len = payload.len().min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.push(START1);
    out.push(START2);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

async fn write_to_radio<W: AsyncWrite + Unpin>(writer: &mut W, msg: &ToRadio) -> io::Result<()> {
    let wire = wrap_to_radio(msg);
    if wire.len() - 4 > MAX_PROTO_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("ToRadio protobuf exceeds {MAX_PROTO_LEN} bytes"),
        ));
    }
    writer.write_all(&wire).await
}

/// One-line human description of a `FromRadio` frame for debug logging.
///
/// For packets it includes the portnum and request_id/reply_id so that admin
/// responses (portnum 67) and their correlation IDs are visible in the log —
/// essential for diagnosing whether the device answers admin GET requests.
fn describe_from_radio(frame: &FromRadio) -> String {
    use crate::proto::{from_radio::PayloadVariant as P, mesh_packet::PayloadVariant as MP};
    match &frame.payload_variant {
        Some(P::MyInfo(i)) => format!("MyInfo(node=0x{:08x})", i.my_node_num),
        Some(P::NodeInfo(_)) => "NodeInfo".to_owned(),
        Some(P::ConfigCompleteId(id)) => format!("ConfigCompleteId({id})"),
        Some(P::Rebooted(_)) => "Rebooted".to_owned(),
        Some(P::Packet(p)) => match &p.payload_variant {
            Some(MP::Decoded(d)) => format!(
                "Packet(port={}, from=0x{:08x}, req_id={}, reply_id={}, {}B)",
                d.portnum,
                p.from,
                d.request_id,
                d.reply_id,
                d.payload.len()
            ),
            Some(MP::Encrypted(_)) => "Packet(encrypted)".to_owned(),
            None => "Packet(empty)".to_owned(),
        },
        None => "<empty>".to_owned(),
    }
}

pub async fn read_from_radio<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<FromRadio> {
    loop {
        let mut b = [0u8; 1];
        reader.read_exact(&mut b).await?;
        if b[0] != START1 {
            continue;
        }
        reader.read_exact(&mut b).await?;
        if b[0] == START2 {
            break;
        }
    }

    let mut len_bytes = [0u8; 2];
    reader.read_exact(&mut len_bytes).await?;
    let len = u16::from_be_bytes(len_bytes) as usize;
    if len > MAX_PROTO_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("FromRadio protobuf length {len} exceeds {MAX_PROTO_LEN}"),
        ));
    }

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    FromRadio::decode(payload.as_slice())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::proto::{from_radio, MyNodeInfo};

    #[test]
    fn wrap_uses_meshtastic_header() {
        let wire = wrap_to_radio(&want_config(7));
        assert_eq!(wire[0], START1);
        assert_eq!(wire[1], START2);
        assert_eq!(
            u16::from_be_bytes([wire[2], wire[3]]) as usize,
            wire.len() - 4
        );
    }

    #[tokio::test]
    async fn read_skips_noise_until_header() {
        let msg = FromRadio {
            id: 1,
            payload_variant: Some(from_radio::PayloadVariant::MyInfo(MyNodeInfo {
                my_node_num: 123,
                reboot_count: 0,
                min_app_version: 0,
                device_id: Vec::new(),
                pio_env: String::new(),
                nodedb_count: 0,
            })),
        };
        let payload = msg.encode_to_vec();
        let mut bytes = vec![0, 1, 2, START1, 0, START1, START2];
        bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&payload);

        let (mut tx, mut rx) = tokio::io::duplex(128);
        tx.write_all(&bytes).await.unwrap();
        let decoded = read_from_radio(&mut rx).await.unwrap();
        assert_eq!(decoded.id, 1);
    }
}
