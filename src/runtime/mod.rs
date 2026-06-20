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

use crate::crypto::{self, Vault};
use crate::discovery::{mdns, PeerEvent};
use crate::errors::CordError;
use crate::identity::store::write_atomic_0600;
use crate::identity::{Identity, PeerId};
use crate::messaging::{Frame, Queue, QueuedMessage};
use crate::noise::{self, NoiseStream, StaticKey};
use crate::transport::lan::LanTransport;
use crate::transport::onion::{self, OnionLaunch};

use self::events::{AppMsg, ContactRoute, DeliveryStatus, LocalAddrs, Role, TransportCmd};

pub mod events;
mod retry;

const ONION_PORT: u16 = 1;
const CONNECTION_QUEUE: usize = 32;

type Connections = Arc<Mutex<HashMap<[u8; 32], mpsc::Sender<Frame>>>>;
type SharedQueue = Arc<Mutex<Option<Queue>>>;
/// Verified contacts the retry loop may poll, synced from the TUI.
type SharedRoutes = Arc<Mutex<Vec<ContactRoute>>>;

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
    let queue: SharedQueue = Arc::new(Mutex::new(None));
    let routes: SharedRoutes = Arc::new(Mutex::new(Vec::new()));
    let (retry_kick_tx, retry_kick_rx) = mpsc::channel::<()>(8);

    let (peer_event_tx, peer_event_rx) = mpsc::channel(64);
    let mdns_handle = mdns::start(peer_id, local.port(), peer_event_tx)?;

    let _ = msg_tx
        .send(AppMsg::TransportReady(LocalAddrs { lan: local }))
        .await;

    if crypto::vault_file_path(&config_dir).exists() {
        let _ = msg_tx.send(AppMsg::VaultLocked).await;
    }

    let in_flight: Arc<Mutex<HashSet<PeerId>>> = Arc::new(Mutex::new(HashSet::new()));

    spawn_accept_loop(
        lan.listener,
        static_key.clone(),
        peer_id,
        msg_tx.clone(),
        connections.clone(),
        queue.clone(),
    );
    spawn_discovery_loop(
        peer_event_rx,
        static_key.clone(),
        peer_id,
        in_flight,
        msg_tx.clone(),
        connections.clone(),
        queue.clone(),
    );

    let (onion_connect_tx, onion_connect_rx) = mpsc::channel::<String>(8);
    spawn_onion(
        config_dir.clone(),
        msg_tx.clone(),
        static_key.clone(),
        peer_id,
        onion_connect_rx,
        connections.clone(),
        queue.clone(),
        routes.clone(),
        retry_kick_rx,
    );

    let route_msg_tx = msg_tx.clone();
    let route_connections = connections.clone();
    let route_queue = queue.clone();
    let route_config_dir = config_dir.clone();
    let route_routes = routes.clone();
    let route_kick_tx = retry_kick_tx.clone();
    let handle = tokio::spawn(async move {
        let mut mdns_handle = Some(mdns_handle);
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                TransportCmd::Shutdown => break,
                TransportCmd::ConnectOnion(addr) => {
                    let _ = onion_connect_tx.send(addr).await;
                }
                TransportCmd::SetupVault(passphrase) => {
                    let path = crypto::vault_file_path(&route_config_dir);
                    if path.exists() {
                        let _ = route_msg_tx
                            .send(AppMsg::VaultFailed(
                                "a vault already exists; use /unlock".into(),
                            ))
                            .await;
                    } else {
                        match Vault::create(&passphrase.0) {
                            Ok((vault, file)) => match write_atomic_0600(&path, &file) {
                                Ok(()) => {
                                    *route_queue.lock().await =
                                        Some(Queue::new(&route_config_dir, Arc::new(vault)));
                                    let _ = route_msg_tx.send(AppMsg::VaultReady).await;
                                }
                                Err(e) => {
                                    let _ = route_msg_tx
                                        .send(AppMsg::VaultFailed(format!(
                                            "could not write vault file: {e}"
                                        )))
                                        .await;
                                }
                            },
                            Err(e) => {
                                let _ = route_msg_tx
                                    .send(AppMsg::VaultFailed(e.to_string()))
                                    .await;
                            }
                        }
                    }
                }
                TransportCmd::UnlockVault(passphrase) => {
                    let path = crypto::vault_file_path(&route_config_dir);
                    match std::fs::read(&path) {
                        Ok(bytes) => match Vault::unlock(&passphrase.0, &bytes) {
                            Ok(vault) => {
                                *route_queue.lock().await =
                                    Some(Queue::new(&route_config_dir, Arc::new(vault)));
                                let _ = route_msg_tx.send(AppMsg::VaultReady).await;
                                // unlocked: a prior backlog is now readable
                                let _ = route_kick_tx.try_send(());
                            }
                            Err(e) => {
                                let _ = route_msg_tx
                                    .send(AppMsg::VaultFailed(e.to_string()))
                                    .await;
                            }
                        },
                        Err(_) => {
                            let _ = route_msg_tx
                                .send(AppMsg::VaultFailed(
                                    "no vault to unlock; use /passphrase to create one".into(),
                                ))
                                .await;
                        }
                    }
                }
                TransportCmd::ClearQueue => {
                    match crate::messaging::queue::clear(&route_config_dir) {
                        Ok(count) => {
                            let _ = route_msg_tx.send(AppMsg::QueueCleared { count }).await;
                        }
                        Err(e) => {
                            let _ = route_msg_tx
                                .send(AppMsg::Log(format!("clear queue failed: {e}")))
                                .await;
                        }
                    }
                }
                TransportCmd::SyncContacts(new_routes) => {
                    *route_routes.lock().await = new_routes;
                    // a new route may match an existing backlog
                    let _ = route_kick_tx.try_send(());
                }
                TransportCmd::SendMessage { remote_static, id, text } => {
                    let sender = {
                        let guard = route_connections.lock().await;
                        guard.get(&remote_static).cloned()
                    };
                    match sender {
                        Some(tx) => {
                            if tx.send(Frame::Msg { id, text }).await.is_ok() {
                                let _ = route_msg_tx
                                    .send(AppMsg::DeliveryUpdate {
                                        id,
                                        status: DeliveryStatus::Sent,
                                    })
                                    .await;
                            } else {
                                let _ = route_msg_tx
                                    .send(AppMsg::DeliveryUpdate {
                                        id,
                                        status: DeliveryStatus::Failed,
                                    })
                                    .await;
                                let _ = route_msg_tx
                                    .send(AppMsg::Log("send failed: connection closed".into()))
                                    .await;
                            }
                        }
                        None => {
                            // recipient offline: queue the message if a vault is ready
                            let hex = hex32(&remote_static);
                            let outcome = {
                                let guard = route_queue.lock().await;
                                guard
                                    .as_ref()
                                    .map(|q| q.enqueue(&hex, QueuedMessage { id, text }))
                            };
                            match outcome {
                                Some(Ok(())) => {
                                    let _ = route_msg_tx
                                        .send(AppMsg::DeliveryUpdate {
                                            id,
                                            status: DeliveryStatus::Queued,
                                        })
                                        .await;
                                    let _ = route_msg_tx
                                        .send(AppMsg::Log(
                                            "recipient offline: queued, will deliver on reconnect"
                                                .into(),
                                        ))
                                        .await;
                                    // freshly queued: poll now, not next window
                                    let _ = route_kick_tx.try_send(());
                                }
                                Some(Err(e)) => {
                                    let _ = route_msg_tx
                                        .send(AppMsg::DeliveryUpdate {
                                            id,
                                            status: DeliveryStatus::Failed,
                                        })
                                        .await;
                                    let _ = route_msg_tx
                                        .send(AppMsg::Log(format!("queue write failed: {e}")))
                                        .await;
                                }
                                None => {
                                    let _ = route_msg_tx
                                        .send(AppMsg::DeliveryUpdate {
                                            id,
                                            status: DeliveryStatus::Failed,
                                        })
                                        .await;
                                    let hint = if crypto::vault_file_path(&route_config_dir).exists()
                                    {
                                        "recipient offline and queue locked: /unlock first, then resend"
                                    } else {
                                        "recipient offline: set a passphrase with /passphrase to enable the offline queue, then resend"
                                    };
                                    let _ = route_msg_tx.send(AppMsg::Log(hint.into())).await;
                                }
                            }
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
    queue: SharedQueue,
) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((sock, _addr)) => {
                    let static_key = static_key.clone();
                    let msg_tx = msg_tx.clone();
                    let connections = connections.clone();
                    let queue = queue.clone();
                    tokio::spawn(async move {
                        handshake_as_responder_tcp(
                            sock,
                            static_key,
                            own_id,
                            msg_tx,
                            connections,
                            queue,
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
    queue: SharedQueue,
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
                            let queue = queue.clone();
                            tokio::spawn(async move {
                                handshake_as_initiator_tcp(
                                    addr,
                                    static_key,
                                    own_id,
                                    peer_id,
                                    msg_tx,
                                    connections,
                                    queue,
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

#[allow(clippy::too_many_arguments)]
fn spawn_onion(
    config_dir: PathBuf,
    msg_tx: mpsc::Sender<AppMsg>,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    mut connect_rx: mpsc::Receiver<String>,
    connections: Connections,
    queue: SharedQueue,
    routes: SharedRoutes,
    retry_kick_rx: mpsc::Receiver<()>,
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

        // Tor is up: the retry loop can dial onions now.
        retry::spawn(
            tor_client.clone(),
            routes,
            retry_kick_rx,
            static_key.clone(),
            own_id,
            msg_tx.clone(),
            connections.clone(),
            queue.clone(),
        );

        let mut stream_requests = Box::pin(handle_rend_requests(rend_requests));

        loop {
            tokio::select! {
                // inbound: someone reached our onion
                Some(req) = stream_requests.next() => {
                    let static_key = static_key.clone();
                    let msg_tx = msg_tx.clone();
                    let connections = connections.clone();
                    let queue = queue.clone();
                    tokio::spawn(async move {
                        accept_onion_stream(req, static_key, own_id, msg_tx, connections, queue).await;
                    });
                }
                // outbound: user typed /connect
                Some(addr_str) = connect_rx.recv() => {
                    let client = tor_client.clone();
                    let static_key = static_key.clone();
                    let msg_tx = msg_tx.clone();
                    let connections = connections.clone();
                    let queue = queue.clone();
                    tokio::spawn(async move {
                        connect_onion_peer(client, addr_str, static_key, own_id, msg_tx, connections, queue, false).await;
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
    queue: SharedQueue,
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

    handle_handshake_result(
        result,
        Role::Initiator,
        Some(expected_peer_id),
        msg_tx,
        connections,
        queue,
    )
    .await;
}

async fn handshake_as_responder_tcp(
    sock: TcpStream,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
    queue: SharedQueue,
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

    handle_handshake_result(result, Role::Responder, None, msg_tx, connections, queue).await;
}

async fn accept_onion_stream(
    req: StreamRequest,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
    queue: SharedQueue,
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

    handle_handshake_result(result, Role::Responder, None, msg_tx, connections, queue).await;
}

/// Dial a peer's onion and handshake as initiator. `quiet` (the retry loop)
/// suppresses the connecting log and failure reporting; only success surfaces.
#[allow(clippy::too_many_arguments)]
async fn connect_onion_peer(
    client: TorClient<PreferredRuntime>,
    onion_addr: String,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
    queue: SharedQueue,
    quiet: bool,
) {
    let trimmed = onion_addr.trim().to_string();
    if !quiet {
        let _ = msg_tx
            .send(AppMsg::Log(format!("onion: connecting to {trimmed}…")))
            .await;
    }
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

    if quiet {
        if let Ok((stream, peer_id, sas, remote_static)) = result {
            spawn_established_connection(
                stream,
                peer_id,
                sas,
                remote_static,
                Role::Initiator,
                msg_tx,
                connections,
                queue,
            )
            .await;
        }
    } else {
        handle_handshake_result(result, Role::Initiator, None, msg_tx, connections, queue).await;
    }
}

async fn handle_handshake_result<S>(
    result: Result<(NoiseStream<S>, PeerId, String, [u8; 32]), noise::NoiseError>,
    role: Role,
    expected_peer_id: Option<PeerId>,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
    queue: SharedQueue,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match result {
        Ok((stream, peer_id, sas, remote_static)) => {
            spawn_established_connection(
                stream,
                peer_id,
                sas,
                remote_static,
                role,
                msg_tx,
                connections,
                queue,
            )
            .await;
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

/// Register a completed handshake and start its connection task. Shared by
/// every success path (LAN, onion accept, `/connect`, retry).
#[allow(clippy::too_many_arguments)]
async fn spawn_established_connection<S>(
    stream: NoiseStream<S>,
    peer_id: PeerId,
    sas: String,
    remote_static: [u8; 32],
    role: Role,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
    queue: SharedQueue,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (send_tx, send_rx) = mpsc::channel::<Frame>(CONNECTION_QUEUE);
    connections.lock().await.insert(remote_static, send_tx.clone());
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
        send_tx,
        send_rx,
        msg_tx,
        connections,
        queue,
    ));
}

async fn run_connection<S>(
    stream: NoiseStream<S>,
    peer_id: PeerId,
    remote_static: [u8; 32],
    ack_tx: mpsc::Sender<Frame>,
    mut send_rx: mpsc::Receiver<Frame>,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
    queue: SharedQueue,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Deliver anything queued for this peer while they were offline. Runs as
    // its own task so the writer loop below drains the channel concurrently; a
    // backlog larger than the channel buffer would otherwise deadlock.
    {
        let flush_ack = ack_tx.clone();
        let flush_queue = queue.clone();
        let flush_msg_tx = msg_tx.clone();
        tokio::spawn(async move {
            let hex = hex32(&remote_static);
            let pending = {
                let guard = flush_queue.lock().await;
                guard.as_ref().and_then(|q| q.load(&hex).ok())
            };
            if let Some(msgs) = pending {
                for m in msgs {
                    if flush_ack
                        .send(Frame::Msg {
                            id: m.id,
                            text: m.text,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                    let _ = flush_msg_tx
                        .send(AppMsg::DeliveryUpdate {
                            id: m.id,
                            status: DeliveryStatus::Sent,
                        })
                        .await;
                }
            }
        });
    }

    let (mut reader, mut writer) = stream.split();

    let read_msg_tx = msg_tx.clone();
    let ack_queue = queue.clone();
    let mut read_task = tokio::spawn(async move {
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
                    Ok(Frame::Msg { id, text }) => {
                        // Acknowledge receipt so the sender can mark it
                        // delivered and drop its queued copy, then surface it.
                        let _ = ack_tx.send(Frame::Ack(id)).await;
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
                    Ok(Frame::Ack(id)) => {
                        // The peer has it: drop our queued copy.
                        {
                            let hex = hex32(&remote_static);
                            let guard = ack_queue.lock().await;
                            if let Some(q) = guard.as_ref() {
                                let _ = q.remove(&hex, id);
                            }
                        }
                        if read_msg_tx
                            .send(AppMsg::DeliveryUpdate {
                                id,
                                status: DeliveryStatus::Delivered,
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

    // Pump outgoing frames, but also stop the instant the read task ends. The
    // read task ends when the peer closes its side, so this tears the
    // connection down promptly instead of waiting for a future write to fail.
    // Without it the registry keeps a dead entry and the sender keeps routing
    // new messages to a connection that is already gone.
    loop {
        tokio::select! {
            maybe_frame = send_rx.recv() => match maybe_frame {
                Some(frame) => {
                    if let Err(e) = writer.send(&frame.encode()).await {
                        let _ = msg_tx.send(AppMsg::Log(format!("send: {e}"))).await;
                        break;
                    }
                }
                None => break,
            },
            _ = &mut read_task => break,
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

fn hex32(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(&mut s, "{b:02x}");
    }
    s
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::StaticKey;
    use std::time::Duration;
    use tokio::time::timeout;

    // When the peer closes its side, run_connection must tear the connection
    // down promptly (drop it from the registry, emit PeerDisconnected) even
    // when no outgoing write is in flight. Otherwise the sender keeps treating
    // the dead peer as connected and a later message is lost instead of queued.
    #[tokio::test]
    async fn dropped_peer_tears_down_connection_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key_a = StaticKey::generate().unwrap();
        let key_b = StaticKey::generate().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            noise::handshake_responder(sock, &key_b).await.unwrap()
        });
        let client_sock = TcpStream::connect(addr).await.unwrap();
        let client = noise::handshake_initiator(client_sock, &key_a).await.unwrap();
        let peer_stream = server.await.unwrap();

        let (msg_tx, mut msg_rx) = mpsc::channel(16);
        let (send_tx, send_rx) = mpsc::channel(8);
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let queue: SharedQueue = Arc::new(Mutex::new(None));
        let remote_static = [7u8; 32];
        connections
            .lock()
            .await
            .insert(remote_static, send_tx.clone());

        tokio::spawn(run_connection(
            client,
            PeerId::generate(),
            remote_static,
            send_tx.clone(),
            send_rx,
            msg_tx,
            connections.clone(),
            queue,
        ));

        // Hold a sender open so the connection can only end via the read side.
        let _keep_alive = send_tx;

        // The peer goes away.
        drop(peer_stream);

        let torn_down = timeout(Duration::from_secs(2), async {
            while let Some(msg) = msg_rx.recv().await {
                if matches!(msg, AppMsg::PeerDisconnected { .. }) {
                    return true;
                }
            }
            false
        })
        .await;

        assert!(
            matches!(torn_down, Ok(true)),
            "peer dropped but no PeerDisconnected was emitted: teardown was not prompt"
        );
        assert!(
            connections.lock().await.is_empty(),
            "dead connection was not removed from the registry"
        );
    }

    #[tokio::test]
    async fn both_sides_flush_their_queue_on_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key_a = StaticKey::generate().unwrap();
        let key_b = StaticKey::generate().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            noise::handshake_responder(sock, &key_b).await.unwrap()
        });
        let client_sock = TcpStream::connect(addr).await.unwrap();
        let stream_a = noise::handshake_initiator(client_sock, &key_a).await.unwrap();
        let stream_b = server.await.unwrap();

        // synthetic remote keys: run_connection takes remote_static explicitly
        let id_a = [1u8; 32];
        let id_b = [2u8; 32];

        let queue_a = make_queue();
        let queue_b = make_queue();
        queue_a
            .lock()
            .await
            .as_ref()
            .unwrap()
            .enqueue(&hex32(&id_b), QueuedMessage { id: 10, text: "a to b".into() })
            .unwrap();
        queue_b
            .lock()
            .await
            .as_ref()
            .unwrap()
            .enqueue(&hex32(&id_a), QueuedMessage { id: 20, text: "b to a".into() })
            .unwrap();

        let (msg_tx_a, msg_rx_a) = mpsc::channel(32);
        let (msg_tx_b, msg_rx_b) = mpsc::channel(32);
        let (send_tx_a, send_rx_a) = mpsc::channel(8);
        let (send_tx_b, send_rx_b) = mpsc::channel(8);
        let conns_a: Connections = Arc::new(Mutex::new(HashMap::new()));
        let conns_b: Connections = Arc::new(Mutex::new(HashMap::new()));

        tokio::spawn(run_connection(
            stream_a,
            PeerId::generate(),
            id_b,
            send_tx_a.clone(),
            send_rx_a,
            msg_tx_a,
            conns_a,
            queue_a.clone(),
        ));
        tokio::spawn(run_connection(
            stream_b,
            PeerId::generate(),
            id_a,
            send_tx_b.clone(),
            send_rx_b,
            msg_tx_b,
            conns_b,
            queue_b.clone(),
        ));

        let (recv_a, recv_b) = timeout(
            Duration::from_secs(5),
            futures::future::join(collect_drain(msg_rx_a), collect_drain(msg_rx_b)),
        )
        .await
        .expect("drain did not complete in time");

        assert_eq!(recv_a, (Some("b to a".to_string()), Some(10)));
        assert_eq!(recv_b, (Some("a to b".to_string()), Some(20)));
        assert!(
            queue_a.lock().await.as_ref().unwrap().load(&hex32(&id_b)).unwrap().is_empty(),
            "A's queue still holds its message after the ack"
        );
        assert!(
            queue_b.lock().await.as_ref().unwrap().load(&hex32(&id_a)).unwrap().is_empty(),
            "B's queue still holds its message after the ack"
        );

        drop((send_tx_a, send_tx_b));
    }

    fn make_queue() -> SharedQueue {
        let dir = std::env::temp_dir().join(format!("cord-rt-{:x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let vault = Arc::new(Vault::create("test passphrase").unwrap().0);
        Arc::new(Mutex::new(Some(Queue::new(&dir, vault))))
    }

    async fn collect_drain(mut rx: mpsc::Receiver<AppMsg>) -> (Option<String>, Option<u64>) {
        let mut incoming = None;
        let mut delivered = None;
        while incoming.is_none() || delivered.is_none() {
            match rx.recv().await {
                Some(AppMsg::MessageReceived { text, .. }) => incoming = Some(text),
                Some(AppMsg::DeliveryUpdate { id, status: DeliveryStatus::Delivered }) => {
                    delivered = Some(id)
                }
                Some(_) => {}
                None => break,
            }
        }
        (incoming, delivered)
    }
}
