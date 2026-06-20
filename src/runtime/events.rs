use std::net::SocketAddr;

use crate::identity::PeerId;

#[derive(Debug)]
pub enum AppMsg {
    Log(String),
    TransportReady(LocalAddrs),
    OnionReady { onion_name: String, hs_id: [u8; 32] },
    OnionFailed(String),
    TorProgress { percent: u8, summary: String },
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
    DeliveryUpdate {
        id: u64,
        status: DeliveryStatus,
    },
    /// A queue vault file exists on disk; the user must unlock it to resume.
    VaultLocked,
    /// The vault is set up or unlocked; the offline queue is now usable.
    VaultReady,
    /// Vault setup or unlock failed (wrong passphrase, i/o, etc.).
    VaultFailed(String),
    /// The message queue was cleared; `count` per contact queues were removed.
    QueueCleared { count: usize },
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

/// How far an outgoing message has progressed. The TUI renders this next to
/// each message you send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Sending,
    Queued,
    Sent,
    Delivered,
    Failed,
    Dropped,
}

impl DeliveryStatus {
    pub fn marker(self) -> &'static str {
        match self {
            DeliveryStatus::Sending => "sending…",
            DeliveryStatus::Queued => "queued",
            DeliveryStatus::Sent => "sent",
            DeliveryStatus::Delivered => "delivered ✓",
            DeliveryStatus::Failed => "failed ✗",
            DeliveryStatus::Dropped => "dropped",
        }
    }
}

/// A user passphrase. Debug is redacted so it never lands in a log line.
#[derive(Clone)]
pub struct Passphrase(pub String);

impl std::fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Passphrase(<redacted>)")
    }
}

#[derive(Debug)]
pub enum TransportCmd {
    Shutdown,
    ConnectOnion(String),
    SendMessage { remote_static: [u8; 32], id: u64, text: String },
    /// Create a brand new queue vault from this passphrase (first time setup).
    SetupVault(Passphrase),
    /// Unlock an existing queue vault.
    UnlockVault(Passphrase),
    /// Delete every queued (undelivered) message. Does not need the vault.
    ClearQueue,
    /// Replace the verified contacts the retry loop may dial. Sent by the TUI
    /// at startup and whenever the contact list changes.
    SyncContacts(Vec<ContactRoute>),
}

/// A verified contact the retry loop can reach: its Noise static key and the raw
/// v3 onion key bytes (kept raw so the TUI stays free of arti types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactRoute {
    pub remote_static: [u8; 32],
    pub hs_id: [u8; 32],
    pub label: String,
}

#[derive(Debug, Clone, Copy)]
pub struct LocalAddrs {
    pub lan: SocketAddr,
}
