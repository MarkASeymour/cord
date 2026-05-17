use snow::TransportState;
use tokio::io::{AsyncRead, AsyncWrite};

use super::{read_frame, write_frame, NoiseError};

const MAX_MSG: usize = 65535;

pub struct NoiseStream<S> {
    transport: S,
    state: TransportState,
}

impl<S> NoiseStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    pub(super) fn new(transport: S, state: TransportState) -> Self {
        Self { transport, state }
    }

    pub async fn send(&mut self, plaintext: &[u8]) -> Result<(), NoiseError> {
        let mut buf = vec![0u8; MAX_MSG];
        let n = self.state.write_message(plaintext, &mut buf)?;
        write_frame(&mut self.transport, &buf[..n]).await
    }

    pub async fn recv(&mut self) -> Result<Vec<u8>, NoiseError> {
        let frame = read_frame(&mut self.transport).await?;
        let mut buf = vec![0u8; MAX_MSG];
        let n = self.state.read_message(&frame, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn remote_static(&self) -> Option<&[u8]> {
        self.state.get_remote_static()
    }
}
