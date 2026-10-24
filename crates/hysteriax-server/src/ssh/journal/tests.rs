use super::*;
use serde_json::{Value, json};
fn step() -> RemoteStep {
    RemoteStep {
        operation_id: uuid::Uuid::new_v4().to_string(),
        resource: "node:fixture".into(),
        owner: "owner".into(),
        generation: 1,
        epoch: uuid::Uuid::new_v4().to_string(),
        work: "work".into(),
        step: "ssh-command:0".into(),
        kind: "ssh-command".into(),
        request_digest: crate::security::token_digest("fixture-command"),
        predecessor: Some("empty".into()),
    }
}
fn receipt(step: &RemoteStep) -> Value {
    json!({"protocol":2,"context":step,"state":"completed","exit_code":0,"stdout_bytes":0,"stderr_bytes":0,"stdout_sha256":hash(b""),"stderr_sha256":hash(b"")})
}
#[test]
fn receipt_requires_exact_incarnation_protocol_and_bounded_complete_output() {
    let s = step();
    let good = receipt(&s);
    assert!(parse(&serde_json::to_vec(&good).unwrap(), &s, 128).is_ok());
    for (pointer, value) in [
        ("/protocol", json!(3)),
        ("/state", json!("running")),
        ("/exit_code", json!(256)),
        ("/stdout_bytes", json!(129)),
        ("/stderr_bytes", json!(u64::MAX)),
        ("/stdout_sha256", json!("invalid")),
        ("/context/epoch", json!(uuid::Uuid::new_v4().to_string())),
        ("/context/generation", json!(2)),
        ("/context/owner", json!("other")),
        ("/context/work", json!("other")),
        (
            "/context/operation_id",
            json!(uuid::Uuid::new_v4().to_string()),
        ),
        ("/context/request_digest", json!("other")),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(
            parse(&serde_json::to_vec(&bad).unwrap(), &s, 128).is_err(),
            "{pointer}"
        );
    }
    let mut bad = good.clone();
    bad["extra"] = json!(true);
    assert!(parse(&serde_json::to_vec(&bad).unwrap(), &s, 128).is_err());
    assert!(parse(&vec![b' '; RECEIPT_LIMIT + 1], &s, 128).is_err());
    assert!(parse(b"{}", &s, 128).is_err());
}
#[test]
fn render_rejects_invalid_bindings_and_never_embeds_commands_in_persisted_intent() {
    let mut s = step();
    s.request_digest = crate::security::token_digest("echo 'private'");
    assert!(
        wrap(&s, "echo 'private'", 128)
            .unwrap()
            .contains("command='echo '\\''private'\\'''")
    );
    assert!(!serde_json::to_string(&s).unwrap().contains("private"));
    assert!(wrap(&s, "invalid\0command", 128).is_err());
    assert!(wrap(&s, "true", 0).is_err());
    s.operation_id = "../../unsafe".into();
    assert!(inspect(&s).is_err());
    s = step();
    s.generation = 0;
    assert!(wrap(&s, "true", 128).is_err());
    s = step();
    s.epoch = "../../unsafe".into();
    assert!(inspect(&s).is_err());
}

#[test]
fn predecessor_requires_an_exact_opaque_head_and_is_part_of_the_completion_context() {
    assert_eq!(parse_head("empty\n").unwrap(), "empty");
    let id = uuid::Uuid::new_v4().to_string();
    assert_eq!(parse_head(&format!("{id}\n")).unwrap(), id);
    for bad in ["", "../foreign", " empty", "empty\r\n", "not-a-uuid"] {
        assert!(parse_head(bad).is_err());
    }
    let mut s = step();
    let good = receipt(&s);
    s.predecessor = Some(uuid::Uuid::new_v4().to_string());
    assert!(parse(&serde_json::to_vec(&good).unwrap(), &s, 128).is_err());
    s.predecessor = None;
    assert!(inspect(&s).is_err());
}

pub(crate) struct Linux {
    pub(crate) container: String,
    pub(crate) home: String,
}
impl Linux {
    fn script(&self, step: &mut RemoteStep, command: &str, cap: usize) -> String {
        let output = self.run(&head(&step.resource).unwrap());
        assert!(output.status.success(), "remote predecessor unavailable");
        step.predecessor = Some(parse_head(std::str::from_utf8(&output.stdout).unwrap()).unwrap());
        step.request_digest = crate::security::token_digest(command);
        wrap(step, command, cap).unwrap()
    }

