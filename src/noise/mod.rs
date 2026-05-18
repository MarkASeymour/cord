use std::fmt;

use snow::Builder;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroize;

pub mod sas;
pub mod stream;

pub use sas::derive as derive_sas;
pub use stream::{NoiseReader, NoiseStream, NoiseWriter};

pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
pub const NOISE_STATIC_LEN: usize = 32;
const MAX_MSG: usize = 65535;

#[derive(Clone)]
pub struct StaticKey {
    private: Vec<u8>,
    public: [u8; NOISE_STATIC_LEN],
}

impl StaticKey {
    pub fn generate() -> Result<Self, NoiseError> {
        let builder = Builder::new(
            NOISE_PATTERN
                .parse()
                .map_err(|_| NoiseError::BadPattern)?,
        );
        let keypair = builder.generate_keypair().map_err(NoiseError::from)?;
        Self::from_private_bytes(keypair.private)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, NoiseError> {
        Self::from_private_bytes(bytes)
    }

    fn from_private_bytes(bytes: Vec<u8>) -> Result<Self, NoiseError> {
        if bytes.len() != NOISE_STATIC_LEN {
            return Err(NoiseError::BadPayload(format!(
                "noise static key must be {NOISE_STATIC_LEN} bytes, got {}",
                bytes.len()
            )));
        }
        let mut private_arr = [0u8; NOISE_STATIC_LEN];
        private_arr.copy_from_slice(&bytes);
        let secret = x25519_dalek::StaticSecret::from(private_arr);
        let public = *x25519_dalek::PublicKey::from(&secret).as_bytes();
        Ok(Self {
            private: bytes,
            public,
        })
    }

    pub fn private_bytes(&self) -> &[u8] {
        &self.private
    }

    pub fn public_bytes(&self) -> &[u8; NOISE_STATIC_LEN] {
        &self.public
    }
}

impl Drop for StaticKey {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}

impl fmt::Debug for StaticKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StaticKey(<redacted>)")
    }
}

#[derive(Debug)]
pub enum NoiseError {
    BadPattern,
    BadPayload(String),
    Io(std::io::Error),
    Snow(snow::Error),
    Truncated,
    FrameTooLarge(usize),
}

impl fmt::Display for NoiseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoiseError::BadPattern => write!(f, "noise pattern parse failed"),
            NoiseError::BadPayload(msg) => write!(f, "noise payload: {msg}"),
            NoiseError::Io(e) => write!(f, "noise i/o: {e}"),
            NoiseError::Snow(e) => write!(f, "noise: {e}"),
            NoiseError::Truncated => write!(f, "noise: truncated frame"),
            NoiseError::FrameTooLarge(n) => {
                write!(f, "noise: frame too large ({n} > {MAX_MSG})")
            }
        }
    }
}

impl std::error::Error for NoiseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NoiseError::Io(e) => Some(e),
            NoiseError::Snow(_) => None,
            _ => None,
        }
    }
}

impl From<std::io::Error> for NoiseError {
    fn from(e: std::io::Error) -> Self {
        NoiseError::Io(e)
    }
}

impl From<snow::Error> for NoiseError {
    fn from(e: snow::Error) -> Self {
        NoiseError::Snow(e)
    }
}

pub async fn handshake_initiator<S>(
    mut stream: S,
    local_static: &StaticKey,
) -> Result<NoiseStream<S>, NoiseError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let builder = Builder::new(NOISE_PATTERN.parse().map_err(|_| NoiseError::BadPattern)?);
    let mut state = builder
        .local_private_key(local_static.private_bytes())?
        .build_initiator()?;

    let mut buf = vec![0u8; MAX_MSG];

    // send e
    let n = state.write_message(&[], &mut buf)?;
    write_frame(&mut stream, &buf[..n]).await?;

    // receive e, ee, s, es
    let msg = read_frame(&mut stream).await?;
    state.read_message(&msg, &mut buf)?;

    // send s, se
    let n = state.write_message(&[], &mut buf)?;
    write_frame(&mut stream, &buf[..n]).await?;

    let handshake_hash = state.get_handshake_hash().to_vec();
    let transport = state.into_transport_mode()?;
    Ok(NoiseStream::new(stream, transport, handshake_hash))
}

pub async fn handshake_responder<S>(
    mut stream: S,
    local_static: &StaticKey,
) -> Result<NoiseStream<S>, NoiseError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let builder = Builder::new(NOISE_PATTERN.parse().map_err(|_| NoiseError::BadPattern)?);
    let mut state = builder
        .local_private_key(local_static.private_bytes())?
        .build_responder()?;

    let mut buf = vec![0u8; MAX_MSG];

    // receive e
    let msg = read_frame(&mut stream).await?;
    state.read_message(&msg, &mut buf)?;

    // send e, ee, s, es
    let n = state.write_message(&[], &mut buf)?;
    write_frame(&mut stream, &buf[..n]).await?;

    // receive s, se
    let msg = read_frame(&mut stream).await?;
    state.read_message(&msg, &mut buf)?;

    let handshake_hash = state.get_handshake_hash().to_vec();
    let transport = state.into_transport_mode()?;
    Ok(NoiseStream::new(stream, transport, handshake_hash))
}

pub(crate) async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, NoiseError>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 2];
    reader.read_exact(&mut len_buf).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            NoiseError::Truncated
        } else {
            NoiseError::Io(e)
        }
    })?;
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut data = vec![0u8; len];
    reader.read_exact(&mut data).await?;
    Ok(data)
}

pub(crate) async fn write_frame<W>(writer: &mut W, data: &[u8]) -> Result<(), NoiseError>
where
    W: AsyncWrite + Unpin,
{
    if data.len() > MAX_MSG {
        return Err(NoiseError::FrameTooLarge(data.len()));
    }
    let len = data.len() as u16;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn xx_handshake_completes_in_memory() {
        let key_a = StaticKey::generate().unwrap();
        let key_b = StaticKey::generate().unwrap();
        let (a, b) = duplex(8192);

        let init = tokio::spawn(async move { handshake_initiator(a, &key_a).await });
        let resp = tokio::spawn(async move { handshake_responder(b, &key_b).await });

        let mut a_stream = init.await.unwrap().unwrap();
        let mut b_stream = resp.await.unwrap().unwrap();

        a_stream.send(b"hello b").await.unwrap();
        let got = b_stream.recv().await.unwrap();
        assert_eq!(got, b"hello b");

        b_stream.send(b"hi a").await.unwrap();
        let got = a_stream.recv().await.unwrap();
        assert_eq!(got, b"hi a");
    }
}
