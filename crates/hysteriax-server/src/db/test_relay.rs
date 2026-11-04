//! PostgreSQL wire relay for lock/round-trip acceptance against real SQL.
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct SqlRelay {
    pub pool: PgPool,
    pub commands: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl SqlRelay {
    pub async fn new(direct: &PgPool, delay: Duration, connections: u32) -> Self {
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(direct)
            .await
            .unwrap();
        let options = (*direct.connect_options()).clone();
        let target = (options.get_host().to_owned(), options.get_port());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let commands = Arc::new(AtomicUsize::new(0));
        let counter = commands.clone();
        let task = tokio::spawn(async move {
            loop {
                let (downstream, _) = listener.accept().await.unwrap();
                let upstream = tokio::net::TcpStream::connect(target.clone())
                    .await
                    .unwrap();
                downstream.set_nodelay(true).unwrap();
                upstream.set_nodelay(true).unwrap();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let (mut dr, mut dw) = downstream.into_split();
                    let (mut ur, mut uw) = upstream.into_split();
                    let send = tokio::io::copy(&mut dr, &mut uw);
                    let receive = async {
                        let mut bytes = [0_u8; 65536];
                        let mut pending = Vec::new();
                        loop {
                            let count = ur.read(&mut bytes).await?;
                            if count == 0 {
                                return Ok::<(), std::io::Error>(());
                            }
                            pending.extend_from_slice(&bytes[..count]);
                            let mut consumed = 0;
                            while pending.len() - consumed >= 5 {
                                let length = u32::from_be_bytes(
                                    pending[consumed + 1..consumed + 5].try_into().unwrap(),
                                ) as usize;
                                if pending.len() - consumed < length + 1 {
                                    break;
                                }
                                if pending[consumed] == b'C' {
                                    counter.fetch_add(1, Ordering::SeqCst);
                                }
                                consumed += length + 1;
                            }
                            pending.drain(..consumed);
                            if !delay.is_zero() {
                                tokio::time::sleep(delay).await;
                            }
                            dw.write_all(&bytes[..count]).await?;
                        }
                    };
                    tokio::select! { _ = send => {}, _ = receive => {} }
                });
            }
        });
        let pool = crate::db::pool_options()
            .max_connections(connections)
            .after_connect(move |connection, _| {
                let schema = schema.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
                        .execute(connection)
                        .await?;
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
        // Finish connection setup and after_release before starting a sample.
        pool.acquire().await.unwrap().return_to_pool().await;
        Self {
            pool,
            commands,
            task,
        }
    }
}
impl Drop for SqlRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
