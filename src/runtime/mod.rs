use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use arti_client::TorClient;
use futures::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::{handle_rend_requests, StreamRequest};
use tor_rtcompat::PreferredRuntime;

use crate::discovery::{mdns, PeerEvent};
use crate::errors::CordError;
use crate::identity::{Identity, PeerId};
use crate::messaging::Frame;
use crate::noise::{self, NoiseStream, StaticKey};
use crate::transport::lan::LanTransport;
use crate::transport::onion::{self, OnionLaunch};

use self::events::{AppMsg, LocalAddrs, Role, TransportCmd};

pub mod events;

const ONION_PORT: u16 = 1;
const CONNECTION_QUEUE: usize = 32;

type Connections = Arc<Mutex<HashMap<[u8; 32], mpsc::Sender<Frame>>>>;

pub struct TransportTask {
    pub handle: JoinHandle<()>,
}

pub async fn spawn(
    identity: &Identity,
    msg_tx: mpsc::Sender<AppMsg>,
    mut cmd_rx: mpsc::Receiver<TransportCmd>,
) -> Result<TransportTask, CordError> {
    let peer_id = identity.peer_id;
    let config_dir = identity.config_dir.clone();
    let static_key = identity.noise_static.clone();

    let lan = LanTransport::bind().await?;
    let local = lan.local_addr()?;

    let connections: Connections = Arc::new(Mutex::new(HashMap::new()));

    let (peer_event_tx, peer_event_rx) = mpsc::channel(64);
    let mdns_handle = mdns::start(peer_id, local.port(), peer_event_tx)?;

    let _ = msg_tx
        .send(AppMsg::TransportReady(LocalAddrs { lan: local }))
        .await;

    let in_flight: Arc<Mutex<HashSet<PeerId>>> = Arc::new(Mutex::new(HashSet::new()));

    spawn_accept_loop(
        lan.listener,
        static_key.clone(),
        peer_id,
        msg_tx.clone(),
        connections.clone(),
    );
    spawn_discovery_loop(
        peer_event_rx,
        static_key.clone(),
        peer_id,
        in_flight,
        msg_tx.clone(),
        connections.clone(),
    );

    let (onion_connect_tx, onion_connect_rx) = mpsc::channel::<String>(8);
    spawn_onion(
        config_dir,
        msg_tx.clone(),
        static_key.clone(),
        peer_id,
        onion_connect_rx,
        connections.clone(),
    );

    let route_msg_tx = msg_tx.clone();
    let route_connections = connections.clone();
    let handle = tokio::spawn(async move {
        let mut mdns_handle = Some(mdns_handle);
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                TransportCmd::Shutdown => break,
                TransportCmd::ConnectOnion(addr) => {
                    let _ = onion_connect_tx.send(addr).await;
                }
                TransportCmd::SendMessage { remote_static, text } => {
                    let sender = {
                        let guard = route_connections.lock().await;
                        guard.get(&remote_static).cloned()
                    };
                    match sender {
                        Some(tx) => {
                            if tx.send(Frame::Text(text)).await.is_err() {
                                let _ = route_msg_tx
                                    .send(AppMsg::Log(
                                        "send failed: connection closed".into(),
                                    ))
                                    .await;
                            }
                        }
                        None => {
                            let _ = route_msg_tx
                                .send(AppMsg::Log(
                                    "no active connection to that contact".into(),
                                ))
                                .await;
                        }
                    }
                }
            }
        }
        if let Some(h) = mdns_handle.take() {
            h.shutdown();
        }
    });

    Ok(TransportTask { handle })
}

fn spawn_accept_loop(
    listener: TcpListener,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((sock, _addr)) => {
                    let static_key = static_key.clone();
                    let msg_tx = msg_tx.clone();
                    let connections = connections.clone();
                    tokio::spawn(async move {
                        handshake_as_responder_tcp(
                            sock,
                            static_key,
                            own_id,
                            msg_tx,
                            connections,
                        )
                        .await;
                    });
                }
                Err(e) => {
                    let _ = msg_tx
                        .send(AppMsg::Log(format!("accept failed: {e}")))
                        .await;
                    break;
                }
            }
        }
    });
}

