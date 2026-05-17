use std::io;

use crate::identity::IdentityError;
use crate::transport::TransportError;

#[derive(Debug)]
pub enum CordError {
    Io(io::Error),
    Identity(IdentityError),
    Transport(TransportError),
}

impl std::fmt::Display for CordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CordError::Io(e) => write!(f, "io: {e}"),
            CordError::Identity(e) => write!(f, "identity: {e}"),
            CordError::Transport(e) => write!(f, "transport: {e}"),
        }
    }
}

impl std::error::Error for CordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CordError::Io(e) => Some(e),
            CordError::Identity(e) => Some(e),
            CordError::Transport(e) => Some(e),
        }
    }
}

impl From<io::Error> for CordError {
    fn from(e: io::Error) -> Self {
        CordError::Io(e)
    }
}

impl From<IdentityError> for CordError {
    fn from(e: IdentityError) -> Self {
        CordError::Identity(e)
    }
}

impl From<TransportError> for CordError {
    fn from(e: TransportError) -> Self {
        CordError::Transport(e)
    }
}
