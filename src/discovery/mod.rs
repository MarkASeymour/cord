use std::net::SocketAddr;

use crate::identity::PeerId;

pub mod mdns;

#[derive(Debug, Clone)]
pub struct KnownPeer {
    pub addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub enum PeerEvent {
    Discovered { peer_id: PeerId, addr: SocketAddr },
    Lost(PeerId),
}