fn spawn_discovery_loop(
    mut peer_event_rx: mpsc::Receiver<PeerEvent>,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    in_flight: Arc<Mutex<HashSet<PeerId>>>,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) {
    tokio::spawn(async move {
        while let Some(event) = peer_event_rx.recv().await {
            match event {
                PeerEvent::Discovered { peer_id, addr } => {
                    let _ = msg_tx
                        .send(AppMsg::PeerDiscovered { peer_id, addr })
                        .await;
                    if own_id < peer_id {
                        let mut guard = in_flight.lock().await;
                        if guard.insert(peer_id) {
                            drop(guard);
                            let static_key = static_key.clone();
                            let msg_tx = msg_tx.clone();
                            let in_flight = in_flight.clone();
                            let connections = connections.clone();
                            tokio::spawn(async move {
                                handshake_as_initiator_tcp(
                                    addr,
                                    static_key,
                                    own_id,
                                    peer_id,
                                    msg_tx,
                                    connections,
                                )
                                .await;
                                in_flight.lock().await.remove(&peer_id);
                            });
                        }
                    }
                }
                PeerEvent::Lost(peer_id) => {
                    let _ = msg_tx.send(AppMsg::PeerLost(peer_id)).await;
                }
            }
        }
    });
}

fn spawn_onion(
    config_dir: PathBuf,
    msg_tx: mpsc::Sender<AppMsg>,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    mut connect_rx: mpsc::Receiver<String>,
    connections: Connections,
) {
    tokio::spawn(async move {
        let _ = msg_tx
            .send(AppMsg::Log("tor: bootstrapping (this may take 10 to 30s)…".into()))
            .await;
        let launch = match onion::launch(config_dir, msg_tx.clone()).await {
            Ok(l) => l,
            Err(e) => {
                let _ = msg_tx.send(AppMsg::OnionFailed(e.to_string())).await;
                return;
            }
        };
        let OnionLaunch {
            onion_name,
            hs_id_bytes,
            service,
            tor_client,
            rend_requests,
        } = launch;

        let _ = msg_tx
            .send(AppMsg::OnionReady {
                onion_name: onion_name.clone(),
                hs_id: hs_id_bytes,
            })
            .await;

        let mut stream_requests = Box::pin(handle_rend_requests(rend_requests));

        loop {
            tokio::select! {
                // inbound: someone reached our onion
                Some(req) = stream_requests.next() => {
                    let static_key = static_key.clone();
                    let msg_tx = msg_tx.clone();
                    let connections = connections.clone();
                    tokio::spawn(async move {
                        accept_onion_stream(req, static_key, own_id, msg_tx, connections).await;
                    });
                }
                // outbound: user typed /connect
                Some(addr_str) = connect_rx.recv() => {
                    let client = tor_client.clone();
                    let static_key = static_key.clone();
                    let msg_tx = msg_tx.clone();
                    let connections = connections.clone();
                    tokio::spawn(async move {
                        connect_onion_peer(client, addr_str, static_key, own_id, msg_tx, connections).await;
                    });
                }
                else => break,
            }
        }
        drop(service);
    });
}

async fn handshake_as_initiator_tcp(
    addr: SocketAddr,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    expected_peer_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) {
    let result = async {
        let sock = TcpStream::connect(addr).await?;
        let mut stream = noise::handshake_initiator(sock, &static_key).await?;
        stream.send(own_id.as_bytes()).await?;
        let bytes = stream.recv().await?;
        let other = decode_peer_id(&bytes)?;
        let sas = noise::derive_sas(stream.handshake_hash());
        let remote_static = capture_remote_static(&stream)?;
        Ok::<(NoiseStream<TcpStream>, PeerId, String, [u8; 32]), noise::NoiseError>((
            stream,
            other,
            sas,
            remote_static,
        ))
    }
    .await;

    handle_handshake_result(result, Role::Initiator, Some(expected_peer_id), msg_tx, connections)
        .await;
}

async fn handshake_as_responder_tcp(
    sock: TcpStream,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) {
    let result = async {
        let mut stream = noise::handshake_responder(sock, &static_key).await?;
        let bytes = stream.recv().await?;
        let other = decode_peer_id(&bytes)?;
        stream.send(own_id.as_bytes()).await?;
        let sas = noise::derive_sas(stream.handshake_hash());
        let remote_static = capture_remote_static(&stream)?;
        Ok::<(NoiseStream<TcpStream>, PeerId, String, [u8; 32]), noise::NoiseError>((
            stream,
            other,
            sas,
            remote_static,
        ))
    }
    .await;

    handle_handshake_result(result, Role::Responder, None, msg_tx, connections).await;
}

async fn accept_onion_stream(
    req: StreamRequest,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) {
    let data_stream = match req.accept(Connected::new_empty()).await {
        Ok(s) => s,
        Err(e) => {
            let _ = msg_tx
                .send(AppMsg::Log(format!("onion: accept rejected: {e}")))
                .await;
            return;
        }
    };
    let compat: Compat<_> = data_stream.compat();
    let result = async {
        let mut stream = noise::handshake_responder(compat, &static_key).await?;
        let bytes = stream.recv().await?;
        let other = decode_peer_id(&bytes)?;
        stream.send(own_id.as_bytes()).await?;
        let sas = noise::derive_sas(stream.handshake_hash());
        let remote_static = capture_remote_static(&stream)?;
        Ok::<_, noise::NoiseError>((stream, other, sas, remote_static))
    }
    .await;

    handle_handshake_result(result, Role::Responder, None, msg_tx, connections).await;
}

