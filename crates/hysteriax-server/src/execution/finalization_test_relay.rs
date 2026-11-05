//! Real PostgreSQL COMMIT response gating; only used by finalization tests.
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};

#[derive(Default)]
pub(super) struct Gate {
    armed: AtomicBool,
    pub seen: Notify,
    pub release: Notify,
    drop_ack: AtomicBool,
}
impl Gate {
    pub fn arm(&self, drop_ack: bool) {
        self.drop_ack.store(drop_ack, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }
}
pub(super) struct CommitRelay {
    pub pool: PgPool,
    pub gate: Arc<Gate>,
    task: tokio::task::JoinHandle<()>,
}
impl CommitRelay {
    pub async fn new(direct: &PgPool) -> Self {
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(direct)
            .await
            .unwrap();
        let options = (*direct.connect_options()).clone();
        let target = (options.get_host().to_owned(), options.get_port());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let gate = Arc::new(Gate::default());
        let shared = gate.clone();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                let (downstream, _) = listener.accept().await.unwrap();
                let upstream = tokio::net::TcpStream::connect(target.clone())
                    .await
                    .unwrap();
                downstream.set_nodelay(true).unwrap();
                upstream.set_nodelay(true).unwrap();
                let gate = shared.clone();
                children.spawn(async move {
                    let (mut dr, mut dw) = downstream.into_split();
                    let (mut ur, mut uw) = upstream.into_split();
                    let send = tokio::io::copy(&mut dr, &mut uw);
                    let receive = async {
                        loop {
                            let mut header = [0_u8; 5];
                            ur.read_exact(&mut header).await?;
                            let length =
                                u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
                            let mut body = vec![0; length - 4];
                            ur.read_exact(&mut body).await?;
                            if header[0] == b'C'
                                && body == b"COMMIT\0"
                                && gate.armed.swap(false, Ordering::SeqCst)
                            {
                                gate.seen.notify_one();
                                if gate.drop_ack.load(Ordering::SeqCst) {
                                    return Ok::<(), std::io::Error>(());
                                }
                                gate.release.notified().await;
                            }
                            dw.write_all(&header).await?;
                            dw.write_all(&body).await?;
                        }
                    };
                    tokio::select! {_=send=>{},_=receive=>{}}
                });
                while children.try_join_next().is_some() {}
            }
        });
        let pool = crate::db::pool_options()
            .max_connections(4)
            .after_connect(move |c, _| {
                let schema = schema.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
                        .execute(&mut *c)
                        .await?;
                    sqlx::query("SET lock_timeout='1s'").execute(c).await?;
                    Ok(())
                })
            })
            .connect_with(
                options
                    .host("127.0.0.1")
                    .port(port)
                    .ssl_mode(sqlx::postgres::PgSslMode::Disable),
            )
            .await
            .unwrap();
        Self { pool, gate, task }
    }
}
impl Drop for CommitRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
