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

    let key_a_pub = *key_a.public_bytes();
    let key_b_pub = *key_b.public_bytes();

    let responder = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut stream = noise::handshake_responder(sock, &key_b).await.unwrap();
        let bytes = stream.recv().await.unwrap();
        let mut id_bytes = [0u8; PeerId::BYTE_LEN];
        id_bytes.copy_from_slice(&bytes);
        stream.send(id_b.as_bytes()).await.unwrap();
        let sas = noise::derive_sas(stream.handshake_hash());
        let mut remote = [0u8; 32];
        remote.copy_from_slice(stream.remote_static().unwrap());
        (PeerId::from_bytes(id_bytes), sas, remote)
    });

    let initiator = tokio::spawn(async move {
        let sock = TcpStream::connect(addr).await.unwrap();
        let mut stream = noise::handshake_initiator(sock, &key_a).await.unwrap();
        stream.send(id_a.as_bytes()).await.unwrap();
        let bytes = stream.recv().await.unwrap();
        let mut id_bytes = [0u8; PeerId::BYTE_LEN];
        id_bytes.copy_from_slice(&bytes);
        let sas = noise::derive_sas(stream.handshake_hash());
        let mut remote = [0u8; 32];
        remote.copy_from_slice(stream.remote_static().unwrap());
        (PeerId::from_bytes(id_bytes), sas, remote)
    });

    let (responder_id, responder_sas, responder_saw_remote) = responder.await.unwrap();
    let (initiator_id, initiator_sas, initiator_saw_remote) = initiator.await.unwrap();

    assert_eq!(responder_id, id_a, "responder learned initiator peer-id");
    assert_eq!(initiator_id, id_b, "initiator learned responder peer-id");
    assert_eq!(
        responder_sas, initiator_sas,
        "both sides must derive the same SAS"
    );
    assert_eq!(
        responder_saw_remote, key_a_pub,
        "responder's remote_static must equal initiator's public_bytes"
    );
    assert_eq!(
        initiator_saw_remote, key_b_pub,
        "initiator's remote_static must equal responder's public_bytes"
    );
}
