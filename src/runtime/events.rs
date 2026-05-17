use std::net::SocketAddr;

use crate::identity::PeerId;

#[derive(Debug)]
pub enum AppMsg {
    Log(String),
    TransportReady(LocalAddrs),
    OnionReady { onion_name: String },
    OnionFailed(String),
    PeerDiscovered { peer_id: PeerId, addr: SocketAddr },
    PeerLost(PeerId),
    HandshakeOk { peer_id: PeerId, role: Role },
    HandshakeFailed { peer_id: Option<PeerId>, role: Role, error: String },
}

#[derive(Debug, Clone, Copy)]
pub enum Role {
    Initiator,
    Responder,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Initiator => "initiator",
            Role::Responder => "responder",
        }
    }
}

#[derive(Debug)]
pub enum TransportCmd {
    Shutdown,
    ConnectOnion(String),
}

#[derive(Debug, Clone, Copy)]
pub struct LocalAddrs {
    pub lan: SocketAddr,
}
