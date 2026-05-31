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
                    let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                    let Some(addr) = dialable_addr(&addrs, port) else {
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

/// Pick an address we can actually dial on the LAN: IPv4 only. mDNS also
/// surfaces IPv6, but a link local fe80:: address has no usable scope id from
/// mdns-sd and a global v6 address is rarely routable across a home LAN, so
/// dialing either just fails the handshake. mDNS resolves addresses
/// incrementally, so early events may carry only IPv6; returning None then lets
/// the loop wait for a later event that includes the v4 address. A peer
/// reachable only over IPv6 is reached through the onion path instead.
fn dialable_addr(addresses: &[IpAddr], port: u16) -> Option<SocketAddr> {
    addresses.iter().find_map(|ip| match ip {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(SocketAddr::new(IpAddr::V4(*v4), port)),
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn prefers_ipv4_over_ipv6() {
        let v6_link_local: IpAddr = "fe80::591a:a4cc:5d64:433e".parse().unwrap();
        let v6_global: IpAddr = "2600:1702:75d0:692f::1".parse().unwrap();
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 86, 220));
        // v6 addresses listed first, but the v4 one must win
        let picked = dialable_addr(&[v6_link_local, v6_global, v4], 59142).unwrap();
        assert_eq!(picked, SocketAddr::new(v4, 59142));
    }

    #[test]
    fn skips_ipv6_only_so_the_loop_waits_for_v4() {
        let v6_link_local: IpAddr = "fe80::1".parse().unwrap();
        let v6_global: IpAddr = "2600::1".parse().unwrap();
        assert!(dialable_addr(&[v6_link_local, v6_global], 59142).is_none());
    }

    #[test]
    fn skips_loopback_v4() {
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(dialable_addr(&[lo], 59142).is_none());
    }
}
