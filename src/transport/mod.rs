use std::fmt;
use std::io;

pub mod lan;
pub mod onion;

#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    Mdns(mdns_sd::Error),
    Tor(Box<dyn std::error::Error + Send + Sync>),
    Onion(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "transport i/o: {e}"),
            TransportError::Mdns(e) => write!(f, "mdns: {e}"),
            TransportError::Tor(e) => write!(f, "tor: {e}"),
            TransportError::Onion(msg) => write!(f, "onion: {msg}"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Io(e) => Some(e),
            TransportError::Mdns(_) => None,
            TransportError::Tor(e) => Some(e.as_ref()),
            TransportError::Onion(_) => None,
        }
    }
}

impl From<arti_client::Error> for TransportError {
    fn from(e: arti_client::Error) -> Self {
        TransportError::Tor(Box::new(e))
    }
}

impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<mdns_sd::Error> for TransportError {
    fn from(e: mdns_sd::Error) -> Self {
        TransportError::Mdns(e)
    }
}
