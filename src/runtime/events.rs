use std::net::SocketAddr;

use crate::identity::PeerId;

#[derive(Debug)]
pub enum AppMsg {
    Log(String),
    TransportReady(LocalAddrs),
    OnionReady { onion_name: String, hs_id: [u8; 32] },
    OnionFailed(String),
    PeerDiscovered { peer_id: PeerId, addr: SocketAddr },
    PeerLost(PeerId),
    HandshakeOk {
        peer_id: PeerId,
        role: Role,
        sas: String,
        remote_static: [u8; 32],
    },
    HandshakeFailed {
        peer_id: Option<PeerId>,
        role: Role,
        error: String,
    },
    MessageReceived {
        from_peer_id: PeerId,
        remote_static: [u8; 32],
        text: String,
    },
    PeerDisconnected {
        peer_id: PeerId,
        remote_static: [u8; 32],
    },
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
    SendMessage { remote_static: [u8; 32], text: String },
}

#[derive(Debug, Clone, Copy)]
pub struct LocalAddrs {
    pub lan: SocketAddr,
}
