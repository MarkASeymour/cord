use std::net::{IpAddr, SocketAddr};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use crate::discovery::PeerEvent;
use crate::identity::PeerId;
use crate::transport::TransportError;

pub const SERVICE_TYPE: &str = "_cord._tcp.local.";

pub struct MdnsHandle {
    daemon: ServiceDaemon,
    full_name: String,
}

impl MdnsHandle {
    pub fn shutdown(self) {
        let _ = self.daemon.unregister(&self.full_name);
        let _ = self.daemon.shutdown();
    }
}

pub fn start(
    peer_id: PeerId,
    port: u16,
    event_tx: mpsc::Sender<PeerEvent>,
) -> Result<MdnsHandle, TransportError> {
    let daemon = ServiceDaemon::new()?;
    let instance_name = peer_id.to_string();
    let host_name = format!("{instance_name}.local.");
    let properties = [("peer_id", instance_name.as_str())];

    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &host_name,
        "",
        port,
        &properties[..],
    )?
    .enable_addr_auto();
    let full_name = info.get_fullname().to_string();
    daemon.register(info)?;

    let receiver = daemon.browse(SERVICE_TYPE)?;
    let self_id = peer_id;
    let tx = event_tx;

    tokio::spawn(async move {
        loop {
            match receiver.recv_async().await {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    let Some(other_id) = peer_id_from_fullname(info.get_fullname()) else {
                        continue;
                    };
                    if other_id == self_id {
                        continue;
                    }
                    let port = info.get_port();
                    let addr = info
                        .get_addresses()
                        .iter()
                        .find_map(|ip| match ip {
                            IpAddr::V4(v4) if !v4.is_loopback() => {
                                Some(SocketAddr::new(IpAddr::V4(*v4), port))
                            }
                            _ => None,
                        })
                        .or_else(|| {
                            info.get_addresses()
                                .iter()
                                .next()
                                .map(|ip| SocketAddr::new(*ip, port))
                        });
                    let Some(addr) = addr else {
                        continue;
                    };
                    if tx
                        .send(PeerEvent::Discovered {
                            peer_id: other_id,
                            addr,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(ServiceEvent::ServiceRemoved(_, full_name)) => {
                    let Some(other_id) = peer_id_from_fullname(&full_name) else {
                        continue;
                    };
                    if other_id == self_id {
                        continue;
                    }
                    if tx.send(PeerEvent::Lost(other_id)).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    Ok(MdnsHandle {
        daemon,
        full_name,
    })
}

fn peer_id_from_fullname(full_name: &str) -> Option<PeerId> {
    let first_label = full_name.split('.').next()?;
    if first_label.len() != PeerId::HEX_LEN {
        return None;
    }
    let mut bytes = [0u8; PeerId::BYTE_LEN];
    for (i, b) in bytes.iter_mut().enumerate() {
        let hi = hex_nibble(first_label.as_bytes()[i * 2])?;
        let lo = hex_nibble(first_label.as_bytes()[i * 2 + 1])?;
        *b = (hi << 4) | lo;
    }
    Some(PeerId::from_bytes(bytes))
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
