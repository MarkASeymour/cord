use tokio::net::{TcpListener, TcpStream};

use cord::messaging::Frame;
use cord::noise::{self, StaticKey};

#[tokio::test]
async fn split_stream_sends_and_receives_a_text_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let key_a = StaticKey::generate().unwrap();
    let key_b = StaticKey::generate().unwrap();

    let responder = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let stream = noise::handshake_responder(sock, &key_b).await.unwrap();
        let (mut reader, mut writer) = stream.split();
        let bytes = reader.recv().await.unwrap();
        let frame = Frame::decode(&bytes).unwrap();
        let reply = match frame {
            Frame::Text(t) => Frame::Text(format!("echo: {t}")),
            other => other,
        };
        writer.send(&reply.encode()).await.unwrap();
    });

    let initiator = tokio::spawn(async move {
        let sock = TcpStream::connect(addr).await.unwrap();
        let stream = noise::handshake_initiator(sock, &key_a).await.unwrap();
        let (mut reader, mut writer) = stream.split();
        writer.send(&Frame::Text("hello".into()).encode()).await.unwrap();
        let bytes = reader.recv().await.unwrap();
        Frame::decode(&bytes).unwrap()
    });

    let initiator_got = initiator.await.unwrap();
    responder.await.unwrap();

    assert_eq!(initiator_got, Frame::Text("echo: hello".into()));
}
