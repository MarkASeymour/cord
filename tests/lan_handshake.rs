use tokio::net::{TcpListener, TcpStream};

use cord::identity::PeerId;
use cord::noise::{self, StaticKey};

#[tokio::test]
async fn two_lan_peers_handshake_over_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let key_a = StaticKey::generate().unwrap();
    let key_b = StaticKey::generate().unwrap();
    let id_a = PeerId::generate();
    let id_b = PeerId::generate();

    let responder = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut stream = noise::handshake_responder(sock, &key_b).await.unwrap();
        let bytes = stream.recv().await.unwrap();
        let mut id_bytes = [0u8; PeerId::BYTE_LEN];
        id_bytes.copy_from_slice(&bytes);
        stream.send(id_b.as_bytes()).await.unwrap();
        PeerId::from_bytes(id_bytes)
    });

    let initiator = tokio::spawn(async move {
        let sock = TcpStream::connect(addr).await.unwrap();
        let mut stream = noise::handshake_initiator(sock, &key_a).await.unwrap();
        stream.send(id_a.as_bytes()).await.unwrap();
        let bytes = stream.recv().await.unwrap();
        let mut id_bytes = [0u8; PeerId::BYTE_LEN];
        id_bytes.copy_from_slice(&bytes);
        PeerId::from_bytes(id_bytes)
    });

    let responder_saw = responder.await.unwrap();
    let initiator_saw = initiator.await.unwrap();

    assert_eq!(responder_saw, id_a, "responder learned initiator peer-id");
    assert_eq!(initiator_saw, id_b, "initiator learned responder peer-id");
}
