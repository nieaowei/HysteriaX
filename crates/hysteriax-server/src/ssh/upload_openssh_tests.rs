use super::*;
use crate::ssh::{self, FingerprintResult, SshNode};

#[tokio::test]
#[ignore = "requires an owned OpenSSH fixture and optional bandwidth-limited TCP forwarder"]
async fn real_openssh_upload_streams_a_release_sized_file_with_independent_deadline() {
    let node = SshNode {
        host: "127.0.0.1".into(),
        port: std::env::var("TEST_SSH_UPLOAD_PORT")
            .unwrap()
            .parse()
            .unwrap(),
        username: "root".into(),
        auth_type: "password".into(),
        secret: std::env::var("TEST_SSH_UPLOAD_SECRET")
            .unwrap_or_else(|_| "fixture-upload-only".into()),
        passphrase: None,
        host_fingerprint: Some(std::env::var("TEST_SSH_UPLOAD_FINGERPRINT").unwrap()),
    };
    let FingerprintResult::Trusted(session) = ssh::connect(&node).await.unwrap() else {
        panic!("fixture host identity must be pinned");
    };
    let size = std::env::var("TEST_SSH_UPLOAD_BYTES")
        .ok()
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(23_003_298);
    let payload: Vec<u8> = match std::env::var_os("TEST_SSH_UPLOAD_FILE") {
        Some(path) => std::fs::read(path).unwrap(),
        None => (0..size).map(|i| ((i * 31) % 251) as u8).collect(),
    };
    let path = format!("/tmp/hx-upload-openssh-{}", uuid::Uuid::new_v4());
    let expected = hex::encode(Sha256::digest(&payload));
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let mut tx = pool.begin().await.unwrap();
    let resource = format!("node:openssh-upload-{}", uuid::Uuid::new_v4());
    let lease = crate::execution::acquire(&mut tx, &resource, "fixture", "upload")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let transfer = async {
        if std::env::var_os("TEST_SSH_UPLOAD_REUSE").is_some() {
            session.upload_release_binary(&path, &payload).await
        } else {
            session.upload(&path, &payload).await
        }
    };
    crate::execution::supervise(pool, vec![lease], transfer)
        .await
        .unwrap()
        .unwrap();
    let actual = session
        .execute_read_only_checked(&format!("sha256sum {path}"))
        .await
        .unwrap();
    assert_eq!(actual.split_whitespace().next(), Some(expected.as_str()));
    session
        .execute_checked(&format!("rm -- {path}"))
        .await
        .unwrap();
}