    fn command(&self, script: &str) -> std::process::Command {
        let mut c = std::process::Command::new("docker");
        c.args([
            "exec",
            "-e",
            &format!("HOME={}", self.home),
            &self.container,
            "sh",
            "-c",
            script,
        ]);
        c
    }
    pub(crate) fn run(&self, script: &str) -> std::process::Output {
        self.command(script).output().unwrap()
    }
    fn read(&self, s: &RemoteStep, cap: usize) -> Result<Receipt> {
        let output = self.run(&inspect(s)?);
        ensure!(output.status.success(), "remote receipt unavailable");
        parse(&output.stdout, s, cap)
    }
}
impl Drop for Linux {
    fn drop(&mut self) {
        let _ = self.run(&format!("rm -rf -- {}", quote(&self.home)));
    }
}
#[test]
#[ignore = "requires an owned Linux Docker fixture via TEST_SSH_JOURNAL_CONTAINER"]
fn linux_journal_preserves_outputs_fences_replays_and_keeps_unknown_barriers() {
    let linux = Linux {
        container: std::env::var("TEST_SSH_JOURNAL_CONTAINER").expect("owned fixture container"),
        home: format!("/tmp/hx-0034-journal-{}", uuid::Uuid::new_v4()),
    };
    assert!(
        linux
            .run(&format!(
                "mkdir -m 700 {}; command -v flock; command -v sha256sum",
                quote(&linux.home)
            ))
            .status
            .success()
    );
    let mut first = step();
    let output =
        linux.run(&linux.script(&mut first, "printf hello; printf warning >&2; exit 7", 128));
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"hello");
    assert_eq!(output.stderr, b"warning");
    let saved = linux.read(&first, 128).unwrap();
    assert_eq!(saved.exit_code, 7);
    assert_eq!(saved.stdout_bytes, 5);
    assert_eq!(saved.stderr_bytes, 7);
    assert_eq!(saved.stdout_sha256, hash(b"hello"));
    assert_eq!(saved.stderr_sha256, hash(b"warning"));
    let directory = format!(
        "{}/.hysteriax-executions-v2/{}/ops/{}",
        linux.home,
        hash(first.resource.as_bytes()),
        first.operation_id
    );
    let files = linux.run(&format!("ls -A {}", quote(&directory)));
    assert_eq!(
        String::from_utf8(files.stdout).unwrap(),
        "intent.json\nresult.json\n"
    );
    let repeated =
        linux.run(&wrap(&first, "printf hello; printf warning >&2; exit 7", 128).unwrap());
    assert_eq!(repeated.status.code(), Some(125));
    assert!(repeated.stdout.is_empty() && repeated.stderr.is_empty());
    let mut next = first.clone();
    next.operation_id = uuid::Uuid::new_v4().to_string();
    next.step = "ssh-command:1".into();
    next.generation = 2;
    assert!(
        linux
            .run(&linux.script(&mut next, "printf next", 128))
            .status
            .success()
    );
    assert!(linux.read(&first, 128).is_err());
    assert!(linux.read(&next, 128).is_ok());
    let mut stale = first.clone();
    stale.operation_id = uuid::Uuid::new_v4().to_string();
    assert!(
        !linux
            .run(&linux.script(&mut stale, "touch \"$HOME/stale\"", 128))
            .status
            .success()
    );
    let mut collision = next.clone();
    collision.operation_id = uuid::Uuid::new_v4().to_string();
    collision.owner = "foreign".into();
    assert!(
        !linux
            .run(&linux.script(&mut collision, "touch \"$HOME/collision\"", 128))
            .status
            .success()
    );
    assert!(!linux.run("test -e \"$HOME/collision\"").status.success());
    let mut restored = next.clone();
    restored.operation_id = uuid::Uuid::new_v4().to_string();
    restored.epoch = uuid::Uuid::new_v4().to_string();
    restored.generation = 1;
    assert!(
        linux
            .run(&linux.script(&mut restored, "printf restored", 128))
            .status
            .success()
    );
    stale.generation = 999;
    stale.operation_id = uuid::Uuid::new_v4().to_string();
    assert!(
        !linux
            .run(&linux.script(&mut stale, "touch \"$HOME/stale\"", 128))
            .status
            .success()
    );
    assert!(!linux.run("test -e \"$HOME/stale\"").status.success());
    // A queued E2 never reached this host before restore; E3 must still fence it.
    let mut queued = step();
    queued.resource = restored.resource.clone();
    queued.epoch = uuid::Uuid::new_v4().to_string();
    let delayed = linux.script(&mut queued, "touch \"$HOME/unseen-old-epoch\"", 128);
    let mut newer = queued.clone();
    newer.operation_id = uuid::Uuid::new_v4().to_string();
    newer.epoch = uuid::Uuid::new_v4().to_string();
    assert!(
        linux
            .run(&linux.script(&mut newer, "printf newest", 128))
            .status
            .success()
    );
    assert_eq!(linux.run(&delayed).status.code(), Some(125));
    assert!(
        !linux
            .run("test -e \"$HOME/unseen-old-epoch\"")
            .status
            .success()
    );
    assert!(linux.read(&newer, 128).is_ok());
    restored = newer;
    // Bounded output leaves a real incomplete active operation after lock release.
    let mut overflow = restored.clone();
    overflow.operation_id = uuid::Uuid::new_v4().to_string();
    let flooded = linux.run(&linux.script(&mut overflow, "head -c 1000 /dev/zero", 128));
    assert!(!flooded.status.success());
    assert!(flooded.stdout.len() > 128);
    assert!(flooded.stdout.len() <= 129);
    assert!(linux.read(&overflow, 128).is_err());
    let mut after = overflow.clone();
    after.operation_id = uuid::Uuid::new_v4().to_string();
    after.generation += 1;
    assert!(
        !linux
            .run(&linux.script(&mut after, "touch \"$HOME/unsafe-takeover\"", 128))
            .status
            .success()
    );
    assert!(
        !linux
            .run("test -e \"$HOME/unsafe-takeover\"")
            .status
            .success()
    );
    // Another resource is independent; there is no host-wide execution lock.
    let mut other = step();
    other.resource = "node:independent".into();
    assert!(
        linux
            .run(&linux.script(&mut other, "printf independent", 128))
            .status
            .success()
    );
    // Large installed files must not be limited by the bounded stdout capture.
    other.operation_id = uuid::Uuid::new_v4().to_string();
    assert!(
        linux
            .run(&linux.script(
                &mut other,
                "head -c 1000000 /dev/zero > \"$HOME/binary\"; printf installed",
                128
            ))
            .status
            .success()
    );
    assert_eq!(linux.read(&other, 128).unwrap().stdout_bytes, 9);
    let mut legacy = step();
    legacy.resource = "node:legacy-v1".into();
    legacy.request_digest = crate::security::token_digest("touch \"$HOME/legacy-bypass\"");
    let legacy_dir = format!(
        "{}/.hysteriax-executions-v1/{}",
        linux.home,
        hash(legacy.resource.as_bytes())
    );
    assert!(
        linux
            .run(&format!("mkdir -p {}", quote(&legacy_dir)))
            .status
            .success()
    );
    assert!(!linux.run(&head(&legacy.resource).unwrap()).status.success());
    assert_eq!(
        linux
            .run(&wrap(&legacy, "touch \"$HOME/legacy-bypass\"", 128).unwrap())
            .status
            .code(),
        Some(125)
    );
    assert!(
        !linux
            .run("test -e \"$HOME/legacy-bypass\"")
            .status
            .success()
    );
    // A live resource lock cannot be mistaken for a completed request.
    let mut live = step();
    live.resource = "node:live".into();
    let mut child = linux
        .command(&linux.script(
            &mut live,
            "printf ready > \"$HOME/ready\"; sleep 2; printf done",
            128,
        ))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !linux.run("test -e \"$HOME/ready\"").status.success() {
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(linux.read(&live, 128).is_err());
    assert!(child.wait().unwrap().success());
    assert!(linux.read(&live, 128).is_ok());
}

