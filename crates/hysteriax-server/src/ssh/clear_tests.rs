use super::*;
use russh::{Channel, server};

struct StatsServer {
    requests: Arc<Mutex<Vec<String>>>,
}

impl server::Handler for StatsServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        mut channel: Channel<server::Msg>,
        _: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        let id = channel.id();
        let handle = session.handle();
        let requests = self.requests.clone();
        tokio::spawn(async move {
            if let Some(ChannelMsg::Data { data }) = channel.wait().await {
                requests
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&data).into_owned());
                if port == 3 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    return;
                }
                let response = if port == 1 {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".as_slice()
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{}".as_slice()
                };
                let _ = handle.data(id, response.to_vec()).await;
                let _ = handle.close(id).await;
            }
        });
        Ok(())
    }
}

#[tokio::test]
async fn clear_http_uses_direct_channel_without_retry_and_rejects_truncation() {
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[11; 32]).into(),
        "test",
    )
    .unwrap();
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let config = Arc::new(server::Config {
        keys: vec![key],
        ..Default::default()
    });
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server_requests = requests.clone();
    let server = tokio::spawn(async move {
        server::run_stream(
            config,
            server_io,
            StatsServer {
                requests: server_requests,
            },
        )
        .await
        .unwrap()
        .await
    });
    let handler = HostKeyHandler {
        expected: Some(fingerprint.clone()),
        observed: Arc::new(Mutex::new(None)),
    };
    let mut handle =
        client::connect_stream(Arc::new(client::Config::default()), client_io, handler)
            .await
            .unwrap();
    assert!(
        handle
            .authenticate_password("user", "test")
            .await
            .unwrap()
            .success()
    );
    let session = SshSession {
        handle,
        _socket: None,
        endpoint: journal::Endpoint {
            host: "fixture".into(),
            port: 22,
            username: "user".into(),
            fingerprint: fingerprint.clone(),
        },
        fingerprint,
    };
    assert_eq!(
        session.loopback_traffic_clear(1, "secret").await.unwrap(),
        b"{}"
    );
    assert!(session.loopback_traffic_clear(2, "secret").await.is_err());
    let started = std::time::Instant::now();
    assert!(
        session
            .loopback_http_remote_with_budget(
                "GET",
                3,
                "/traffic?clear=1",
                "127.0.0.1",
                Some("secret"),
                None,
                Duration::from_millis(100),
                true
            )
            .await
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let observed = requests.lock().unwrap();
    assert_eq!(observed.len(), 3);
    for request in observed.iter() {
        assert!(request.starts_with("GET /traffic?clear=1 HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: secret\r\n"));
    }
    server.abort();
}
