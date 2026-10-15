use super::*;
use russh::{Channel, ChannelId, server};

struct TestServer;
impl server::Handler for TestServer {
    type Error = russh::Error;
    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        if data == b"ok" {
            session.data(channel, b"hello".to_vec())?;
            session.exit_status_request(channel, 0)?;
            session.close(channel)?;
        } else {
            let handle = session.handle();
            tokio::spawn(async move {
                // Active output keeps an inactivity timeout from protecting us.
                loop {
                    if handle.data(channel, vec![b'x'; 512]).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
        }
        Ok(())
    }
}

#[tokio::test]
async fn commands_bound_active_output_and_execution_time() {
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]).into(),
        "test",
    )
    .unwrap();
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let config = Arc::new(server::Config {
        keys: vec![key],
        ..Default::default()
    });
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        server::run_stream(config, server_io, TestServer)
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
        fingerprint,
    };
    let output = session.execute("ok").await.unwrap();
    assert_eq!(output.stdout, "hello");
    assert_eq!(output.code, 0);
    let limit = session
        .execute_with_limits("flood", Duration::from_secs(2), 1024)
        .await
        .unwrap_err();
    assert!(matches!(
        limit.downcast_ref::<SshError>(),
        Some(SshError::OutputLimitExceeded)
    ));
    assert!(!crate::deployment::retryable(&limit));
    let timeout = session
        .execute_with_limits("flood", Duration::from_millis(80), 1024 * 1024)
        .await
        .unwrap_err();
    assert!(matches!(
        timeout.downcast_ref::<SshError>(),
        Some(SshError::OperationTimedOut)
    ));
    assert!(!crate::deployment::retryable(&timeout));
    assert_eq!(session.execute("ok").await.unwrap().stdout, "hello");
    server.abort();
}
