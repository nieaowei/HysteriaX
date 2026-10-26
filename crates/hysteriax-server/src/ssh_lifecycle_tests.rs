use super::*;
use std::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context as TaskContext, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};

struct CountedIo {
    io: tokio::io::DuplexStream,
    reads: Arc<AtomicUsize>,
}

impl AsyncRead for CountedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for CountedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

fn handler() -> HostKeyHandler {
    HostKeyHandler {
        expected: None,
        observed: Arc::new(Mutex::new(None)),
    }
}

#[tokio::test]
async fn keepalive_before_key_exchange_does_not_spin_or_extend_inactivity() {
    let (client_io, mut peer) = tokio::io::duplex(65536);
    peer.write_all(b"SSH-2.0-stalled-fixture\r\n")
        .await
        .unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let config = client::Config {
        keepalive_interval: Some(Duration::from_millis(10)),
        inactivity_timeout: Some(Duration::from_millis(120)),
        ..Default::default()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        client::connect_stream(
            Arc::new(config),
            CountedIo {
                io: client_io,
                reads: reads.clone(),
            },
            handler(),
        ),
    )
    .await
    .expect("keepalive ticks must not postpone the inactivity deadline");
    assert!(matches!(result, Err(russh::Error::InactivityTimeout)));
    assert!(
        reads.load(Ordering::Relaxed) < 100,
        "idle handshake was repeatedly polled"
    );
}

#[tokio::test]
async fn cancelling_key_exchange_closes_the_spawned_session_transport() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"SSH-2.0-stalled-fixture\r\n")
            .await
            .unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).await.unwrap();
        assert!(
            request.len() > 50,
            "the client must reach key exchange before cancellation"
        );
    });
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_millis(10)),
        // Cancellation must close the transport before this timeout elapses.
        inactivity_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            connect_transport(config, ("127.0.0.1", address.port()), handler()),
        )
        .await
        .is_err()
    );
    tokio::time::timeout(Duration::from_secs(1), peer)
        .await
        .expect("cancelled handshake retained its socket")
        .unwrap();
}

struct PasswordServer;
impl russh::server::Handler for PasswordServer {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        _: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, b"healthy".to_vec())?;
        session.exit_status_request(channel, 0)?;
        session.close(channel)?;
        Ok(())
    }
}

#[tokio::test]
async fn authenticated_session_keeps_working_and_closes_on_drop() {
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[19; 32]).into(),
        "fixture",
    )
    .unwrap();
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let server_config = Arc::new(russh::server::Config {
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        russh::server::run_stream(server_config, stream, PasswordServer)
            .await
            .unwrap()
            .await
    });
    let mut handler = handler();
    handler.expected = Some(fingerprint.clone());
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_millis(30)),
        inactivity_timeout: Some(Duration::from_millis(300)),
        ..Default::default()
    });
    let (mut handle, socket) = connect_transport(config, ("127.0.0.1", address.port()), handler)
        .await
        .unwrap();
    assert!(
        handle
            .authenticate_password("fixture", "fixture")
            .await
            .unwrap()
            .success()
    );
    let session = SshSession {
        handle,
        _socket: Some(socket),
        endpoint: journal::Endpoint {
            host: "127.0.0.1".into(),
            port: address.port(),
            username: "fixture".into(),
            fingerprint: fingerprint.clone(),
        },
        fingerprint,
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    let output = session
        .execute_remote("observe", Duration::from_secs(1), 1024)
        .await
        .unwrap();
    assert_eq!(output.code, 0);
    assert_eq!(output.stdout, "healthy");
    drop(session);
    tokio::time::timeout(Duration::from_secs(1), peer)
        .await
        .expect("released session retained its transport")
        .unwrap()
        .ok();
}
