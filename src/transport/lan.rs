use std::net::SocketAddr;

use tokio::net::TcpListener;

pub struct LanTransport {
    pub listener: TcpListener,
}

impl LanTransport {
    pub async fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind("0.0.0.0:0").await?;
        Ok(Self { listener })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}
