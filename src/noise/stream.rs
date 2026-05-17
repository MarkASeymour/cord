use std::sync::Arc;

use snow::TransportState;
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio::sync::Mutex;

use super::{read_frame, write_frame, NoiseError};

const MAX_MSG: usize = 65535;

pub struct NoiseStream<S> {
    transport: S,
    state: TransportState,
    handshake_hash: Vec<u8>,
}

pub struct NoiseReader<R> {
    reader: R,
    state: Arc<Mutex<TransportState>>,
}

pub struct NoiseWriter<W> {
    writer: W,
    state: Arc<Mutex<TransportState>>,
}

impl<S> NoiseStream<S> {
    pub(super) fn new(transport: S, state: TransportState, handshake_hash: Vec<u8>) -> Self {
        Self {
            transport,
            state,
            handshake_hash,
        }
    }

    pub fn remote_static(&self) -> Option<&[u8]> {
        self.state.get_remote_static()
    }

    pub fn handshake_hash(&self) -> &[u8] {
        &self.handshake_hash
    }
}

impl<S> NoiseStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
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

    pub fn split(self) -> (NoiseReader<ReadHalf<S>>, NoiseWriter<WriteHalf<S>>) {
        let (reader, writer) = tokio::io::split(self.transport);
        let state = Arc::new(Mutex::new(self.state));
        (
            NoiseReader {
                reader,
                state: state.clone(),
            },
            NoiseWriter { writer, state },
        )
    }
}

impl<R> NoiseReader<R>
where
    R: AsyncRead + Unpin + Send,
{
    pub async fn recv(&mut self) -> Result<Vec<u8>, NoiseError> {
        let frame = read_frame(&mut self.reader).await?;
        let mut buf = vec![0u8; MAX_MSG];
        let n = {
            let mut state = self.state.lock().await;
            state.read_message(&frame, &mut buf)?
        };
        buf.truncate(n);
        Ok(buf)
    }
}

impl<W> NoiseWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    pub async fn send(&mut self, plaintext: &[u8]) -> Result<(), NoiseError> {
        let mut buf = vec![0u8; MAX_MSG];
        let n = {
            let mut state = self.state.lock().await;
            state.write_message(plaintext, &mut buf)?
        };
        write_frame(&mut self.writer, &buf[..n]).await
    }
}