struct JournalServer {
    container: String,
    home: String,
}
pub(crate) fn fixture_handler(
    container: String,
    home: String,
) -> impl russh::server::Handler<Error = russh::Error> + Send {
    JournalServer { container, home }
}
impl russh::server::Handler for JournalServer {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        _: &str,
    ) -> std::result::Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut russh::server::Session,
    ) -> std::result::Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_success(channel)?;
        let command = String::from_utf8(data.to_vec()).unwrap();
        let (container, home, handle) =
            (self.container.clone(), self.home.clone(), session.handle());
        tokio::spawn(async move {
            let mut drop_ack = command.contains("HX_DROP_ACK_AFTER_EXECUTION");
            if command.contains("check_id=") {
                drop_ack |= tokio::process::Command::new("docker")
                    .args([
                        "exec",
                        "-e",
                        &format!("HOME={home}"),
                        &container,
                        "sh",
                        "-c",
                        "test -f \"$HOME/drop-recheck-ack\" && rm -- \"$HOME/drop-recheck-ack\"",
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success();
            }
            let output = tokio::process::Command::new("docker")
                .args([
                    "exec",
                    "-e",
                    &format!("HOME={home}"),
                    &container,
                    "sh",
                    "-c",
                    &command,
                ])
                .output()
                .await
                .unwrap();
            if !output.stdout.is_empty() {
                let _ = handle.data(channel, output.stdout).await;
            }
            if !output.stderr.is_empty() {
                let _ = handle.extended_data(channel, 1, output.stderr).await;
            }
            if !drop_ack {
                let _ = handle
                    .exit_status_request(channel, output.status.code().unwrap_or(255) as u32)
                    .await;
            }
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires an owned Linux Docker fixture via TEST_SSH_JOURNAL_CONTAINER"]
async fn actual_ssh_response_loss_keeps_intent_and_exact_remote_receipt_without_replaying() {
    use crate::{
        execution,
        ssh::{HostKeyHandler, SshSession},
    };
    use sqlx::Row;
    use std::sync::{Arc, Mutex};
    let linux = Linux {
        container: std::env::var("TEST_SSH_JOURNAL_CONTAINER").unwrap(),
        home: format!("/tmp/hx-0034-journal-{}", uuid::Uuid::new_v4()),
    };
    assert!(
        linux
            .run(&format!("mkdir -m 700 {}", quote(&linux.home)))
            .status
            .success()
    );
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[17; 32]).into(),
        "test",
    )
    .unwrap();
    let fingerprint = key
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        ..Default::default()
    });
    let (client_io, server_io) = tokio::io::duplex(65536);
    let handler = JournalServer {
        container: linux.container.clone(),
        home: linux.home.clone(),
    };
    let server = tokio::spawn(async move {
        russh::server::run_stream(config, server_io, handler)
            .await
            .unwrap()
            .await
    });
    let mut handle = russh::client::connect_stream(
        Arc::new(russh::client::Config::default()),
        client_io,
        HostKeyHandler {
            expected: Some(fingerprint.clone()),
            observed: Arc::new(Mutex::new(None)),
        },
    )
    .await
    .unwrap();
    assert!(
        handle
            .authenticate_password("fixture", "fixture")
            .await
            .unwrap()
            .success()
    );
    let endpoint = Endpoint {
        host: "journal-fixture".into(),
        port: 22,
        username: "fixture".into(),
        fingerprint: fingerprint.clone(),
    };
    let session = SshSession {
        handle,
        fingerprint,
        endpoint: endpoint.clone(),
    };
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let mut tx = pool.begin().await.unwrap();
    let lease = execution::acquire(&mut tx, "node:lost-ssh", "owner", "lost-work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let command = "printf applied >> \"$HOME/applied\"; # HX_DROP_ACK_AFTER_EXECUTION";
    let error = execution::supervise(pool.clone(), vec![lease.clone()], session.execute(command))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("completion was not acknowledged")
    );
    let row = sqlx::query("SELECT * FROM execution_operations WHERE resource_key='node:lost-ssh'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "dispatching");
    let intent: Value = row.get("intent");
    assert_eq!(
        intent["verification"]["endpoint"],
        serde_json::to_value(&endpoint).unwrap()
    );
    assert!(!intent.to_string().contains("applied"));
    let context = RemoteStep {
        operation_id: row.get("operation_id"),
        resource: row.get("resource_key"),
        owner: row.get("owner"),
        generation: row.get("generation"),
        epoch: row.get("epoch"),
        work: row.get("work_id"),
        step: row.get("step"),
        kind: intent["kind"].as_str().unwrap().into(),
        request_digest: intent["request_digest"].as_str().unwrap().into(),
        predecessor: intent["verification"]["expected_head"]
            .as_str()
            .map(str::to_owned),
    };
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key='node:lost-ssh'").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        execution::acquire(&mut tx, "node:lost-ssh", "next", "next-work")
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    assert!(
        execution::supervise(pool.clone(), vec![lease], session.execute(command))
            .await
            .is_none()
    );
    let evidence = session
        .inspect_command(&context, &endpoint, 4 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(evidence.exit_code, 0);
    assert_eq!(evidence.context, context);
    assert_eq!(linux.run("cat \"$HOME/applied\"").stdout, b"applied");
    let mut wrong = endpoint.clone();
    wrong.fingerprint = "foreign".into();
    assert!(
        session
            .inspect_command(&context, &wrong, 4 * 1024 * 1024)
            .await
            .is_err()
    );
    let mut wrong = context.clone();
    wrong.epoch = uuid::Uuid::new_v4().to_string();
    assert!(
        session
            .inspect_command(&wrong, &endpoint, 4 * 1024 * 1024)
            .await
            .is_err()
    );
    // An unrelated resource uses the complete command + receipt protocol and
    // commits the original acknowledgement normally over the same SSH handle.
    let mut tx = pool.begin().await.unwrap();
    let healthy = execution::acquire(&mut tx, "node:healthy-ssh", "owner", "healthy-work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let output = execution::supervise(
        pool.clone(),
        vec![healthy],
        session.execute("printf healthy"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.stdout, "healthy");
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM execution_operations WHERE work_id='healthy-work'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "verified"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM execution_operations WHERE work_id='lost-work'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "reconciling"
    );
    // A held remote predecessor must persist a DB barrier without constructing
    // or sending the requested payload, including after its lock is released.
    let mut busy = step();
    busy.resource = "node:busy-head".into();
    let held=linux.script(&mut busy,"printf ready > \"$HOME/busy-ready\"; while [ -d \"$HOME\" ] && [ ! -e \"$HOME/busy-release\" ]; do sleep 0.05; done; printf done",128);
    let mut child = linux
        .command(&held)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !linux.run("test -e \"$HOME/busy-ready\"").status.success() {
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut tx = pool.begin().await.unwrap();
    let blocked = execution::acquire(&mut tx, "node:busy-head", "owner", "busy-work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        execution::supervise(
            pool.clone(),
            vec![blocked],
            session.execute("touch \"$HOME/should-not-run\"")
        )
        .await
        .unwrap()
        .is_err()
    );
    let barrier =
        sqlx::query("SELECT state,intent FROM execution_operations WHERE work_id='busy-work'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(barrier.get::<String, _>("state"), "dispatching");
    assert_eq!(
        barrier.get::<Value, _>("intent")["verification"]["preparation"],
        "unverified"
    );
    assert!(
        !linux
            .run("test -e \"$HOME/should-not-run\"")
            .status
            .success()
    );
    assert!(linux.run("touch \"$HOME/busy-release\"").status.success());
    assert!(child.wait().unwrap().success());
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key='node:busy-head'").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        execution::acquire(&mut tx, "node:busy-head", "next", "next-busy")
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    server.abort();
}

#[test]
fn recheck_proof_binds_exact_original_receipt_target_and_nonce() {
    let original = step();
    let completion: Receipt = serde_json::from_value(receipt(&original)).unwrap();
    let request = recheck::Request {
        id: uuid::Uuid::new_v4().to_string(),
        original,
        receipt_sha256: recheck::digest(&completion).unwrap(),
        target_epoch: uuid::Uuid::new_v4().to_string(),
        target_generation: 2,
    };
    let proof = recheck::Proof {
        protocol: 2,
        request: request.clone(),
        completion,
        state: "fenced".into(),
    };
    let bytes = serde_json::to_vec(&proof).unwrap();
    assert_eq!(recheck::parse(&bytes, &request).unwrap(), proof);
    for (pointer, value) in [
        ("/protocol", json!(1)),
        ("/state", json!("prepared")),
        ("/request/id", json!(uuid::Uuid::new_v4().to_string())),
        ("/request/target_generation", json!(3)),
        ("/completion/exit_code", json!(1)),
        (
            "/completion/context/epoch",
            json!(uuid::Uuid::new_v4().to_string()),
        ),
    ] {
        let mut changed = serde_json::to_value(&proof).unwrap();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            recheck::parse(&serde_json::to_vec(&changed).unwrap(), &request).is_err(),
            "{pointer}"
        );
    }
    assert!(recheck::parse(&vec![b' '; RECEIPT_LIMIT + 1], &request).is_err());
    let mut invalid = request.clone();
    invalid.target_generation = 1;
    assert!(recheck::render(&invalid).is_err());
}

#[test]
#[ignore = "requires an owned Linux Docker fixture via TEST_SSH_JOURNAL_CONTAINER"]
fn linux_recheck_resumes_partial_fences_and_never_replays_payload_or_stale_authority() {
    for crash in ["none", "before_fence", "after_fence", "after_head"] {
        let linux = Linux {
            container: std::env::var("TEST_SSH_JOURNAL_CONTAINER").unwrap(),
            home: format!("/tmp/hx-0034-recheck-{}", uuid::Uuid::new_v4()),
        };
        assert!(linux.run("mkdir -m 700 \"$HOME\"").status.success());
        let mut original = step();
        assert!(
            linux
                .run(&linux.script(&mut original, "printf once >> \"$HOME/payload\"", 128))
                .status
                .success()
        );
        let completion = linux.read(&original, 128).unwrap();
        let resource = format!(
            "{}/.hysteriax-executions-v2/{}",
            linux.home,
            hash(original.resource.as_bytes())
        );
        let mut delayed = original.clone();
        delayed.operation_id = uuid::Uuid::new_v4().to_string();
        delayed.generation = 2;
        let delayed_script = linux.script(&mut delayed, "touch \"$HOME/stale\"", 128);
        let old_fence = linux.run(&format!("cat {}/fence", quote(&resource))).stdout;
        let request = recheck::Request {
            id: uuid::Uuid::new_v4().to_string(),
            original: original.clone(),
            receipt_sha256: recheck::digest(&completion).unwrap(),
            target_epoch: original.epoch.clone(),
            target_generation: 2,
        };
        let script = recheck::render(&request).unwrap();
        let output = linux.run(&script);
        assert!(
            output.status.success(),
            "{crash}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let proof = recheck::parse(&output.stdout, &request).unwrap();
        assert_eq!(proof.completion, completion);
        assert_eq!(
            parse_head(
                std::str::from_utf8(&linux.run(&head(&original.resource).unwrap()).stdout).unwrap()
            )
            .unwrap(),
            request.id
        );
        let directory = format!("{resource}/rechecks/{}", request.id);
        if crash != "none" {
            let mut reset = format!("rm -- {}/result.json", quote(&directory));
            if crash == "before_fence" || crash == "after_fence" {
                reset.push_str(&format!(
                    "\nprintf '%s\\n' {} > {}/head",
                    quote(&original.operation_id),
                    quote(&resource)
                ));
            }
            if crash == "before_fence" {
                reset.push_str(&format!(
                    "\nprintf '%s' {} > {}/fence",
                    quote(std::str::from_utf8(&old_fence).unwrap()),
                    quote(&resource)
                ));
            }
            assert!(linux.run(&reset).status.success());
            let resumed = linux.run(&script);
            assert!(resumed.status.success(), "{crash}");
            assert_eq!(recheck::parse(&resumed.stdout, &request).unwrap(), proof);
        }
        assert_eq!(linux.run(&delayed_script).status.code(), Some(125));
        assert!(!linux.run("test -e \"$HOME/stale\"").status.success());
        let mut wrong = request.clone();
        wrong.id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            linux.run(&recheck::render(&wrong).unwrap()).status.code(),
            Some(125)
        );
        let mut next = original.clone();
        next.operation_id = uuid::Uuid::new_v4().to_string();
        next.generation = 3;
        assert!(
            linux
                .run(&linux.script(&mut next, "printf healthy", 128))
                .status
                .success()
        );
        // Completed proof remains readable after a legitimate later operation has
        // garbage-collected the original receipt and advanced its own authority.
        let head_before = linux.run(&head(&original.resource).unwrap()).stdout;
        let repeated = linux.run(&script);
        assert!(repeated.status.success());
        assert_eq!(recheck::parse(&repeated.stdout, &request).unwrap(), proof);
        assert_eq!(
            linux.run(&head(&original.resource).unwrap()).stdout,
            head_before
        );
        assert_eq!(linux.run("cat \"$HOME/payload\"").stdout, b"once");
    }
}
