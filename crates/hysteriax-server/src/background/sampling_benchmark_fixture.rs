//! Loopback fixture: real SSH transport and real Hysteria QUIC forwarding.
//! Linux counters and user traffic are deterministic synthetic observations.
use super::*;
use russh::{Channel, ChannelMsg, server};
use std::{collections::HashMap, path::PathBuf};

#[derive(Clone)]
struct SshPeer {
    node: String,
    clears: Arc<Mutex<HashMap<String, usize>>>,
}
impl server::Handler for SshPeer {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> std::result::Result<server::Auth, Self::Error> {
        assert_eq!(password, "fixture");
        self.node = user.to_owned();
        tokio::time::sleep(Duration::from_millis(25)).await;
        Ok(server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        command: &[u8],
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        let command = String::from_utf8_lossy(command);
        let output = if command.contains("systemctl show hysteriax.service") {
            "stable-instance\n"
        } else if command.contains("/sys/class/net/") {
            "00000000-0000-4000-8000-000000000001 eth0 1000 500\n"
        } else {
            panic!("unexpected fixture command: {command}")
        };
        session.channel_success(channel)?;
        let handle = session.handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let _ = handle.data(channel, output.as_bytes().to_vec()).await;
            let _ = handle.exit_status_request(channel, 0).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        mut channel: Channel<server::Msg>,
        host: &str,
        _: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        assert_eq!(host, "127.0.0.1");
        reply.accept().await;
        let (node, clears, handle, id) = (
            self.node.clone(),
            self.clears.clone(),
            session.handle(),
            channel.id(),
        );
        tokio::spawn(async move {
            let Some(ChannelMsg::Data { data }) = channel.wait().await else {
                return;
            };
            let request = String::from_utf8_lossy(&data);
            let index: usize = node.strip_prefix("n-").unwrap().parse::<usize>().unwrap() - 1;
            let users: Vec<_> = (0..10)
                .flat_map(|k| {
                    (0..10).map(move |r| 1 + (index + 10000 - k * 1000) % 10000 + r * 10000)
                })
                .collect();
            let body = if request.starts_with("GET /traffic?clear=1 ") {
                *clears.lock().unwrap().entry(node).or_default() += 1;
                Value::Object(
                    users
                        .iter()
                        .map(|i| (format!("u-{i:08}"), json!({"tx":100,"rx":50})))
                        .collect(),
                )
                .to_string()
            } else {
                assert!(request.starts_with("GET /online "));
                Value::Object(
                    users
                        .iter()
                        .map(|i| (format!("u-{i:08}"), json!(1)))
                        .collect(),
                )
                .to_string()
            };
            tokio::time::sleep(Duration::from_millis(25)).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = handle.data(id, response.into_bytes()).await;
            let _ = handle.close(id).await;
        });
        Ok(())
    }
}

pub(super) struct Remote {
    pub secrets: crate::security::SecretBox,
    ssh_port: u16,
    fingerprint: String,
    proxy_port: u16,
    stats_port: u16,
    peer: tokio::task::JoinHandle<()>,
    auth: tokio::task::JoinHandle<()>,
    server: tokio::process::Child,
    directory: PathBuf,
}

fn tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Remote {
    pub async fn start(pool: PgPool, _active: usize) -> Self {
        let secrets =
            crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap();
        let key = russh::keys::PrivateKey::new(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[23; 32]).into(),
            "fixture",
        )
        .unwrap();
        let fingerprint = key
            .public_key()
            .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
            .to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ssh_port = listener.local_addr().unwrap().port();
        let counts = Arc::new(Mutex::new(HashMap::new()));
        let peer = tokio::spawn(async move {
            let mut sessions = JoinSet::new();
            loop {
                tokio::select! {
                    Some(_)=sessions.join_next(),if !sessions.is_empty()=>{},
                    incoming=listener.accept()=>{
                        let (stream,_)=incoming.unwrap(); stream.set_nodelay(true).unwrap();

                        let (key,clears)=(key.clone(),counts.clone());
                        sessions.spawn(async move {if let Ok(s)=server::run_stream(Arc::new(server::Config{keys:vec![key],..Default::default()}),stream,SshPeer{node:String::new(),clears}).await {let _=s.await;}});
                    }
                }
            }
        });
        async fn auth(
            axum::extract::State(pool): axum::extract::State<PgPool>,
            axum::Json(request): axum::Json<Value>,
        ) -> axum::Json<Value> {
            let hash = crate::security::token_digest(request["auth"].as_str().unwrap());
            let node:Option<String>=sqlx::query_scalar("SELECT node_id FROM monitoring_probe_tokens WHERE token_hash=$1 AND expires_at>clock_timestamp()")
                .bind(&hash).fetch_optional(&pool).await.unwrap();
            axum::Json(match node {
                Some(node) => json!({"ok":true,"id":crate::proxy_monitor::identity(&node,&hash)}),
                None => json!({"ok":false}),
            })
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let auth_port = listener.local_addr().unwrap().port();
        let auth = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new()
                    .route("/auth", axum::routing::post(auth))
                    .with_state(pool),
            )
            .await
            .unwrap();
        });
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let proxy_port = udp.local_addr().unwrap().port();
        drop(udp);
        let stats_port = tcp_port();
        let root = PathBuf::from(std::env::var("HX_SAMPLING_OUTPUT").unwrap());
        let directory = root.join(format!("remote-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::write(directory.join("cert.pem"), certified.cert.pem()).unwrap();
        std::fs::write(
            directory.join("key.pem"),
            certified.signing_key.serialize_pem(),
        )
        .unwrap();
        let config = json!({"listen":format!("127.0.0.1:{proxy_port}"),"tls":{"cert":directory.join("cert.pem"),"key":directory.join("key.pem")},
            "auth":{"type":"http","http":{"url":format!("http://127.0.0.1:{auth_port}/auth")}},"trafficStats":{"listen":format!("127.0.0.1:{stats_port}"),"secret":"stats"}});
        let config_path = directory.join("server.json");
        std::fs::write(&config_path, config.to_string()).unwrap();
        let log = std::fs::File::create(directory.join("server.log")).unwrap();
        let server = tokio::process::Command::new(std::env::var("HYSTERIAX_PROBE_BINARY").unwrap())
            .args(["--disable-update-check", "server", "-c"])
            .arg(config_path)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(r) = client
                    .get(format!("http://127.0.0.1:{stats_port}/online"))
                    .header("Authorization", "stats")
                    .send()
                    .await
                    && r.status().is_success()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("real Hysteria stats server did not start; inspect retained server.log");
        Self {
            secrets,
            ssh_port,
            fingerprint,
            proxy_port,
            stats_port,
            peer,
            auth,
            server,
            directory,
        }
    }

    pub async fn seed(&self, pool: &PgPool, active: usize) {
        let encrypted = self.secrets.encrypt(r#"{"secret":"fixture"}"#).unwrap();
        let config=self.secrets.encrypt(&json!({"public_host":"127.0.0.1","public_port":self.proxy_port,"tls_skip_verify":true,"tls_sni":"localhost",
            "traffic_stats_port":self.stats_port,"server_config":{}}).to_string()).unwrap();
        let stats = self.secrets.encrypt("stats").unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL session_replication_role=replica")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::raw_sql("INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('bench-ssh','SSH','ssh_password',now(),now()); INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('bench','Benchmark',now(),now()); INSERT INTO users(id,name,created_at,updated_at) SELECT 'u-'||lpad(i::text,8,'0'),'User '||i,now(),now() FROM generate_series(1,100000)i; INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||id,id,'connection',id,'fixture',now() FROM users;")
            .execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('bench-ssh',1,$1,now())")
            .bind(encrypted).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_host_fingerprint,public_host,public_port,listen_addr,tls_skip_verify,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,ssh_credential_id,ssh_credential_version,created_at,updated_at,deployed_revision,state,mtls_state,traffic_stats_port) SELECT 'n-'||lpad(i::text,8,'0'),'Node '||i,'127.0.0.1',$1,'n-'||lpad(i::text,8,'0'),$2,'127.0.0.1',$3,':443',true,'node-'||i,'fixture',$4,$5,'bench-ssh',1,now(),now(),CASE WHEN i<=$6 THEN 1 END,CASE WHEN i<=$6 THEN 'deployed' ELSE 'new' END,'ready',$7 FROM generate_series(1,10000)i")
            .bind(i32::from(self.ssh_port)).bind(&self.fingerprint).bind(i32::from(self.proxy_port)).bind(stats).bind(&config).bind(active as i32).bind(i32::from(self.stats_port)).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) SELECT 'config:'||id,id,1,$1,'fixture',true,now() FROM nodes")
            .bind(config).execute(&mut *tx).await.unwrap();
        sqlx::raw_sql("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT 'bench',id,now() FROM users; INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT 'bench',id,now() FROM nodes; INSERT INTO node_assignments(user_id,node_id,created_at) SELECT 'u-'||lpad(i::text,8,'0'),'n-'||lpad((1+((i-1)+k*1000)%10000)::text,8,'0'),now() FROM generate_series(1,100000)i CROSS JOIN generate_series(0,9)k; INSERT INTO node_packages(node_id,period_id,config,boot_id,interface,tx_total,rx_total) SELECT id,id,'{}','00000000-0000-4000-8000-000000000001','eth0',0,0 FROM nodes;")
            .execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        sqlx::raw_sql("ANALYZE users; ANALYZE nodes; ANALYZE node_assignments; ANALYZE node_packages; ANALYZE config_versions; ANALYZE user_credentials; ANALYZE authorization_group_users; ANALYZE authorization_group_nodes;")
            .execute(pool).await.unwrap();
        let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM users),(SELECT count(*) FROM nodes),(SELECT count(*) FROM node_assignments)").fetch_one(pool).await.unwrap();
        assert_eq!(counts, (100000, 10000, 1000000));
        println!(
            "SAMPLING_FIXTURE {}",
            json!({"directory":self.directory,"active_nodes":active,"counts":counts})
        );
    }
    pub async fn stop(mut self) {
        self.peer.abort();
        self.auth.abort();
        let _ = self.server.kill().await;
        let _ = self.server.wait().await;
        let _ = tokio::join!(self.peer, self.auth);
    }
}