async fn connect_onion_peer(
    client: TorClient<PreferredRuntime>,
    onion_addr: String,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) {
    let trimmed = onion_addr.trim().to_string();
    let _ = msg_tx
        .send(AppMsg::Log(format!("onion: connecting to {trimmed}…")))
        .await;
    let result = async {
        let data_stream = client
            .connect((trimmed.as_str(), ONION_PORT))
            .await
            .map_err(|e| noise::NoiseError::Io(std::io::Error::other(e.to_string())))?;
        let compat: Compat<_> = data_stream.compat();
        let mut stream = noise::handshake_initiator(compat, &static_key).await?;
        stream.send(own_id.as_bytes()).await?;
        let bytes = stream.recv().await?;
        let other = decode_peer_id(&bytes)?;
        let sas = noise::derive_sas(stream.handshake_hash());
        let remote_static = capture_remote_static(&stream)?;
        Ok::<_, noise::NoiseError>((stream, other, sas, remote_static))
    }
    .await;

    handle_handshake_result(result, Role::Initiator, None, msg_tx, connections).await;
}

async fn handle_handshake_result<S>(
    result: Result<(NoiseStream<S>, PeerId, String, [u8; 32]), noise::NoiseError>,
    role: Role,
    expected_peer_id: Option<PeerId>,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match result {
        Ok((stream, peer_id, sas, remote_static)) => {
            let (send_tx, send_rx) = mpsc::channel::<Frame>(CONNECTION_QUEUE);
            connections.lock().await.insert(remote_static, send_tx);
            let _ = msg_tx
                .send(AppMsg::HandshakeOk {
                    peer_id,
                    role,
                    sas,
                    remote_static,
                })
                .await;
            tokio::spawn(run_connection(
                stream,
                peer_id,
                remote_static,
                send_rx,
                msg_tx,
                connections,
            ));
        }
        Err(e) => {
            let _ = msg_tx
                .send(AppMsg::HandshakeFailed {
                    peer_id: expected_peer_id,
                    role,
                    error: e.to_string(),
                })
                .await;
        }
    }
}

async fn run_connection<S>(
    stream: NoiseStream<S>,
    peer_id: PeerId,
    remote_static: [u8; 32],
    mut send_rx: mpsc::Receiver<Frame>,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = stream.split();

    let read_msg_tx = msg_tx.clone();
    let read_task = tokio::spawn(async move {
        loop {
            match reader.recv().await {
                Ok(bytes) => match Frame::decode(&bytes) {
                    Ok(Frame::Text(text)) => {
                        if read_msg_tx
                            .send(AppMsg::MessageReceived {
                                from_peer_id: peer_id,
                                remote_static,
                                text,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(Frame::Ping) | Ok(Frame::Pong) => {}
                    Err(e) => {
                        let _ = read_msg_tx
                            .send(AppMsg::Log(format!("frame: {e}")))
                            .await;
                    }
                },
                Err(_) => break,
            }
        }
    });

    while let Some(frame) = send_rx.recv().await {
        if let Err(e) = writer.send(&frame.encode()).await {
            let _ = msg_tx.send(AppMsg::Log(format!("send: {e}"))).await;
            break;
        }
    }

    read_task.abort();
    connections.lock().await.remove(&remote_static);
    let _ = msg_tx
        .send(AppMsg::PeerDisconnected {
            peer_id,
            remote_static,
        })
        .await;
}

fn capture_remote_static<S>(stream: &NoiseStream<S>) -> Result<[u8; 32], noise::NoiseError> {
    let bytes = stream
        .remote_static()
        .ok_or_else(|| noise::NoiseError::BadPayload("no remote static after handshake".into()))?;
    if bytes.len() != 32 {
        return Err(noise::NoiseError::BadPayload(format!(
            "remote static length {} not 32",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

fn decode_peer_id(bytes: &[u8]) -> Result<PeerId, noise::NoiseError> {
    if bytes.len() != PeerId::BYTE_LEN {
        return Err(noise::NoiseError::BadPayload(format!(
            "expected {}-byte peer-id, got {}",
            PeerId::BYTE_LEN,
            bytes.len()
        )));
    }
    let mut id_bytes = [0u8; PeerId::BYTE_LEN];
    id_bytes.copy_from_slice(bytes);
    Ok(PeerId::from_bytes(id_bytes))
}
